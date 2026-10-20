#!/usr/bin/env python3
"""The answers of the fast suite's multi-graph cases from an independent engine (Oxigraph,
`pip install pyoxigraph`): `graphs4` (merged default graphs, FROM NAMED, GRAPH) and
`graphs4-reader` (the same data read by a reader restricted to two graphs), over the data
`perf_lab generate social4` writes. Prints, per query, its rows and, for a single row, its
value: what the cases pin (benches/cases/core.toml, `pinned` names this probe).

    graphs4-oracle.py DATA.nq [--readable http://example.org/fast/graph0,...]

A query's FROM graphs are merged here, not by Oxigraph: SPARQL 1.1 §13.2 makes the default graph
the RDF merge of the FROM graphs, a set, and Oxigraph 0.5.12 keeps a statement once per graph
that has it (`SELECT * FROM g0 FROM g1 { ?s ?p ?o }` over one statement in both: 2 rows). The
data has no blank nodes, so the merge is the set union of the graphs' statements, loaded as a
default graph of its own and queried without the FROM clauses. FROM NAMED and GRAPH need no
merge and run as written. A restricted reader's query runs over the dataset it may read:
FROM and FROM NAMED limited to the readable graphs, or, where it names none, FROM NAMED them.
"""
import argparse
import json
import re
import sys
from pathlib import Path

import pyoxigraph

ROOT = Path(__file__).resolve().parents[2]
QUERIES = ROOT / "benches" / "fast" / "queries"
EX = "http://example.org/fast/"
DATASET = re.compile(r"\s*FROM (NAMED )?(\S+)")


def iri(term: str) -> str:
    return term.replace("e:", EX).strip("<>")


def restricted(text: str, readable: list[str]) -> str:
    """The query over the dataset a reader of `readable` sees."""
    clauses = DATASET.findall(text)
    if clauses:
        kept = " ".join(f"FROM {named}<{iri(g)}>" for named, g in clauses if iri(g) in readable)
        return DATASET.sub("", text).replace(" WHERE ", f" {kept} WHERE ", 1)
    named = " ".join(f"FROM NAMED <{g}>" for g in readable)
    return text.replace(" WHERE ", f" {named} WHERE ", 1)


def answer(store: "pyoxigraph.Store", text: str) -> list:
    """The query's solutions, its FROM graphs merged as a set."""
    merged = [iri(g) for named, g in DATASET.findall(text) if not named]
    if not merged:
        return list(store.query(text))
    union = pyoxigraph.Store()
    for g in merged:
        union.extend(pyoxigraph.Quad(q.subject, q.predicate, q.object, pyoxigraph.DefaultGraph())
                     for q in store.quads_for_pattern(None, None, None, pyoxigraph.NamedNode(g)))
    return list(union.query(DATASET.sub("", text)))


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("data", type=Path)
    p.add_argument("--readable", default=f"{EX}graph0,{EX}graph1")
    args = p.parse_args()
    store = pyoxigraph.Store()
    with args.data.open("rb") as stream:
        store.bulk_load(stream, pyoxigraph.RdfFormat.N_QUADS)
    readable = args.readable.split(",")
    out = {}
    for case, rewrite in (("graphs4", False), ("graphs4-reader", True)):
        for path in sorted((QUERIES / case).glob("*.rq")):
            text = " ".join(path.read_text(encoding="utf-8").split())
            rows = answer(store, restricted(text, readable) if rewrite else text)
            entry = {"rows": len(rows)}
            if len(rows) == 1 and len(rows[0]) == 1:
                entry["value"] = rows[0][0].value
            out[f"{case}/{path.stem}"] = entry
    print(json.dumps({"oxigraph": pyoxigraph.__version__, "data": args.data.name, "answers": out}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
