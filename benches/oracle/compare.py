#!/usr/bin/env python3
"""Compares NRESE's rows with Jena's for every query of a dump (README.md) and writes a
Markdown report.

    benches/oracle/compare.py <dump> [--report report.md]

A query agrees when the answer contract (benches/suite/suitekit/answers.py) finds NRESE's
answer the same as Jena's (`ok`) on the dataset as NRESE loaded it, or on the copy with
numbers in canonical lexical forms (NRESE's STR of "07"^^xsd:integer is "7": a known
deviation, README.md); equal rows are no agreement on their own. Rows are compared as
multisets, and in order where q<M>.ordered exists; terms by the same datatype and value,
language tags in any case, plain literals as xsd:string. Blank nodes, results of a decimal
division (AVG, `/`), numbers equal only across datatypes and a COUNT not in xsd:integer are
never an agreement: the report names them (blank-nodes, division, cross-type) as the
contract classifies them.

A difference is explained when one of the rules below says why the standard allows both
answers, or names a known deviation; the report groups them by rule. Exit 1 if an
unexplained difference or a Jena error remains.
"""
import argparse
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "suite"))
from suitekit import answers as contract  # noqa: E402

TYPED = re.compile(r'^"(.*)"\^\^<([^>]*)>$', re.S)
LANG = re.compile(r'^"(.*)"@([A-Za-z0-9-]+)$', re.S)
PLAIN = re.compile(r'^"(.*)"$', re.S)


def as_term(text: str) -> dict:
    """A term of a dump (N-Triples form) as the contract's SPARQL JSON term."""
    if text.startswith("_:"):
        return {"type": "bnode", "value": text[2:]}
    if text.startswith("<") and text.endswith(">"):
        return {"type": "uri", "value": text[1:-1]}
    m = TYPED.match(text)
    if m:
        return {"type": "literal", "value": m.group(1), "datatype": m.group(2)}
    m = LANG.match(text)
    if m:
        return {"type": "literal", "value": m.group(1), "xml:lang": m.group(2)}
    m = PLAIN.match(text)
    return {"type": "literal", "value": m.group(1) if m else text}


def term(text: str) -> str:
    """A term as the contract's `value` level compares it, written back as text; a term with no
    value semantics as written (the rules read dates in it)."""
    if text.startswith("_:"):
        return "_:b"
    k = contract.value_key(as_term(text))
    if k[0] == "number":
        return "num:" + ":".join(str(part) for part in k[1:])
    if k[0] == "string":
        return f'"{k[1]}"'
    if k[0] == "lang":
        return f'"{k[1]}"@{k[2]}'
    if k[0] == "boolean":
        return f"bool:{k[1]}"
    return text


def row(line: str) -> str:
    return "\t".join(term(t) for t in line.split("\t"))


def table(path: Path) -> tuple[list[str], list[str]]:
    """The variables (first line) and the rows of an answer file."""
    text = path.read_text(encoding="utf-8").split("\n")
    return text[0].split("\t"), [l for l in text[1:] if l != ""]


def aligned(variables: list[str], rows: list[str], order: list[str]) -> list[str]:
    """Rows with their columns in the order of `order` (engines order SELECT * differently)."""
    if variables == order or sorted(variables) != sorted(order):
        return rows
    index = [variables.index(v) for v in order]
    return ["\t".join(r.split("\t")[i] for i in index) for r in rows]


def same(ours: list[str], theirs: list[str], ordered: bool) -> bool:
    return ours == theirs if ordered else Counter(ours) == Counter(theirs)


def masked(rows: list[str], columns: list[int]) -> Counter:
    def mask(r):
        cells = r.split("\t")
        return "\t".join("*" if i in columns else c for i, c in enumerate(cells))
    return Counter(mask(r) for r in rows)


PATH_ALTERNATIVE = re.compile(r"[>)]\s*\|\s*[<(^!]")
MIN_MAX = re.compile(r"\(\(?(?:MIN|MAX)\([^()]*\)\)?\s+AS\s+\?(\w+)\)", re.I)
STRING_OF_VALUE = re.compile(r"\bSTR\(\?")
# A triple inside (NOT) EXISTS with a variable as its predicate.
EXISTS_PREDICATE_VARIABLE = re.compile(r"EXISTS\s*\{[^{}]*\S+\s+\?\w+\s+\S+\s*\.[^{}]*\S+\s+\S+\s+\S+[^{}]*\}")
PATH = re.compile(r"[>)]\s*[|+*?]|[|/^!]\s*[<(]")
ZERO_LENGTH = re.compile(r"[)>]\s*[*?]\s*(<http://example\.com/e6>|\?)|<http://example\.com/e6>\s+\S+[*?]\s")

