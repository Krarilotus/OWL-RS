"""Checks a loaded SNB store's counts (pg_hops m0) against the mapping's (snb_to_rdf.py counts.json).

    python3 -I snb_check.py probes COUNTS.json             # the subjects m0 is to probe, one per line
    python3 -I snb_check.py check COUNTS.json M0.json A|B  # exit 1 on a difference; the report on stdout

What the store must hold, from the CSV's counts:
- per edge label, its edges less the parallel ones (a base triple is one statement);
- per property, the values written less those repeated in one cell (one statement each);
- `rdf:type`: a label per node and one per `type` cell; per node type, its rows;
- `rdf:reifies`: a statement per reified edge (variant A: the edges with properties; B: every
  edge), and a triple term per reified base triple;
- the probed subjects (the largest id of each node type, the subjects holding each date
  column's smallest and largest value) with those values: the same lexical form, or a
  different one of the same value, which is reported, not failed (the store may keep a
  value's canonical form; the timing policy's datatype rule decides what that means).
"""
from __future__ import annotations

import json
import re
import sys
from datetime import date, datetime

V = "http://ldbcouncil.org/snb/vocab#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
XSD = "http://www.w3.org/2001/XMLSchema#"


def expected(counts: dict, variant: str) -> dict:
    """Statements per predicate IRI, and the reified triple terms."""
    per: dict[str, int] = {}

    def add(iri: str, n: int) -> None:
        per[iri] = per.get(iri, 0) + n

    for name, node in counts["nodes"].items():
        add(RDF + "type", node["rows"] + node["triples"].get("type", 0))
        for column, n in node["triples"].items():
            if column != "type":
                add(V + column, n - node.get("repeated_values", {}).get(column, 0))
    terms = 0
    for label, e in counts["edges"].items():
        parallel = e.get("parallel", 0)
        add(V + label, e["edges"] - parallel)
        reified = e["reified_a"] if variant == "A" else e["reified_b"]
        add(RDF + "reifies", reified)
        # Parallel edges all have properties (they come from edge files), so in either variant
        # each parallel pair shares one triple term.
        terms += reified - parallel
        for column, n in e["properties"].items():
            add(V + column, n)
    return {"predicates": per, "terms": terms, "quads": sum(per.values())}


def probes(counts: dict) -> list[tuple[str, str, str]]:
    """(subject, predicate, value) the store must hold: each node type's largest id, and the
    holders of each date column's extremes."""
    out = []
    for node in counts["nodes"].values():
        if "max_id_iri" in node:
            out.append((node["max_id_iri"], V + "id", str(node["max_id"])))
    extremes = [counts["extremes"]] + [e.get("extremes", {}) for e in counts["edges"].values()]
    for group in extremes:
        for column, e in group.items():
            for value, subject in (e["min"], e["max"]):
                out.append((subject, V + column, value))
    return out


LITERAL = re.compile(r'^"(.*)"(?:\^\^<([^>]*)>)?$')


def same_value(lexical: str, stored: str, datatype: str | None) -> bool:
    """Whether two lexical forms of the datatype name the same value."""
    try:
        if datatype == XSD + "dateTime":
            return datetime.fromisoformat(lexical) == datetime.fromisoformat(stored)
        if datatype == XSD + "date":
            return date.fromisoformat(lexical) == date.fromisoformat(stored)
        if datatype in (XSD + "long", XSD + "int", XSD + "integer"):
            return int(lexical) == int(stored)
    except ValueError:
        return False
    return False


def check(counts: dict, m0: dict, variant: str) -> tuple[list[str], list[str]]:
    """The differences (each fails the check), and the notes (reported)."""
    want = expected(counts, variant)
    bad, notes = [], []
    got = {k.strip("<>"): v for k, v in m0["predicates"].items()}
    for iri in sorted(set(want["predicates"]) | set(got)):
        if want["predicates"].get(iri, 0) != got.get(iri, 0):
            bad.append(f"{iri}: expected {want['predicates'].get(iri, 0)}, stored {got.get(iri, 0)}")
    if m0["quads"] != want["quads"]:
        bad.append(f"quads: expected {want['quads']}, stored {m0['quads']}")
    if m0["reified_triple_terms"] != want["terms"]:
        bad.append(f"reified triple terms: expected {want['terms']}, stored {m0['reified_triple_terms']}")
    classes = {k.strip("<>"): v for k, v in m0["classes"].items()}
    for name, node in counts["nodes"].items():
        if classes.get(V + name, 0) != node["rows"]:
            bad.append(f"snb:{name}: expected {node['rows']} nodes, stored {classes.get(V + name, 0)}")
    for subject, predicate, value in probes(counts):
        statements = m0["probes"].get(subject)
        if statements is None:
            bad.append(f"{subject}: not probed")
            continue
        held = [s.split(" ", 1)[1] for s in statements if s.startswith(f"<{predicate}> ")]
        found = [LITERAL.match(h) for h in held]
        found = [(m.group(1), m.group(2)) for m in found if m]
        if any(lexical == value for lexical, _ in found):
            continue
        same = [(lexical, dt) for lexical, dt in found if same_value(value, lexical, dt)]
        if same:
            notes.append(f"{subject} {predicate}: {value!r} stored as {same[0][0]!r} (the same value)")
        else:
            bad.append(f"{subject} {predicate}: {value!r} not stored (it has {held})")
    return bad, notes


def main(argv: list[str]) -> int:
    counts = json.loads(open(argv[2], encoding="utf-8").read())
    if argv[1] == "probes":
        for subject in sorted({s for s, _, _ in probes(counts)}):
            print(subject)
        return 0
    m0 = json.loads(open(argv[3], encoding="utf-8").read())
    bad, notes = check(counts, m0, argv[4])
    for line in bad:
        print("DIFFERS", line)
    for line in notes:
        print("NOTE", line)
    print(f"{'FAILED' if bad else 'PASSED'}: {len(bad)} differences, {len(notes)} notes")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
