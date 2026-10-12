#!/usr/bin/env python3
"""G-CARE's labelled graphs as RDF, its query graphs as SPARQL (prepare.sh gcare).

    python3 -I convert.py GCARE-DIR DATA-DIR QUERIES-DIR

GCARE-DIR holds the published archives unpacked (`dataset/`, `queryset/`,
`true_cardinality/`); DATA-DIR gets `gcare-NAME.nt` per data graph; QUERIES-DIR (the suite's
benches/competitors/queries) gets `gcare-NAME/TOPOLOGY_SIZE-FILE.rq` per query graph and
`gcare-NAME/truth.tsv` with the published true cardinality of each.

The mapping keeps G-CARE's semantics, homomorphisms of the query graph into the data graph,
which is what a SPARQL basic graph pattern counts under bag semantics:
- data vertex `v ID L1 L2 ...`: `<http://gcare/v/ID> rdf:type <http://gcare/l/L>` per label
  (every vertex has one, the default 0 included, so every vertex exists);
- data edge `e A B L`: `<http://gcare/v/A> <http://gcare/e/L> <http://gcare/v/B>`;
- query vertex `v ID L DVID`: the variable `?vID`, typed `<http://gcare/l/L>` unless L is
  -1, and the constant `<http://gcare/v/DVID>` instead of the variable when DVID isn't -1;
- query edge `e A B L`: the triple pattern `?vA <http://gcare/e/L> ?vB`.
A query is `SELECT (COUNT(*) AS ?count)` of its pattern: the planner's estimate of the
pattern is read with `explain=plan` (estimates.py), its count is the truth. RDF has no
duplicate statements: a data graph with a repeated edge (YAGO has two) can count fewer
homomorphisms than G-CARE's; the qualification run shows which queries that touches.
"""
from __future__ import annotations

import sys
from pathlib import Path

V, E, L = "http://gcare/v/", "http://gcare/e/", "http://gcare/l/"
TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
GRAPHS = ["aids", "human", "lubm80", "yago"]


def data(source: Path, target: Path) -> int:
    seen: set[tuple[str, str, str]] = set()
    n = 0
    with open(source, encoding="utf-8") as f, open(target.with_suffix(".nt.part"), "w", encoding="utf-8", newline="\n") as out:
        for line in f:
            p = line.split()
            if not p or p[0] == "t":
                continue
            if p[0] == "v":
                for label in p[2:] or ["0"]:
                    out.write(f"<{V}{p[1]}> <{TYPE}> <{L}{label}> .\n")
                    n += 1
            elif p[0] == "e":
                key = (p[1], p[2], p[3])
                if key in seen:
                    continue
                seen.add(key)
                out.write(f"<{V}{p[1]}> <{E}{p[3]}> <{V}{p[2]}> .\n")
                n += 1
    target.with_suffix(".nt.part").replace(target)
    return n


def query(source: Path) -> str:
    vertices: dict[str, str] = {}
    patterns: list[str] = []
    for line in source.read_text(encoding="utf-8").splitlines():
        p = line.split()
        if not p or p[0] == "t":
            continue
        if p[0] == "v":
            vid, label, dvid = p[1], p[2], p[3] if len(p) > 3 else "-1"
            term = f"<{V}{dvid}>" if dvid != "-1" else f"?v{vid}"
            vertices[vid] = term
            if label != "-1":
                patterns.append(f"{term} <{TYPE}> <{L}{label}> .")
        elif p[0] == "e":
            patterns.append(f"{vertices[p[1]]} <{E}{p[3]}> {vertices[p[2]]} .")
    return "SELECT (COUNT(*) AS ?count) WHERE {\n  " + "\n  ".join(patterns) + "\n}\n"


def write(path: Path, text: str):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(__doc__)
        return 2
    root, data_dir, queries_dir = (Path(a) for a in argv[1:])
    for g in GRAPHS:
        target = data_dir / f"gcare-{g}.nt"
        if not target.exists():
            print(f"gcare-{g}.nt: {data(root / 'dataset' / g / f'{g}.txt', target)} statements")
        out = queries_dir / f"gcare-{g}"
        out.mkdir(parents=True, exist_ok=True)
        truth = ["# query\ttrue cardinality (G-CARE, SIGMOD 2020: true_cardinality.tar.gz)"]
        for q in sorted((root / "queryset" / g).rglob("*.txt")):
            rel = q.relative_to(root / "queryset" / g)
            name = "-".join([*rel.parent.parts, rel.stem])
            write(out / f"{name}.rq", f"# G-CARE {g}: {rel.as_posix()}\n" + query(q))
            t = root / "true_cardinality" / g / rel
            truth.append(f"{name}\t{t.read_text(encoding='utf-8').split()[0] if t.exists() else '-'}")
        write(out / "truth.tsv", "\n".join(truth) + "\n")
        print(f"gcare-{g}: {len(truth) - 1} queries")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