# Why a difference is allowed, or which known deviation it is (README.md).
RULES = {
    "order": "ORDER BY between incomparable values (literals of different types) is left to the implementation",
    "min-max": "MIN and MAX over incomparable values follow that order too",
    "limit": "ORDER BY with LIMIT or OFFSET over incomparable values picks different rows",
    "relative-iri": "IRI() of a relative string: Jena resolves it against its file:/// base, NRESE (no BASE) makes it an error",
    "path-duplicates": "known deviation: path alternatives (p|q) give each solution once in NRESE, as a UNION with duplicates in Jena (the standard)",
    "lexical-forms": "known deviation: STR of a number gives its canonical form in NRESE (STR of \"07\"^^xsd:integer is \"7\"), where joins or grouping keep the canonical copy of the data from matching either",
    "date-functions": "YEAR, MONTH, DAY and the other accessors take xsd:dateTime in SPARQL; Jena also takes xsd:date",
    "zero-length": "known deviation: zero-length paths from a term outside the graph (completion plan 4.3)",
    "jena-path-values": "Jena's property paths match a bound literal by value (\"01\"^^xsd:integer finds \"1\"^^xsd:int), where the standard asks for the same term; Jena has the extra rows",
    "jena-exists-literal-predicate": "Jena drops the row when a (NOT) EXISTS of two or more patterns gets a literal for a predicate variable; the substituted pattern matches nothing, so NOT EXISTS holds; NRESE has the extra rows",
    "jena-path-values-aggregated": "Jena's path value matching (jena-path-values) under an aggregate: on the canonical copy both engines agree, Jena answers both copies alike (it matched by value), NRESE doesn't (it matched terms, as the standard asks)",
    "date-timezones": "= and != between a date or dateTime with a timezone and one without: unequal in NRESE (XSD 1.1: values with and without a timezone are never equal), an error in Jena; NRESE has the extra rows",
    # The answer contract's own classes (benches/suite/suitekit/answers.py): not agreements.
    "blank-nodes": "blank nodes: the answers need an isomorphism check, which this comparison doesn't make (answers.py: unqualified)",
    "division": "the rows agree but for the decimal results of a division (AVG, /), which only an exact oracle decides, and none is given here; their digits don't rule each other out (answers.py: unqualified)",
    "cross-type": "the same numbers in other datatypes, or a COUNT not in xsd:integer (answers.py: deviates): reported, not the same answer",
}

DATE = re.compile(r'^"(-?\d{4,}-\d\d-\d\d(?:T[0-9:.]+)?)(Z|[+-]\d\d:\d\d)?"\^\^<http://www\.w3\.org/2001/XMLSchema#date(?:Time)?>$')


def mixes_timezones(row: str) -> bool:
    """Whether a row holds a date or dateTime with a timezone and one without."""
    zones = [m.group(2) is not None for m in map(DATE.match, row.split("\t")) if m]
    return any(zones) and not all(zones)


def answer_of(variables: list[str], rows: list[str]) -> contract.Answer:
    """Rows of a dump as the contract's answer."""
    names = [v.lstrip("?") for v in variables]
    return contract.Answer(tuple(sorted(names)), [dict(zip(names, map(as_term, line.split("\t")))) for line in rows])


def classify(query: str, variables: list[str], our_rows: list[str], raw: list[list[str]], ordered: bool) -> str | None:
    """The answer contract's verdict on a query, as the report names it: an agreement with Jena's
    rows (`agree`) or with Jena's on the canonical copy (`agree on canonical numbers`), blank
    nodes (never an agreement here), decimal division results only an exact oracle decides
    (`division`), or a deviation (`cross-type`: numbers equal across datatypes, a COUNT not in
    xsd:integer). None for a difference, which the rules may explain. Equal rows agree only
    where the contract says so (T8 PR #56 R1)."""
    if any(t.startswith("_:") for rows in [our_rows, *raw] for line in rows for t in line.split("\t")):
        return "blank-nodes"
    mine, ops = answer_of(variables, our_rows), contract.operators(query)
    verdicts = [contract.compare(mine, answer_of(variables, rows), "sequence" if ordered else "bag", ops=ops)["verdict"]
                for rows in raw]
    if verdicts[0] == "ok":
        return "agree"
    if verdicts[1:] == ["ok"]:
        return "agree on canonical numbers"
    return "division" if "unqualified" in verdicts else "cross-type" if "deviates" in verdicts else None


