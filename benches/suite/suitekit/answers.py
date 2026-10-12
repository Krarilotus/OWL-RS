"""Answer capture (`suite.py run --answers full --answers-reference SYSTEM`): every system's
complete answer to every query, compared with one reference system's answer.

After a pair's timed queries, while its server still runs, each query is posted once more,
untimed, and its SPARQL JSON result kept under the pair's logs (one file per query, cache
mode and repetition). The server's serve-phase memory therefore includes this replay. At
the end of the run `answers.json`, beside results.csv, holds every capture with its SHA-256
and one verdict per query and system:

| Verdict | Meaning |
|---|---|
| `match` | the answer equals the reference's: `strict_match` (the same terms, as written) and `value_match` (the same values) are both recorded |
| `mismatch` | the values differ: a correctness finding for one of the two |
| `incomplete` | a capture failed (error, timeout, not JSON), or the reference has none |
| `unqualified` | the comparison can't decide: blank nodes (they need an isomorphism), an unstable reference (its captures differ among themselves), or a reference that contradicts an adjudicated count (adjudications.tsv) |

Each system is compared with the reference alone: one deviating system never disqualifies
the others. A value match takes numeric literals by their value within one XSD primitive
type (`"3"^^xsd:int` is the integer 3, as `"3"^^xsd:integer` is; `xsd:double` is apart),
plain literals as `xsd:string`, and language tags case-insensitively; where only the
strict comparison fails, the verdict names the datatypes that differ. The order compared:

| Query | Compared as |
|---|---|
| `ORDER BY` | a sequence (ties in the sort key are not yet told apart: such a difference shows as a sequence mismatch with equal bags, see `note`) |
| a final `LIMIT` without `ORDER BY` | a legal subset: as many rows as the limit allows, each from the reference's answer to the query without that `LIMIT` (captured from the reference too) |
| anything else | a bag |
"""
from __future__ import annotations

import hashlib
import json
import re
from collections import Counter
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from pathlib import Path

XSD = "http://www.w3.org/2001/XMLSchema#"
# Primitive XSD types and the types derived from them whose values compare across.
DECIMAL_TYPES = {XSD + t for t in (
    "decimal", "integer", "int", "long", "short", "byte", "nonNegativeInteger", "positiveInteger",
    "nonPositiveInteger", "negativeInteger", "unsignedLong", "unsignedInt", "unsignedShort", "unsignedByte")}
FLOAT_TYPES = {XSD + "double", XSD + "float"}

ORDERED = re.compile(r"\bORDER\s+BY\b", re.I)
FINAL_LIMIT = re.compile(r"\s+(?:OFFSET\s+\d+\s+)?LIMIT\s+(\d+)(?:\s+OFFSET\s+\d+)?\s*$", re.I)


def short(datatype: str | None) -> str:
    if not datatype:
        return "plain"
    return "xsd:" + datatype[len(XSD):] if datatype.startswith(XSD) else datatype


def value_key(term: dict) -> tuple:
    """A term by its value: what `value_match` compares."""
    kind = term.get("type")
    if kind in ("literal", "typed-literal"):
        datatype, text = term.get("datatype"), term.get("value", "")
        if "xml:lang" in term:
            return ("lang", text, term["xml:lang"].lower())
        try:
            if datatype in DECIMAL_TYPES:
                return ("decimal", Decimal(text.strip()))
            if datatype in FLOAT_TYPES:
                return ("double", float(text.strip()))
        except (InvalidOperation, ValueError):
            pass  # an ill-typed literal compares as written
        if datatype in (None, XSD + "string"):
            return ("string", text)
        if datatype == XSD + "boolean" and text.strip() in ("true", "false", "1", "0"):
            return ("boolean", text.strip() in ("true", "1"))
        return ("literal", text, datatype)
    return (kind, term.get("value"))


def strict_key(term: dict) -> str:
    return json.dumps(term, sort_keys=True, separators=(",", ":"))


@dataclass
class Answer:
    variables: tuple[str, ...] = ()
    rows: list[dict] = field(default_factory=list)
    boolean: bool | None = None

    @property
    def blank(self) -> bool:
        return any(t.get("type") == "bnode" for row in self.rows for t in row.values())

    def keys(self, key) -> list[tuple]:
        return [tuple(sorted((v, key(t)) for v, t in row.items())) for row in self.rows]


