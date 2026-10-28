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
| `ok` | the same answer as the reference's (`level`: `exact`, `value` or `division`, below); `strict_match` says whether the terms are also the same as written |
| `wrong` | the values differ: the system's answer is wrong (the reference is checked as above) |
| `deviates` | the same values only across datatypes (`cross-type`), or a COUNT not typed `xsd:integer`: reported, not timed, not disqualifying |
| `incomplete` | no answer: an error, a timeout, not JSON, not captured; or no reference answered |
| `unsupported` | the system can't run the query (the suite skipped the pair, or the system says so) |
| `unqualified` | the comparison can't decide: blank nodes (they need an isomorphism), a decimal division without an exact oracle, or the references disagree |
| `reference` | the reference's own answer, not confirmed by a second reference or a count |

A group's verdict is its worst system's, in that order of precedence (PRECEDENCE, the one list
every report reads): `wrong` (naming whose), `unqualified`, `deviates`, `incomplete`,
`unsupported`, then `ok`. One deviating system never disqualifies the others.

**The levels** (rule.owl-rs-answer-comparison: the same datatype and value counts as the same
answer for timing; a numeric match across types is diagnostic only; only wrong answers
disqualify), each binding at the first that holds and the answer at its worst:

| Level | Holds when |
|---|---|
| `exact` | the RDF terms are identical |
| `value` | the same datatype IRI and the same value in its value space: integers and decimals exactly, doubles and floats by IEEE value, booleans `1`/`true`, plain literals as `xsd:string`, language tags case-insensitively; other datatypes as written |
| `division` | a computed decimal division (AVG, `/`) equal to its exact quotient (an oracle, by row) rounded half-even or truncated at its own last digit; fewer than 18 significant digits is a note (F&O 3.1 §4.2's SHOULD), not a fault. Without an oracle a difference there is `unqualified`: no output length, scale or tolerance qualifies it; it is `wrong` where the two results' own digits rule out one quotient. Only an `xsd:decimal` result: a division's double or float is compared by value |
| `cross-type` | the same numbers across datatypes: `deviates` |

A COUNT's variable must be `xsd:integer` (SPARQL 1.1 §18.5.1.2), whatever the reference says:
otherwise the answer is at best `deviates`, and no reference. The order compared:

| Query | Compared as |
|---|---|
| `ORDER BY` | a sequence (ties in the sort key are not yet told apart: such a difference shows as a sequence mismatch with equal bags, see `note`) |
| a final `LIMIT` without `ORDER BY` | a legal subset: as many rows as the limit allows, each from the reference's answer to the query without that `LIMIT` (captured from every reference system too) |
| anything else | a bag |
"""
from __future__ import annotations

import hashlib
import json
import math
import os
import re
import time
from collections import Counter
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from fractions import Fraction
from pathlib import Path

XSD = "http://www.w3.org/2001/XMLSchema#"
INTEGER = XSD + "integer"
# XSD's decimal and the types derived from it (the integers), and its two floating types.
DECIMAL_TYPES = {XSD + t for t in (
    "decimal", "integer", "int", "long", "short", "byte", "nonNegativeInteger", "positiveInteger",
    "nonPositiveInteger", "negativeInteger", "unsignedLong", "unsignedInt", "unsignedShort", "unsignedByte")}
FLOAT_TYPES = {XSD + "double", XSD + "float"}

ORDERED = re.compile(r"\bORDER\s+BY\b", re.I)
FINAL_LIMIT = re.compile(r"\s+(?:OFFSET\s+\d+\s+)?LIMIT\s+(\d+)(?:\s+OFFSET\s+\d+)?\s*$", re.I)
UNSUPPORTED = re.compile(r"unsupported|not supported|not implemented", re.I)

# The verdicts, worst first: a group's verdict is its worst system's. Every report reads them
# from here.
PRECEDENCE = ("wrong", "unqualified", "deviates", "incomplete", "unsupported", "reference", "ok")
# Those a group names its systems for.
FAULTS = PRECEDENCE[:5]
# Those whose time no report takes: not the reference's answer, or not known to be (a capture
# that failed after the timed runs included: incomplete).
UNTIMED = FAULTS
# An answer incomplete because no reference answered: nobody checked it.
UNCHECKED = "unchecked"


def timing_verdict(entry: dict) -> str:
    """A system's verdict on a query (Recorder.verdicts) as timing reads it: `unchecked` where
    it is incomplete because no reference answered (the answer may be right; nothing showed it),
    else its verdict."""
    if entry["verdict"] == "incomplete" and entry.get("note", "").startswith("no reference"):
        return UNCHECKED
    return entry["verdict"]


def timed(verdict: str | None) -> bool:
    """Whether a query's time may count with this answer verdict (timing_verdict): not one of
    UNTIMED, and not unchecked, since an answer nobody checked earns no time (T8 PR #56 R3).
    The reports that name winners (versus, compete) decide it here. No verdict at all (answers
    not captured) leaves it to the answer counts, as before."""
    return verdict is None or (verdict not in UNTIMED and verdict != UNCHECKED)
# F&O 3.1 §4.2: a decimal division SHOULD give at least 18 significant digits.
DIVISION_DIGITS = 18


def short(datatype: str | None) -> str:
    if not datatype:
        return "plain"
    return "xsd:" + datatype[len(XSD):] if datatype.startswith(XSD) else datatype


# XSD 1.1's lexical forms of a float's or double's infinities.
INFINITIES = {"INF": math.inf, "+INF": math.inf, "-INF": -math.inf}
FLOATING = re.compile(r"[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?")


def number(term: dict) -> Fraction | float | None:
    """A numeric literal's exact value (a double's binary64 one, a float's binary32 one), or a
    float's or double's infinity of either sign: INF, +INF, -INF, or a numeral beyond the type's
    range, which XSD 1.1 maps to the infinity of its sign (T8 PR #56 R2). None for anything else,
    NaN or an ill-typed literal."""
    if term.get("type") not in ("literal", "typed-literal") or "xml:lang" in term:
        return None
    datatype, text = term.get("datatype"), term.get("value", "").strip()
    try:
        if datatype in DECIMAL_TYPES:
            return Fraction(Decimal(text))
        if datatype in FLOAT_TYPES and text in INFINITIES:
            return INFINITIES[text]
        if datatype in FLOAT_TYPES and not FLOATING.fullmatch(text):
            return None
        if datatype == XSD + "float":
            try:
                return binary32(Fraction(Decimal(text)))
            except OverflowError:
                return math.copysign(math.inf, Decimal(text))
        if datatype in FLOAT_TYPES:
            value = float(text)
            return Fraction(value) if math.isfinite(value) else value
    except (InvalidOperation, ValueError, OverflowError):
        pass
    return None


def binary32(value: Fraction) -> Fraction:
    """VALUE rounded to the nearest IEEE binary32 value, ties to even: an xsd:float's value from
    its exact literal (through a binary64 it would round twice); OverflowError past the largest."""
    if value == 0:
        return value
    magnitude = abs(value)
    exponent = magnitude.numerator.bit_length() - magnitude.denominator.bit_length()
    if magnitude < Fraction(2) ** exponent:
        exponent -= 1  # 2^exponent <= magnitude < 2^(exponent + 1)
    unit = Fraction(2) ** (max(exponent, -126) - 23)  # 24 significant bits; subnormals below 2^-126
    rounded = round(magnitude / unit) * unit
    if rounded >= Fraction(2) ** 128:
        raise OverflowError("beyond binary32")
    return rounded if value > 0 else -rounded


def value_key(term: dict) -> tuple:
    """A term by its datatype and value: the `value` level."""
    kind = term.get("type")
    if kind in ("literal", "typed-literal"):
        datatype, text = term.get("datatype"), term.get("value", "")
        if "xml:lang" in term:
            return ("lang", text, term["xml:lang"].lower())
        if datatype in (None, XSD + "string"):
            return ("string", text)
        if datatype in FLOAT_TYPES and text.strip() == "NaN":
            return ("number", datatype, "NaN")
        value = number(term)
        if value is not None:
            return ("number", datatype, value)
        if datatype == XSD + "boolean" and text.strip() in ("true", "false", "1", "0"):
            return ("boolean", text.strip() in ("true", "1"))
        return ("literal", text, datatype)  # also an ill-typed literal: as written
    return (kind, term.get("value"))


def cross_key(term: dict) -> tuple:
    """A term by its value across numeric datatypes: the `cross-type` level, diagnostic only."""
    value = number(term)
    return ("number", value) if value is not None else value_key(term)


def strict_key(term: dict) -> str:
    return json.dumps(term, sort_keys=True, separators=(",", ":"))


def select_expressions(query: str) -> dict[str, str]:
    """Each projected `(EXPRESSION AS ?v)` of a query, by variable (IRIs and strings blanked, so
    their characters don't count as operators)."""
    text = re.sub(r"<[^<>\s]*>|\"(?:[^\"\\]|\\.)*\"|'(?:[^'\\]|\\.)*'", "<>", query)
    out = {}
    for m in re.finditer(r"\bAS\s+\?(\w+)\s*\)", text, re.I):
        depth, i = 0, m.start()
        while i > 0:
            i -= 1
            if text[i] == ")":
                depth += 1
            elif text[i] == "(":
                if depth == 0:
                    break
                depth -= 1
        out[m.group(1)] = text[i + 1:m.start()].strip()
    return out


def called(expression: str) -> str | None:
    """The function an expression is one call of (`COUNT(*)`, not `COUNT(*) / 2`), upper case."""
    m = re.match(r"(\w+)\s*\(", expression)
    if not m:
        return None
    depth = 0
    for i in range(m.end() - 1, len(expression)):
        depth += {"(": 1, ")": -1}.get(expression[i], 0)
        if depth == 0:
            return m.group(1).upper() if i == len(expression) - 1 else None
    return None


def operators(query: str) -> dict[str, str]:
    """The variables whose datatype the query's operator fixes or makes approximate: `count` (a
    COUNT's own result, so xsd:integer) and `division` (AVG or `/` in it: decimal division,
    exact only by an oracle)."""
    out = {}
    for v, expression in select_expressions(query).items():
        if called(expression) == "COUNT":
            out[v] = "count"
        elif re.search(r"\bAVG\s*\(", expression, re.I) or "/" in expression:
            out[v] = "division"
    return out


def count_faults(answer: Answer, ops: dict[str, str]) -> list[str]:
    """The COUNT variables an answer gives in another datatype than xsd:integer."""
    return sorted({f"?{v} {short(t.get('datatype'))}" for row in answer.rows for v, t in row.items()
                   if ops.get(v) == "count" and t.get("datatype") != INTEGER})


def division_holds(term: dict, quotient: Fraction) -> tuple[bool, str]:
    """Whether a decimal division's result is its exact quotient rounded half-even or truncated
    at its own last digit; a note when it has fewer significant digits than F&O 3.1 asks."""
    if term.get("datatype") != XSD + "decimal" or number(term) is None:
        return False, ""
    value = Decimal(term["value"].strip())
    scale = 10 ** max(0, -value.as_tuple().exponent)
    if Fraction(value) not in (Fraction(int(quotient * scale), scale), Fraction(round(quotient * scale), scale)):
        return False, ""
    significant = len(value.normalize().as_tuple().digits)
    exact = (quotient * scale).denominator == 1
    return True, "" if exact or significant >= DIVISION_DIGITS else \
        f"{significant} significant digits, fewer than F&O 3.1 §4.2's {DIVISION_DIGITS}"


@dataclass
class Answer:
    variables: tuple[str, ...] = ()
    rows: list[dict] = field(default_factory=list)
    boolean: bool | None = None

    @property
    def blank(self) -> bool:
        return any(t.get("type") == "bnode" for row in self.rows for t in row.values())


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


def decimal(term: dict) -> bool:
    return term.get("datatype") == XSD + "decimal" and number(term) is not None


def row_keys(answer: Answer, key, masked=frozenset()) -> list[tuple]:
    """Each row by KEY, the decimal results of the MASKED variables (divisions) left to their
    oracle: compared only as being such results. Any other result of theirs is compared by KEY."""
    return [tuple(sorted((v, ("decimal division",) if v in masked and decimal(t) else key(t)) for v, t in row.items()))
            for row in answer.rows]


def agree(answer: Answer, reference: Answer, order: str, limit: int | None, full: Answer | None,
          key, masked=frozenset()) -> bool:
    if (answer.boolean is None) != (reference.boolean is None):
        return False
    if answer.boolean is not None:
        return answer.boolean == reference.boolean
    if answer.variables != reference.variables:
        return False
    if order == "sequence":
        return row_keys(answer, key, masked) == row_keys(reference, key, masked)
    if order == "limit":
        pool = Counter(row_keys(full, key, masked))
        got = Counter(row_keys(answer, key, masked))
        return sum(got.values()) == min(limit, sum(pool.values())) and not (got - pool)
    return Counter(row_keys(answer, key, masked)) == Counter(row_keys(reference, key, masked))


def oracle_of(spec: dict) -> dict:
    """An oracle as the fixtures write it (benches/suite/fixtures/answer-levels.json: per
    variable, each row's other columns and its quotient "n/d"), as `compare` takes it."""
    return {v: {other_columns(e["row"], ()): Fraction(e["quotient"]) for e in entries} for v, entries in spec.items()}


def other_columns(row: dict, columns) -> tuple:
    """A row by its bindings outside the division COLUMNS, by value across datatypes: which row
    a division's result belongs to (a datatype deviation there is reported on its own)."""
    return tuple(sorted((v, cross_key(t)) for v, t in row.items() if v not in columns))


def divisions(answer: Answer, columns: set[str], oracle: dict | None) -> tuple[bool | None, str]:
    """Whether every decimal result of the division COLUMNS is its exact quotient (the oracle:
    per variable, by the row's other columns); None when there is no oracle for one. Any other
    result of theirs was compared by value (`row_keys`)."""
    if not oracle or any(v not in oracle for v in columns):
        return None, ""
    notes = set()
    for row in answer.rows:
        other = other_columns(row, columns)
        for v in sorted(c for c in columns if c not in row or decimal(row[c])):
            quotient = oracle[v].get(other)
            if v not in row or quotient is None:
                return None, ""
            holds, note = division_holds(row[v], quotient)
            if not holds:
                return False, f"?{v} {row[v].get('value')} isn't the exact quotient {quotient} at its own digits"
            if note:
                notes.add(f"?{v}: {note}")
    return True, "; ".join(sorted(notes))


def apart(a: dict, b: dict) -> bool:
    """Whether two decimal division results can't both be one quotient rounded or truncated at
    their own last digits: each is less than a unit in its last place from it."""
    x, y = (Decimal(t["value"].strip()) for t in (a, b))
    return abs(Fraction(x) - Fraction(y)) >= sum(Fraction(1, 10 ** max(0, -d.as_tuple().exponent)) for d in (x, y))


def refuted(answer: Answer, reference: Answer, columns: set[str]) -> str:
    """A decimal result of the division COLUMNS that the reference's, in the one row equal but
    for those results, rules out (`apart`); '' where none does. Without an oracle this tells a
    wrong answer from an undecided one; it never qualifies one."""
    theirs: dict[tuple, list[dict]] = {}
    for key, row in zip(row_keys(reference, cross_key, columns), reference.rows):
        theirs.setdefault(key, []).append(row)
    for key, row in zip(row_keys(answer, cross_key, columns), answer.rows):
        paired = theirs.get(key, [])
        for v in sorted(columns) if len(paired) == 1 else ():
            a, b = row.get(v), paired[0].get(v)
            if a and b and decimal(a) and decimal(b) and apart(a, b):
                return f"?{v} {a['value']} and the reference's {b['value']} can't both be one quotient at their digits"
    return ""


def compare(answer: Answer, reference: Answer, order: str = "bag", limit: int | None = None,
            full: Answer | None = None, ops: dict[str, str] | None = None, oracle: dict | None = None) -> dict:
    """One answer against the reference's, each binding at the first level that holds (the
    module's table) and the answer at its worst: verdict (ok, wrong, deviates, incomplete,
    unqualified), level, strict_match, value_match, note. OPS: the query's operators
    (`operators`), None where they are unknown (any decimal result may then be a division's);
    ORACLE: the exact quotients of its divisions."""
    def result(verdict, level=None, strict=None, value=None, note=""):
        return dict(verdict=verdict, level=level, strict_match=strict, value_match=value, note=note)

    def agrees(key, masked=frozenset()):
        return agree(answer, reference, order, limit, full, key, masked)
    if answer.blank or reference.blank:
        return result("unqualified", note="blank nodes: the answers need an isomorphism check")
    if order == "limit" and full is None:
        return result("incomplete", note="no reference answer without the LIMIT")
    known = ops is not None
    ops = ops or {}
    faults = count_faults(answer, ops)
    strict = agrees(strict_key)
    value = strict or agrees(value_key)
    cross = value or agrees(cross_key)
    level, note = ("exact" if strict else "value") if value else None, ""
    mine, theirs = datatypes(answer), datatypes(reference)
    differing = "; ".join(f"?{v} {', '.join(sorted(mine.get(v, {})))} for {', '.join(sorted(theirs.get(v, {})))}"
                          for v in sorted(set(mine) | set(theirs)) if set(mine.get(v, {})) != set(theirs.get(v, {})))
    why = [f"COUNT not xsd:integer: {', '.join(faults)}"] if faults else []
    division = {v for v, op in ops.items() if op == "division"} if known else set(answer.variables)
    if not cross and division and agrees(cross_key, division):
        # Every other binding agrees, at most across datatypes: the divisions' decimal results
        # are their exact quotients' to decide.
        held, note = divisions(answer, division, oracle)
        if held is None:
            note = refuted(answer, full if order == "limit" else reference, division)
            if note:
                return result("wrong", None, False, False, note)
            what = f"decimal division ({', '.join('?' + v for v in sorted(division))})" if known \
                else "decimal results, any of them a division's (the query's operators unknown),"
            return result("unqualified", strict=False, value=False, note="; ".join([
                f"{what} without an exact oracle: no output length, scale or tolerance qualifies it", *why]))
        if not held:
            return result("wrong", None, False, False, note)
        cross, value = True, agrees(value_key, division)
        level = "division" if value else None
    if value and not faults:
        if level == "value":
            note = note or "terms differ as written"
        return result("ok", level, strict, True, note)
    if cross:
        why += [f"datatypes differ: {differing}"] if differing and not value else []
        return result("deviates", "cross-type", strict, False, "; ".join(why + ([note] if note else [])))
    if order == "sequence" and Counter(row_keys(answer, value_key)) == Counter(row_keys(reference, value_key)):
        note = "same rows in another order (ties in the sort key are not told apart)"
    elif answer.boolean is None and reference.boolean is None:
        note = note or f"{len(answer.rows)} rows for the reference's {len(reference.rows)}"
    return result("wrong", None, False, False, note)


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


def known_term(known) -> dict:
    """A known value as the term it stands for: an integer is xsd:integer (a count), another
    number xsd:decimal, anything else a plain literal."""
    text = str(known).strip()
    for datatype, parse_ in ((INTEGER, int), (XSD + "decimal", Decimal)):
        try:
            parse_(text)
            return {"type": "literal", "value": text, "datatype": datatype}
        except (ValueError, InvalidOperation):
            continue
    return {"type": "literal", "value": str(known)}


def same_value(term: dict, known) -> str | None:
    """A term against a known value: `value` (same datatype and value), `cross-type` (the same
    number in another datatype: deviates) or None."""
    expected = known_term(known)
    if value_key(term) == value_key(expected):
        return "value"
    return "cross-type" if cross_key(term) == cross_key(expected) else None


def entry(verdict: str, note: str = "", strict=None, value=None, level=None) -> dict:
    return dict(verdict=verdict, level=level, strict_match=strict, value_match=value, note=note)


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
        self.operators: dict[tuple, dict[str, str] | None] = {}  # item -> its COUNT and division variables
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

    def holds(self, item: tuple, answer: Answer) -> tuple[str | None, bool, str]:
        """How an answer stands to the oracle: (`holds`, `contradicts`, `deviates` (the known
        value only across datatypes), or None without an oracle; decisive, whether that settles
        the whole answer; a note)."""
        known = self.oracle(item)
        order, _ = self.orders.get(item, ("bag", None))
        if not known or answer.boolean is not None or order == "limit":
            return None, False, ""
        if "value" in known:
            cells = [t for row in answer.rows for t in row.values()]
            level = same_value(cells[0], known["value"]) if len(answer.rows) == 1 and len(cells) == 1 else None
            got = f"{cells[0].get('value')} ({short(cells[0].get('datatype'))})" if len(cells) == 1 \
                else f"{len(answer.rows)} rows"
            if level == "value":
                return "holds", True, ""
            if level == "cross-type":
                return "deviates", True, f"{got} for the known {known['value']} ({short(known_term(known['value']).get('datatype'))})"
            return "contradicts", True, f"{got} for the known value {known['value']}"
        if len(answer.rows) == int(known["rows"]):
            return "holds", False, ""
        return "contradicts", False, f"{len(answer.rows)} rows for the known {known['rows']}"

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
                logs: Path, timeout: float, *, prior_error: str | None = None):
        text = query.read_text(encoding="utf-8")
        self.queries[item] = hashlib.sha256(text.encode()).hexdigest()
        self.orders[item] = order_of(text)
        self.operators[item] = operators(text)
        if prior_error:
            # Preserve this invocation's failure; answer collection is not an implicit
            # second attempt with the same settings. A recovery run has its own identity.
            self.captures.append(Capture(item, system, cache, run, "",
                                         error=f"timed query failed; not recaptured: {prior_error}"))
            return
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
        state, _, note = self.holds(item, first)
        if state in ("contradicts", "deviates"):
            return None, None, f"{system}'s answer {state} the known answer: {note}"
        faults = count_faults(first, self.operators.get(item) or {})
        if faults:
            return None, None, f"{system}'s COUNT is not xsd:integer: {', '.join(faults)}"
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
            check = compare(theirs, mine, "bag" if order == "limit" else order, ops=self.operators.get(item))
            if check["verdict"] == "ok":
                out["confirmed"] = second
            else:
                out["dispute"] = f"the references {system} and {second} disagree ({check['note']}): adjudicate"
        if self.holds(item, answer)[0] == "holds" and not out["dispute"]:
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
        for state, verdict in (("contradicts", "wrong"), ("deviates", "deviates")):
            if any(s == state for s, _, _ in held):
                return entry(verdict, next(note for s, _, note in held if s == state), False, False)
        if reference["system"] is None:
            why = reference["why"]
            if held and all(s == "holds" for s, _, _ in held):
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
        each = [compare(c.answer, reference["answer"], order, limit, reference["pool"], self.operators.get(item))
                for c in own]
        bad = [e for e in each if e["verdict"] != "ok"]
        if bad:
            return min(bad, key=lambda e: PRECEDENCE.index(e["verdict"]))
        return dict(verdict="ok", level=max((e["level"] for e in each), key=("exact", "value", "division").index),
                    value_match=True, strict_match=all(e["strict_match"] for e in each),
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
                     for kind in FAULTS}
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
                        limit=self.orders.get(i, ("bag", None))[1], operators=self.operators.get(i, {}),
                        data=list(self.data.get(i, ())), known=self.known.get(i))
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
                recorder.operators[item] = i.get("operators")  # None: not recorded (before T8), unknown
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
        whose = "; ".join(f"{kind}: {', '.join(g[kind])}" for kind in FAULTS if g.get(kind))
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