def explain(query: str, variables: list[str], ours: list[str], answers: list[list[str]],
            ours_canonical: list[str] | None = None, ordered: bool = False) -> str | None:
    # NRESE on the canonical copy agrees with Jena there, Jena answers both copies alike, and
    # NRESE doesn't: Jena matched by value where NRESE matched terms.
    if (PATH.search(query) and ours_canonical is not None and len(answers) > 1
            and same(ours_canonical, answers[1], ordered) and same(answers[0], answers[1], ordered)
            and not same(ours, ours_canonical, ordered)):
        return "jena-path-values-aggregated"
    if re.search(r"!?=", query) and answers:
        extra = list((Counter(ours) - Counter(answers[0])).elements())
        if extra and not (Counter(answers[0]) - Counter(ours)) and all(mixes_timezones(r) for r in extra):
            return "date-timezones"
    if any(Counter(ours) == Counter(theirs) for theirs in answers) and "ORDER BY" in query:
        return "order"
    if re.search(r"\b(IRI|URI)\(", query):
        return "relative-iri"
    if PATH_ALTERNATIVE.search(query) and any(set(ours) == set(theirs) for theirs in answers):
        return "path-duplicates"
    aggregates = [variables.index(f"?{v}") if f"?{v}" in variables else variables.index(v)
                  for v in MIN_MAX.findall(query) if f"?{v}" in variables or v in variables]
    if aggregates and any(masked(ours, aggregates) == masked(theirs, aggregates) for theirs in answers):
        return "min-max"
    if "ORDER BY" in query and re.search(r"\b(LIMIT|OFFSET)\b", query):
        # At the top, the same number of rows; in a subquery, its rows feed joins.
        if any(len(ours) == len(t) for t in answers) or re.search(r"\{\s*SELECT\b[^{}]*\{[^{}]*\}\s*ORDER BY", query):
            return "limit"
    if STRING_OF_VALUE.search(query):
        return "lexical-forms"
    if re.search(r"\b(YEAR|MONTH|DAY|HOURS|MINUTES|SECONDS|TIMEZONE|TZ)\(", query):
        return "date-functions"
    if ZERO_LENGTH.search(query):
        return "zero-length"
    if PATH.search(query) and any(not (Counter(ours) - Counter(theirs)) for theirs in answers):
        return "jena-path-values"
    if EXISTS_PREDICATE_VARIABLE.search(query) and any(not (Counter(theirs) - Counter(ours)) for theirs in answers):
        return "jena-exists-literal-predicate"
    return None


def main(argv):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("dump", type=Path)
    p.add_argument("--report", type=Path)
    args = p.parse_args(argv)
    counts = Counter()
    explained: dict[str, list[str]] = {k: [] for k in RULES}
    unexplained = []
    for nrese_file in sorted(args.dump.rglob("q*.nrese")):
        stem = nrese_file.with_suffix("")
        query = stem.with_suffix(".rq").read_text(encoding="utf-8").strip()
        where = f"{nrese_file.parent.parent.name}/{nrese_file.parent.name}/{stem.name}"
        counts["queries"] += 1
        variants = [f for f in (stem.with_suffix(".jena"), stem.with_suffix(".jena-canonical")) if f.exists()]
        if not variants:
            counts["unanswered"] += 1
            continue
        first = variants[0].read_text(encoding="utf-8").split("\n")[0]
        if first.startswith("ERROR "):
            counts["jena errors"] += 1
            unexplained.append(f"### {where}: Jena error\n\n```sparql\n{query}\n```\n\n{first[6:][:500]}\n")
            continue
        our_vars, our_rows = table(nrese_file)
        ours = [row(l) for l in our_rows]
        ordered = stem.with_suffix(".ordered").exists()
        raw = []
        for f in variants:
            their_vars, their_rows = table(f)
            raw.append(aligned(their_vars, their_rows, our_vars))
        answers = [[row(l) for l in rows] for rows in raw]
        contract_class = classify(query, our_vars, our_rows, raw, ordered)
        if contract_class in ("agree", "agree on canonical numbers"):
            counts[contract_class] += 1
            continue
        if contract_class:
            counts["explained"] += 1
            explained[contract_class].append(where)
            continue
        canonical_file = stem.with_suffix(".nrese-canonical")
        ours_canonical = None
        if canonical_file.exists():
            canonical_vars, canonical_rows = table(canonical_file)
            ours_canonical = [row(l) for l in aligned(canonical_vars, canonical_rows, our_vars)]
        rule = explain(query, our_vars, ours, answers, ours_canonical, ordered)
        if rule:
            counts["explained"] += 1
            explained[rule].append(where)
            continue
        counts["unexplained"] += 1
        theirs = answers[0]
        only_ours = list((Counter(ours) - Counter(theirs)).elements())[:5]
        only_theirs = list((Counter(theirs) - Counter(ours)).elements())[:5]
        detail = "(the same rows in a different order)\n\n" if ordered and not (only_ours or only_theirs) else ""
        unexplained.append(
            f"### {where}: NRESE {len(ours)} rows, Jena {len(theirs)}\n\n```sparql\n{query}\n```\n\n{detail}"
            f"only NRESE: `{only_ours}`\n\nonly Jena: `{only_theirs}`\n")
    summary = ", ".join(f"{v} {k}" for k, v in counts.items())
    lines = [f"# NRESE and Jena on the differential tests' queries\n\n{summary}\n", "## Explained\n"]
    for rule, places in explained.items():
        if places:
            shown = ", ".join(places[:8]) + (" …" if len(places) > 8 else "")
            lines.append(f"- **{rule}** ({len(places)}): {RULES[rule]}. {shown}")
    lines.append("\n## Unexplained\n")
    lines += unexplained or ["None."]
    if args.report:
        args.report.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(summary)
    for rule, places in explained.items():
        if places:
            print(f"  {rule}: {len(places)}")
    # No cases compared is a failure too: the dump step found no tests (a renamed test
    # once made the workflow pass on nothing).
    if counts["queries"] == 0 or counts["agree"] + counts["explained"] == 0:
        print("no queries were compared", file=sys.stderr)
        return 1
    return 1 if counts["unexplained"] or counts["jena errors"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