def parse(body: str) -> Answer:
    """A SPARQL JSON result; ValueError if it isn't one."""
    try:
        result = json.loads(body)
        if "boolean" in result:
            if not isinstance(result["boolean"], bool):
                raise ValueError("ASK result is not a JSON boolean")
            return Answer(boolean=result["boolean"])
        return Answer(tuple(sorted(result["head"].get("vars", []))), list(result["results"]["bindings"]))
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError(f"not a SPARQL JSON result: {error}") from error


def order_of(query: str) -> tuple[str, int | None]:
    """How a query's answers compare: ("sequence"|"limit"|"bag", the final LIMIT)."""
    if ORDERED.search(query):
        return "sequence", None
    limit = FINAL_LIMIT.search(query.strip())
    return ("limit", int(limit.group(1))) if limit else ("bag", None)


def without_limit(query: str) -> str:
    return FINAL_LIMIT.sub("", query.strip()) + "\n"


def datatypes(answer: Answer) -> dict[str, Counter]:
    out: dict[str, Counter] = {}
    for row in answer.rows:
        for v, t in row.items():
            if t.get("type") in ("literal", "typed-literal"):
                out.setdefault(v, Counter())[short(t.get("datatype"))] += 1
    return out


def agree(answer: Answer, reference: Answer, order: str, limit: int | None, full: Answer | None,
          key) -> bool:
    if (answer.boolean is None) != (reference.boolean is None):
        return False
    if answer.boolean is not None:
        return answer.boolean == reference.boolean
    if answer.variables != reference.variables:
        return False
    if order == "sequence":
        return answer.keys(key) == reference.keys(key)
    if order == "limit":
        pool = Counter(full.keys(key))
        got = Counter(answer.keys(key))
        return sum(got.values()) == min(limit, sum(pool.values())) and not (got - pool)
    return Counter(answer.keys(key)) == Counter(reference.keys(key))


def compare(answer: Answer, reference: Answer, order: str = "bag", limit: int | None = None,
            full: Answer | None = None) -> dict:
    """One answer against the reference's: verdict, strict_match, value_match, note."""
    if answer.blank or reference.blank:
        return dict(verdict="unqualified", strict_match=None, value_match=None,
                    note="blank nodes: the answers need an isomorphism check")
    if order == "limit" and full is None:
        return dict(verdict="incomplete", strict_match=None, value_match=None,
                    note="no reference answer without the LIMIT")
    strict = agree(answer, reference, order, limit, full, strict_key)
    value = strict or agree(answer, reference, order, limit, full, value_key)
    note = ""
    if value and not strict:
        mine, theirs = datatypes(answer), datatypes(reference)
        differing = [f"?{v} {', '.join(sorted(mine.get(v, {})))} for {', '.join(sorted(theirs.get(v, {})))}"
                     for v in sorted(set(mine) | set(theirs)) if set(mine.get(v, {})) != set(theirs.get(v, {}))]
        note = "datatypes differ: " + "; ".join(differing) if differing else "terms differ as written"
    elif not value and order == "sequence" and Counter(answer.keys(value_key)) == Counter(reference.keys(value_key)):
        note = "same rows in another order (ties in the sort key are not told apart)"
    return dict(verdict="match" if value else "mismatch", strict_match=strict, value_match=value, note=note)


@dataclass
class Capture:
    item: tuple[str, str, str, str]  # workload, tier, regime, query file
    system: str
    cache: str
    run: int
    path: str
    sha256: str | None = None
    answer: Answer | None = None
    error: str = ""


