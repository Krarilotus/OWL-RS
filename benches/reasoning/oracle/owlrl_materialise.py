"""OWL 2 RL (or RDFS) closure with owlrl, the reference implementation, as a correctness oracle.

    owlrl_materialise.py --profile owl2-rl|rdfs --out inferred.nt [--queries DIR] input.nt...

Writes the *inferred* triples (closure minus input) as sorted N-Triples, so other systems'
inferred sets can be diffed against it, and prints the answer count of every query in DIR
over the closure.
"""

import argparse
import sys
import time
from pathlib import Path

import owlrl
import pyoxigraph
from rdflib import Graph, Literal


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", choices=["owl2-rl", "rdfs"], default="owl2-rl")
    parser.add_argument("--out", required=True)
    parser.add_argument("--queries")
    parser.add_argument("inputs", nargs="+")
    args = parser.parse_args()

    graph = Graph()
    for path in args.inputs:
        graph.parse(path, format="nt")
    asserted = set(graph)
    print(f"asserted: {len(asserted)} triples", file=sys.stderr)

    semantics = owlrl.OWLRL_Semantics if args.profile == "owl2-rl" else owlrl.RDFS_Semantics
    started = time.perf_counter()
    owlrl.DeductiveClosure(semantics, axiomatic_triples=False, datatype_axioms=False).expand(graph)
    seconds = time.perf_counter() - started
    inferred = set(graph) - asserted
    print(f"closure ({args.profile}): {len(inferred)} inferred triples in {seconds:.1f} s",
          file=sys.stderr)

    lines = sorted(f"{s.n3()} {p.n3()} {o.n3()} ." for s, p, o in inferred)
    Path(args.out).write_text("\n".join(lines) + "\n", encoding="utf-8")

    if args.queries:
        # The closure is owlrl's; only query evaluation is delegated, to Oxigraph (W3C
        # conformant). rdflib's SPARQL engine takes hours on LUBM's multi-way joins.
        # Literal subjects (generalised RDF) aren't valid N-Triples.
        closure = Graph()
        for triple in graph:
            if not isinstance(triple[0], Literal):
                closure.add(triple)
        store = pyoxigraph.Store()
        store.bulk_load(closure.serialize(format="nt", encoding="utf-8"), pyoxigraph.RdfFormat.N_TRIPLES)
        for query in sorted(Path(args.queries).glob("*.rq")):
            rows = len(list(store.query(query.read_text(encoding="utf-8"))))
            print(f"{query.stem}\t{rows}")


if __name__ == "__main__":
    main()
