#!/usr/bin/env python3
"""Compares NRESE's rows with Jena's for every query of a dump (README.md) and writes a
Markdown report.

    benches/oracle/compare.py <dump> [--report report.md]

A query agrees when NRESE's rows equal Jena's on the dataset as NRESE loaded it, or on the
copy with numbers in canonical lexical forms (NRESE's STR of "07"^^xsd:integer is "7": a
known deviation, README.md). Rows are compared as multisets, and in order where
q<M>.ordered exists. Terms are compared by value where the engines may write one value
differently: numbers (integers by value, decimals by value to 18 fractional digits,
doubles and floats by value), language tags in lower case, blank nodes as one placeholder.

A difference is explained when one of the rules below says why the standard allows both
answers, or names a known deviation; the report groups them by rule. Exit 1 if an
unexplained difference or a Jena error remains.
"""
import argparse
import re
import sys
from collections import Counter
from decimal import ROUND_DOWN, Decimal, InvalidOperation
from pathlib import Path

XSD = "http://www.w3.org/2001/XMLSchema#"
INTEGERS = {f"{XSD}{t}" for t in (
    "integer", "int", "long", "short", "byte", "nonNegativeInteger", "positiveInteger",
    "nonPositiveInteger", "negativeInteger", "unsignedLong", "unsignedInt", "unsignedShort",
    "unsignedByte")}
TYPED = re.compile(r'^"(.*)"\^\^<([^>]*)>$', re.S)
LANG = re.compile(r'^"(.*)"@([A-Za-z0-9-]+)$', re.S)
EIGHTEEN = Decimal("1e-18")


def term(text: str) -> str:
    if text.startswith("_:"):
        return "_:b"
    m = TYPED.match(text)
    if m:
        value, datatype = m.groups()
        try:
            if datatype in INTEGERS:
                return f"int:{int(value)}"
            if datatype == f"{XSD}decimal":
                return f"dec:{Decimal(value).quantize(EIGHTEEN, rounding=ROUND_DOWN).normalize()}"
            if datatype in (f"{XSD}double", f"{XSD}float"):
                return f"dbl:{float(value.replace('INF', 'inf'))!r}"
        except (ValueError, InvalidOperation):
            pass
        if datatype == f"{XSD}string":
            return f'"{value}"'
        return text
    m = LANG.match(text)
    if m:
        return f'"{m.group(1)}"@{m.group(2).lower()}'
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
}

DATE = re.compile(r'^"(-?\d{4,}-\d\d-\d\d(?:T[0-9:.]+)?)(Z|[+-]\d\d:\d\d)?"\^\^<http://www\.w3\.org/2001/XMLSchema#date(?:Time)?>$')


def mixes_timezones(row: str) -> bool:
    """Whether a row holds a date or dateTime with a timezone and one without."""
    zones = [m.group(2) is not None for m in map(DATE.match, row.split("\t")) if m]
    return any(zones) and not all(zones)


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
        answers = []
        for f in variants:
            their_vars, their_rows = table(f)
            answers.append([row(l) for l in aligned(their_vars, their_rows, our_vars)])
        if same(ours, answers[0], ordered):
            counts["agree"] += 1
            continue
        if len(answers) > 1 and same(ours, answers[1], ordered):
            counts["agree on canonical numbers"] += 1
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