class Recorder:
    """Captures answers during a run and writes answers.json at its end."""

    def __init__(self, directory: Path, reference: str, adjudicated: dict[tuple, str] | None = None):
        self.directory, self.reference = directory, reference
        self.adjudicated = adjudicated or {}
        self.captures: list[Capture] = []
        self.unlimited: dict[tuple, Answer] = {}
        self.queries: dict[tuple, str] = {}
        self.orders: dict[tuple, tuple[str, int | None]] = {}
        self.expected: set[tuple[tuple, str]] = set()

    def expect(self, item: tuple, system: str):
        """A capture that should come: without one, the system's verdict is incomplete."""
        self.expected.add((item, system))

    def capture(self, post, url: str, item: tuple, system: str, cache: str, run: int, query: Path,
                logs: Path, timeout: float):
        text = query.read_text(encoding="utf-8")
        self.queries[item] = hashlib.sha256(text.encode()).hexdigest()
        path = logs / f"{query.stem}-answers-{cache}.json"
        capture = Capture(item, system, cache, run, str(path))
        try:
            code, body = post(url, {"query": text}, timeout=timeout)
            path.write_text(body, encoding="utf-8")
            capture.sha256 = hashlib.sha256(body.encode()).hexdigest()
            if code != 200:
                raise ValueError(f"HTTP {code}")
            capture.answer = parse(body)
        except (ValueError, OSError) as error:
            capture.error = f"{type(error).__name__}: {error}"[:300]
        self.captures.append(capture)
        self.orders[item] = order_of(text)
        if system == self.reference and self.orders[item][0] == "limit" and item not in self.unlimited:
            code, body = post(url, {"query": without_limit(text)}, timeout=timeout)
            (logs / f"{query.stem}-answers-{cache}-without-limit.json").write_text(body, encoding="utf-8")
            try:
                if code == 200:
                    self.unlimited[item] = parse(body)
            except ValueError:
                pass

    def reference_of(self, item: tuple) -> tuple[Answer | None, str]:
        """The reference's answer for an item, or None and why."""
        own = [c for c in self.captures if c.item == item and c.system == self.reference]
        if not own or any(c.answer is None for c in own):
            return None, f"no complete answer from the reference {self.reference}"
        first = own[0].answer
        if first.blank:
            return None, "blank nodes: the answers need an isomorphism check"
        order, limit = self.orders.get(item, ("bag", None))
        full = self.unlimited.get(item, first)
        if any(not compare(c.answer, first, order, limit, full)["strict_match"] for c in own[1:]):
            return None, "the reference's captures differ among themselves"
        count = self.adjudicated.get((item[0], item[1], Path(item[3]).stem))
        if count is not None and first.boolean is None and str(len(first.rows)) != str(count):
            return None, f"the reference's {len(first.rows)} rows contradict the adjudicated {count}"
        return first, ""

    def verdicts(self) -> list[dict]:
        out = []
        for item in sorted({c.item for c in self.captures} | {i for i, _ in self.expected}):
            reference, why = self.reference_of(item)
            order, limit = self.orders.get(item, ("bag", None))
            systems = {c.system for c in self.captures if c.item == item} | {s for i, s in self.expected if i == item}
            for system in sorted(systems):
                own = [c for c in self.captures if c.item == item and c.system == system]
                if not own:
                    entry = dict(verdict="incomplete", strict_match=None, value_match=None, note="not captured")
                elif any(c.answer is None for c in own):
                    entry = dict(verdict="incomplete", strict_match=None, value_match=None,
                                 note="; ".join(c.error for c in own if c.error))
                elif reference is None:
                    entry = dict(verdict="incomplete" if why.startswith("no complete") else "unqualified",
                                 strict_match=None, value_match=None, note=why)
                else:
                    each = [compare(c.answer, reference, order, limit, self.unlimited.get(item)) for c in own]
                    bad = [e for e in each if e["verdict"] != "match"]
                    entry = bad[0] if bad else dict(verdict="match", value_match=True,
                                                    strict_match=all(e["strict_match"] for e in each),
                                                    note=next((e["note"] for e in each if e["note"]), ""))
                out.append(dict(workload=item[0], tier=item[1], regime=item[2], query=item[3],
                                query_sha256=self.queries.get(item), order=order, system=system,
                                reference=self.reference, captures=len(own), **entry))
        return out

    def write(self) -> Path:
        verdicts = self.verdicts()
        path = self.directory / "answers.json"
        path.write_text(json.dumps(dict(
            reference=self.reference,
            captures=[dict(item=list(c.item), system=c.system, cache=c.cache, run=c.run, path=c.path,
                           sha256=c.sha256, error=c.error or None) for c in self.captures],
            verdicts=verdicts,
            summary=dict(Counter(v["verdict"] for v in verdicts if v["system"] != self.reference)),
            caveat="Untimed: one full answer per query, cache mode and fresh-store repetition, captured after "
                   "the timed queries while the server ran; serve-phase memory includes it."), indent=1) + "\n",
            encoding="utf-8")
        return path
