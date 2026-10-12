"""Answer capture (`suite.py run --answers full --answers-reference SYSTEM[,SYSTEM...]`): every
system's complete answer to every query, compared with an adjudicated reference's answer;
and `suite.py answers DIR...`, which classifies the captures of one or more runs again (after
an adjudication, or over several runs at once) without running anything.

After a pair's timed queries, while its server still runs, each query is posted once more,
untimed, and its SPARQL JSON result kept under the pair's logs (one file per query, cache
mode and repetition). The server's serve-phase memory therefore includes this replay. At
the end of the run `answers.json`, beside results.csv, holds every capture with its SHA-256,
one verdict per query and system, and one per group (a distinct query on the same data).

**The reference.** `--answers-reference` names one system or an ordered list. Per query the
reference is the first listed system whose answer is complete (every capture answered),
stable (its captures agree among themselves), free of blank nodes, and consistent with an
adjudicated count (`adjudications.tsv`); the next such system confirms it. When the two
disagree by value, which of them is wrong is a question for adjudication, not a vote: the
group is `unqualified` until a line in adjudications.tsv settles it. The reference itself
is `ok` when confirmed (by the second reference or an adjudicated count), else `reference`.

**Verdicts**, per system and query:

| Verdict | Meaning |
|---|---|
| `ok` | the answer equals the reference's by value (`value_match`); `strict_match` says whether the terms are also the same as written, and `note` names the datatypes that differ |
| `wrong` | the values differ: the system's answer is wrong (the reference is checked as above) |
| `incomplete` | no answer: an error, a timeout, not JSON, not captured; or no reference answered |
| `unsupported` | the system can't run the query (the suite skipped the pair, or the system says so) |
| `unqualified` | the comparison can't decide: blank nodes (they need an isomorphism), or the references disagree |
| `reference` | the reference's own answer, not confirmed by a second reference or a count |

A group's verdict is its worst system's, in that order of precedence: `wrong` (naming
whose), `unqualified`, `incomplete`, `unsupported`, then `ok`. One deviating system never
disqualifies the others.

A value match takes numeric literals by their value within one XSD primitive type
(`"3"^^xsd:int` is the integer 3, as `"3"^^xsd:integer` is; `xsd:double` is apart), plain
literals as `xsd:string`, and language tags case-insensitively. The order compared:

| Query | Compared as |
|---|---|
| `ORDER BY` | a sequence (ties in the sort key are not yet told apart: such a difference shows as a sequence mismatch with equal bags, see `note`) |
| a final `LIMIT` without `ORDER BY` | a legal subset: as many rows as the limit allows, each from the reference's answer to the query without that `LIMIT` (captured from every reference system too) |
| anything else | a bag |
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import time
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
UNSUPPORTED = re.compile(r"unsupported|not supported|not implemented", re.I)

# A group's verdict is its worst system's.
PRECEDENCE = ("wrong", "unqualified", "incomplete", "unsupported", "reference", "ok")


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
    """One answer against the reference's: verdict (ok, wrong, incomplete, unqualified),
    strict_match, value_match, note."""
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
    elif not value and answer.boolean is None and reference.boolean is None:
        note = f"{len(answer.rows)} rows for the reference's {len(reference.rows)}"
    return dict(verdict="ok" if value else "wrong", strict_match=strict, value_match=value, note=note)


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
    unlimited: bool = False  # the query without its final LIMIT (a reference's pool)


def capped_post(url: str, data: dict, timeout: float = 30) -> tuple[int, str]:
    """A query's answer as the recorder may hold it: streamed and read at most up to
    NRESE_ANSWER_CAP_MB (default 256 MiB) of JSON, else (-1, why) and the connection closed.
    An answer past the cap isn't compared here; holding it would let one unlimited answer
    (a LIMIT query's reference without its LIMIT: on DBLP every typed statement) exhaust the
    host's memory, as it did on 12 Oct 2026 (the queue service of phuoc-yu killed at 25.6 GB)."""
    import urllib.error
    import urllib.parse
    import urllib.request
    cap = int(float(os.environ.get("NRESE_ANSWER_CAP_MB", "256")) * 1048576)
    request = urllib.request.Request(url, data=urllib.parse.urlencode(data).encode(),
                                     headers={"Accept": "application/sparql-results+json"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            chunks, size = [], 0
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                size += len(chunk)
                if size > cap:
                    return -1, f"an answer past the recorder's cap of {cap // 1048576} MiB (NRESE_ANSWER_CAP_MB): not compared"
                chunks.append(chunk)
            return response.status, b"".join(chunks).decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read(65536).decode("utf-8", "replace")
    except (urllib.error.URLError, ConnectionError, TimeoutError, OSError):
        return 0, ""


def same_value(term: dict, known) -> bool:
    """A term against a known value: numerically when both are numbers, else as written."""
    text = term.get("value", "")
    try:
        return Decimal(text.strip()) == Decimal(str(known).strip())
    except (InvalidOperation, ValueError):
        return text == str(known)


def entry(verdict: str, note: str = "", strict=None, value=None) -> dict:
    return dict(verdict=verdict, strict_match=strict, value_match=value, note=note)


class Recorder:
    """Captures answers during a run (or reads them back) and classifies them."""

    def __init__(self, directory: Path, reference: str | list[str], adjudicated: dict[tuple, str] | None = None):
        self.directory = directory
        self.references = [s.strip() for s in reference.split(",") if s.strip()] if isinstance(reference, str) \
            else list(reference)
        self.adjudicated = adjudicated or {}
        self.captures: list[Capture] = []
        self.queries: dict[tuple, str] = {}  # item -> the query's SHA-256
        self.orders: dict[tuple, tuple[str, int | None]] = {}
        self.data: dict[tuple, tuple[str, ...]] = {}  # item -> the inputs it ran on
        self.expected: set[tuple[tuple, str]] = set()
        self.skipped: dict[tuple[tuple, str], tuple[str, str]] = {}
        self.known: dict[tuple, dict] = {}  # item -> {"value": v} and/or {"rows": n}: an oracle's answer

    def check(self, item: tuple, known: dict | None):
        """What an independent source knows of an item's answer: its one value, its row count."""
        if known:
            self.known[item] = dict(known)

    def oracle(self, item: tuple) -> dict:
        out = dict(self.known.get(item, {}))
        count = self.adjudicated.get((item[0], item[1], Path(item[3]).stem))
        if count is not None:
            out.setdefault("rows", int(count))
        return out

    def holds(self, item: tuple, answer: Answer) -> tuple[bool | None, bool, str]:
        """Whether an answer agrees with the oracle: (agrees, None without an oracle; decisive,
        whether that settles the whole answer; a note)."""
        known = self.oracle(item)
        order, _ = self.orders.get(item, ("bag", None))
        if not known or answer.boolean is not None or order == "limit":
            return None, False, ""
        if "value" in known:
            cells = [t for row in answer.rows for t in row.values()]
            agrees = len(answer.rows) == 1 and len(cells) == 1 and same_value(cells[0], known["value"])
            got = cells[0].get("value") if len(cells) == 1 else f"{len(answer.rows)} rows"
            return agrees, True, "" if agrees else f"{got} for the known value {known['value']}"
        agrees = len(answer.rows) == int(known["rows"])
        return agrees, False, "" if agrees else f"{len(answer.rows)} rows for the known {known['rows']}"

    @property
    def reference(self) -> str:
        return ",".join(self.references)

    def expect(self, item: tuple, system: str, data: tuple[str, ...] = ()):
        """A capture that should come: without one, the system's verdict is incomplete."""
        self.expected.add((item, system))
        if data:
            self.data[item] = tuple(data)

    def skip(self, item: tuple, system: str, why: str, verdict: str = "unsupported", data: tuple[str, ...] = ()):
        """A pair the suite skipped: `unsupported` when the system can't run the query, else
        `incomplete` (a missing input)."""
        self.skipped[(item, system)] = (verdict, why)
        if data:
            self.data.setdefault(item, tuple(data))

    def capture(self, post, url: str, item: tuple, system: str, cache: str, run: int, query: Path,
                logs: Path, timeout: float):
        text = query.read_text(encoding="utf-8")
        self.queries[item] = hashlib.sha256(text.encode()).hexdigest()
        self.orders[item] = order_of(text)
        self.captures.append(self.post(post, url, item, system, cache, run, text,
                                       logs / f"{query.stem}-answers-{cache}.json", timeout))
        if (system in self.references and self.orders[item][0] == "limit"
                and not any(c.unlimited and c.item == item and c.system == system for c in self.captures)):
            self.captures.append(self.post(post, url, item, system, cache, run, without_limit(text),
                                           logs / f"{query.stem}-answers-{cache}-without-limit.json", timeout,
                                           unlimited=True))

    @staticmethod
    def post(post, url: str, item: tuple, system: str, cache: str, run: int, text: str, path: Path,
             timeout: float, unlimited: bool = False) -> Capture:
        capture = Capture(item, system, cache, run, str(path), unlimited=unlimited)
        started = time.monotonic()
        try:
            code, body = post(url, {"query": text}, timeout=timeout)
            if code == -1:
                raise ValueError(body)
            path.write_text(body, encoding="utf-8")
            capture.sha256 = hashlib.sha256(body.encode()).hexdigest()
            if code == 0:
                waited = time.monotonic() - started
                raise ValueError(f"timeout after {timeout:g} s" if waited >= 0.95 * timeout else "no response")
            if code != 200:
                raise ValueError(f"HTTP {code}: {body.strip()[:200]}")
            capture.answer = parse(body)
        except (ValueError, OSError) as error:
            capture.error = f"{type(error).__name__}: {error}"[:300]
        return capture

    # --- classification ---
    def own(self, item: tuple, system: str, unlimited: bool = False) -> list[Capture]:
        return [c for c in self.captures if c.item == item and c.system == system and c.unlimited == unlimited]

    def usable(self, item: tuple, system: str) -> tuple[Answer | None, Answer | None, str]:
        """A reference candidate's answer and its pool without the LIMIT, or None and why not."""
        own = self.own(item, system)
        if not own or any(c.answer is None for c in own):
            return None, None, f"{system} has no complete answer"
        first = own[0].answer
        if first.blank:
            return None, None, "blank nodes: the answers need an isomorphism check"
        order, limit = self.orders.get(item, ("bag", None))
        pool = next((c.answer for c in self.own(item, system, unlimited=True) if c.answer is not None), None)
        if order == "limit" and pool is None:
            return None, None, f"{system} has no answer without the LIMIT"
        full = pool if order == "limit" else first
        if any(not compare(c.answer, first, order, limit, full)["strict_match"] for c in own[1:]):
            return None, None, f"{system}'s captures differ among themselves"
        agrees, _, note = self.holds(item, first)
        if agrees is False:
            return None, None, f"{system}'s answer contradicts the known answer: {note}"
        return first, pool, ""

    def reference_of(self, item: tuple) -> dict:
        """The item's reference: system, answer, pool, the confirming system, a dispute, why
        none."""
        found, skipped = [], []
        for system in self.references:
            answer, pool, why = self.usable(item, system)
            if answer is None:
                skipped.append(why)
            else:
                found.append((system, answer, pool))
            if len(found) == 2:
                break
        if not found:
            return dict(system=None, why="; ".join(skipped) or "no reference system")
        system, answer, pool = found[0]
        order, limit = self.orders.get(item, ("bag", None))
        out = dict(system=system, answer=answer, pool=pool, confirmed="", dispute="", why="")
        if len(found) > 1:
            second, other, other_pool = found[1]
            # A limit is confirmed on the pools, which both references answered in full.
            mine, theirs = (pool, other_pool) if order == "limit" else (answer, other)
            check = compare(theirs, mine, "bag" if order == "limit" else order)
            if check["verdict"] == "ok":
                out["confirmed"] = second
            else:
                out["dispute"] = f"the references {system} and {second} disagree ({check['note']}): adjudicate"
        if self.holds(item, answer)[0] and not out["dispute"]:
            out["confirmed"] = out["confirmed"] or "the known answer"
        return out

    def verdict(self, item: tuple, system: str, reference: dict) -> dict:
        own = self.own(item, system)
        if not own:
            if (item, system) in self.skipped:
                return entry(*self.skipped[(item, system)])
            return entry("incomplete", "not captured")
        failed = [c for c in own if c.answer is None]
        if failed:
            errors = "; ".join(dict.fromkeys(c.error for c in failed if c.error))
            return entry("unsupported" if UNSUPPORTED.search(errors) else "incomplete", errors)
        held = [self.holds(item, c.answer) for c in own]
        if any(agrees is False for agrees, _, _ in held):
            return entry("wrong", next(note for agrees, _, note in held if agrees is False), False, False)
        if reference["system"] is None:
            why = reference["why"]
            if held and all(agrees for agrees, _, _ in held):
                if all(decisive for _, decisive, _ in held):
                    return entry("ok", f"the known answer (no reference answered: {why})", None, True)
                return entry("unqualified", f"the known row count holds, the bindings are unchecked; {why}")
            return entry("unqualified" if why.startswith("blank") else "incomplete", "no reference: " + why)
        if reference["dispute"]:
            return entry("unqualified", reference["dispute"])
        if system == reference["system"]:
            confirmed = reference["confirmed"]
            return entry("ok" if confirmed else "reference",
                         f"confirmed by {confirmed}" if confirmed else "the reference, unconfirmed", True, True)
        order, limit = self.orders.get(item, ("bag", None))
        each = [compare(c.answer, reference["answer"], order, limit, reference["pool"]) for c in own]
        bad = [e for e in each if e["verdict"] != "ok"]
        if bad:
            return bad[0]
        return dict(verdict="ok", value_match=True, strict_match=all(e["strict_match"] for e in each),
                    note=next((e["note"] for e in each if e["note"]), ""))

    def items(self) -> list[tuple]:
        return sorted({c.item for c in self.captures} | {i for i, _ in self.expected} | {i for i, _ in self.skipped})

    def verdicts(self) -> list[dict]:
        out = []
        for item in self.items():
            reference = self.reference_of(item)
            order, _ = self.orders.get(item, ("bag", None))
            systems = ({c.system for c in self.captures if c.item == item}
                       | {s for i, s in self.expected if i == item} | {s for i, s in self.skipped if i == item})
            for system in sorted(systems):
                out.append(dict(workload=item[0], tier=item[1], regime=item[2], query=item[3],
                                query_sha256=self.queries.get(item), order=order, system=system,
                                reference=reference["system"] or "-", captures=len(self.own(item, system)),
                                **self.verdict(item, system, reference)))
        return out

    def group_key(self, item: tuple) -> tuple:
        """A distinct query on the same data under the same regime (repeats across cases are one)."""
        data = self.data.get(item) or (item[0], item[1])
        return (item[2], tuple(data), self.queries.get(item) or item[3])

    def groups(self, verdicts: list[dict] | None = None) -> list[dict]:
        verdicts = self.verdicts() if verdicts is None else verdicts
        by_key: dict[tuple, list[dict]] = {}
        for v in verdicts:
            item = (v["workload"], v["tier"], v["regime"], v["query"])
            by_key.setdefault(self.group_key(item), []).append(v)
        out = []
        for key, members in by_key.items():
            systems: dict[str, dict] = {}
            for v in members:
                held = systems.get(v["system"])
                if held is None or PRECEDENCE.index(v["verdict"]) < PRECEDENCE.index(held["verdict"]):
                    systems[v["system"]] = v
            worst = min((v["verdict"] for v in systems.values()), key=PRECEDENCE.index)
            # Whose: the systems that gave no usable answer, not those left without a reference.
            whose = {kind: sorted(s for s, v in systems.items() if v["verdict"] == kind
                                  and not v["note"].startswith("no reference"))
                     for kind in ("wrong", "unqualified", "incomplete", "unsupported")}
            strict = {s: v["note"] for s, v in systems.items() if v["verdict"] == "ok" and v["strict_match"] is False}
            items = sorted({f"{v['tier']}/{v['query']}" for v in members})
            out.append(dict(group=items[0], items=items, regime=key[0], data=list(key[1]),
                            query_sha256=members[0]["query_sha256"], order=members[0]["order"],
                            reference=next((v["reference"] for v in members if v["reference"] != "-"),
                                           "known answer" if any(v["note"].startswith("the known answer")
                                                                 for v in members) else "-"),
                            verdict="ok" if worst == "reference" else worst, **whose, strict_differs=strict,
                            systems={s: dict(verdict=v["verdict"], strict_match=v["strict_match"], note=v["note"])
                                     for s, v in sorted(systems.items())}))
        return sorted(out, key=lambda g: g["group"])

    # --- the record ---
    def write(self, path: Path | None = None) -> Path:
        verdicts = self.verdicts()
        groups = self.groups(verdicts)
        path = path or self.directory / "answers.json"

        def where(p: str) -> str:
            try:
                return Path(p).resolve().relative_to(path.parent.resolve()).as_posix()
            except ValueError:
                return p
        path.write_text(json.dumps(dict(
            references=self.references,
            items=[dict(item=list(i), query_sha256=self.queries.get(i), order=self.orders.get(i, ("bag", None))[0],
                        limit=self.orders.get(i, ("bag", None))[1], data=list(self.data.get(i, ())),
                        known=self.known.get(i))
                   for i in self.items()],
            captures=[dict(item=list(c.item), system=c.system, cache=c.cache, run=c.run, path=where(c.path),
                           sha256=c.sha256, error=c.error or None, unlimited=c.unlimited) for c in self.captures],
            skipped=[dict(item=list(i), system=s, verdict=v, why=w) for (i, s), (v, w) in sorted(self.skipped.items())],
            expected=[dict(item=list(i), system=s) for i, s in sorted(self.expected)],
            verdicts=verdicts,
            groups=groups,
            summary=dict(groups=dict(Counter(g["verdict"] for g in groups)),
                         verdicts=dict(Counter(v["verdict"] for v in verdicts))),
            caveat="Untimed: one full answer per query, cache mode and fresh-store repetition, captured after "
                   "the timed queries while the server ran; serve-phase memory includes it."), indent=1) + "\n",
            encoding="utf-8")
        return path

    @classmethod
    def load(cls, paths: list[Path], reference: str | list[str] | None = None,
             adjudicated: dict[tuple, str] | None = None) -> "Recorder":
        """The captures of one or more answers.json files, read back from their files, under
        the references given (else the first file's)."""
        records = [(p, json.loads(p.read_text(encoding="utf-8"))) for p in paths]
        first = records[0][1]
        recorder = cls(paths[0].parent, reference or first.get("references") or first.get("reference", ""), adjudicated)
        for path, record in records:
            for i in record.get("items", []):
                item = tuple(i["item"])
                recorder.check(item, i.get("known"))
                recorder.queries[item] = i.get("query_sha256")
                recorder.orders[item] = (i.get("order") or "bag", i.get("limit"))
                if i.get("data"):
                    recorder.data[item] = tuple(i["data"])
            for e in record.get("expected", []):
                recorder.expected.add((tuple(e["item"]), e["system"]))
            for s in record.get("skipped", []):
                recorder.skipped[(tuple(s["item"]), s["system"])] = (s.get("verdict", "unsupported"), s["why"])
            for c in record["captures"]:
                capture = Capture(tuple(c["item"]), c["system"], c["cache"], c["run"], c["path"], c.get("sha256"),
                                  error=c.get("error") or "", unlimited=c.get("unlimited", False))
                file = Path(c["path"]) if Path(c["path"]).is_absolute() else path.parent / c["path"]
                if not capture.error:
                    try:
                        capture.answer = parse(file.read_text(encoding="utf-8"))
                    except (OSError, ValueError) as error:
                        capture.error = f"{type(error).__name__}: {error}"[:300]
                recorder.captures.append(capture)
        return recorder


def table(groups: list[dict]) -> str:
    """The groups as a Markdown table, worst first."""
    lines = ["| Group | Order | Reference | Verdict | Whose | Strict differences |", "|---|---|---|---|---|---|"]
    for g in sorted(groups, key=lambda g: (PRECEDENCE.index(g["verdict"]), g["group"])):
        whose = "; ".join(f"{kind}: {', '.join(g[kind])}" for kind in ("wrong", "unqualified", "incomplete", "unsupported")
                          if g[kind])
        strict = "; ".join(f"{s}: {note.removeprefix('datatypes differ: ')}" for s, note in sorted(g["strict_differs"].items()))
        repeats = f" (×{len(g['items'])})" if len(g["items"]) > 1 else ""
        whose = whose or ("no reference answered" if g["verdict"] == "incomplete" else "-")
        lines.append(f"| {g['group']}{repeats} | {g['order']} | {g['reference']} | {g['verdict']} | {whose} | {strict or '-'} |")
    counts = Counter(g["verdict"] for g in groups)
    lines.append("")
    lines.append(f"{len(groups)} groups: " + ", ".join(f"{counts[k]} {k}" for k in PRECEDENCE if counts.get(k)))
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    """`suite.py answers DIR... [--reference S[,S]] [--write FILE]`: the captures of runs
    classified again, as one table of groups."""
    import argparse
    from .summary import adjudications
    p = argparse.ArgumentParser(prog="suite.py answers", description=main.__doc__)
    p.add_argument("runs", nargs="+", help="results directories (with answers.json) or answers.json files")
    p.add_argument("--reference", help="the reference systems, in order (default: the runs' own)")
    p.add_argument("--write", help="write the combined classification here (answers.json format)")
    p.add_argument("--include-restricted", action="store_true",
                   help="also licensed systems' verdicts (never commit or publish them)")
    args = p.parse_args(argv)
    paths = [Path(r) / "answers.json" if Path(r).is_dir() else Path(r) for r in args.runs]
    recorder = Recorder.load(paths, args.reference, adjudications())
    if not args.include_restricted:
        import tomllib
        systems = tomllib.loads((Path(__file__).resolve().parent.parent / "systems.toml").read_text(encoding="utf-8"))

        def restricted(system: str) -> bool:
            return systems.get(system, {}).get("publish") == "permission"
        recorder.captures = [c for c in recorder.captures if not restricted(c.system)]
        recorder.expected = {(i, s) for i, s in recorder.expected if not restricted(s)}
        recorder.skipped = {k: w for k, w in recorder.skipped.items() if not restricted(k[1])}
    if args.write:
        print(f"written: {recorder.write(Path(args.write))}")
    print(table(recorder.groups()))
    return 0
