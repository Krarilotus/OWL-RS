#!/usr/bin/env python3
"""Where a materialised closure's inferred facts come from (merge checklist §2, item 1):
the share a store could leave out by answering it from the schema instead of storing it.

    python benches/probes/provenance-split.py ASSERTED.nt CLOSURE.nt

ASSERTED is the input (ontology and data), CLOSURE the same plus everything the reasoner
derived (e.g. `lubm-100.nt` and `lubm-100-materialised.nt` from
benches/reasoning/prepare-lubm-materialised.sh). Every inferred fact is put in the first
class that applies:

- `type, inherited`: `x a C` where `x` has another type `D` with `D ⊑ C` strictly in the
  closure's own class hierarchy (equivalent classes: all but the least kept). A store that
  answers types from the hierarchy needs only the others.
- `type, rule-produced`: every other inferred type (domains, ranges, intersections, ...).
- `property, sub-property`: `x p y` where `x q y` holds for some `q ⊑ p`, `q ≠ p`.
- `property, inverse`: `x p y` where `y q x` holds for an inverse `q` of `p`.
- `property, transitive`: `x p y` for a transitive `p` with some `x p m`, `m p y`.
- `property, other`; `schema`: inferred triples about classes and properties.

The hierarchy is read from the closure (OWL 2 RL closes `rdfs:subClassOf` and
`rdfs:subPropertyOf`, intersections included), so no ontology parser is needed. Prints counts
and shares; memory follows facts in NRESE's inferred stack (every fact costs the same in its
permutations), so the shares are the stack's.
"""
from __future__ import annotations

import re
import sys
from collections import defaultdict

RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
SUB_CLASS = "<http://www.w3.org/2000/01/rdf-schema#subClassOf>"
SUB_PROPERTY = "<http://www.w3.org/2000/01/rdf-schema#subPropertyOf>"
INVERSE = "<http://www.w3.org/2002/07/owl#inverseOf>"
TRANSITIVE = "<http://www.w3.org/2002/07/owl#TransitiveProperty>"
SYMMETRIC = "<http://www.w3.org/2002/07/owl#SymmetricProperty>"
SCHEMA_PREDICATES = {
    SUB_CLASS, SUB_PROPERTY, INVERSE,
    "<http://www.w3.org/2002/07/owl#equivalentClass>",
    "<http://www.w3.org/2002/07/owl#equivalentProperty>",
    "<http://www.w3.org/2000/01/rdf-schema#domain>",
    "<http://www.w3.org/2000/01/rdf-schema#range>",
    "<http://www.w3.org/2002/07/owl#disjointWith>",
}
LINE = re.compile(r'^(\S+)\s+(\S+)\s+(.+?)\s*\.\s*$')

ids: dict[str, int] = {}
names: list[str] = []


def intern(term: str) -> int:
    i = ids.get(term)
    if i is None:
        i = len(names)
        ids[term] = i
        names.append(term)
    return i


def read(path: str) -> set[tuple[int, int, int]]:
    facts = set()
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = LINE.match(line)
            if m:
                facts.add((intern(m.group(1)), intern(m.group(2)), intern(m.group(3))))
    return facts


def main(asserted_path: str, closure_path: str) -> None:
    asserted = read(asserted_path)
    closure = read(closure_path)
    inferred = closure - asserted
    t, sc, sp, inv, tr, sym = (intern(x) for x in (RDF_TYPE, SUB_CLASS, SUB_PROPERTY, INVERSE, TRANSITIVE, SYMMETRIC))
    schema_preds = {intern(p) for p in SCHEMA_PREDICATES}
    # The closure's hierarchy (already transitive under OWL 2 RL).
    supers: dict[int, set[int]] = defaultdict(set)
    sub_props: dict[int, set[int]] = defaultdict(set)
    inverses: dict[int, set[int]] = defaultdict(set)
    transitive: set[int] = set()
    for s, p, o in closure:
        if p == sc and s != o:
            supers[s].add(o)
        elif p == sp and s != o:
            sub_props[o].add(s)
        elif p == inv:
            inverses[s].add(o)
            inverses[o].add(s)
        elif p == t and o == tr:
            transitive.add(s)
        elif p == t and o == sym:
            inverses[s].add(s)
    classes = set(supers) | {c for cs in supers.values() for c in cs}
    # Types per subject; facts per (predicate) for the property checks.
    types: dict[int, set[int]] = defaultdict(set)
    out_edges: dict[tuple[int, int], set[int]] = defaultdict(set)
    for s, p, o in closure:
        if p == t:
            types[s].add(o)
        else:
            out_edges[(p, s)].add(o)

    def has(s: int, p: int, o: int) -> bool:
        return o in out_edges.get((p, s), ())

    counts: dict[str, int] = defaultdict(int)
    for s, p, o in inferred:
        if p in schema_preds or (p == t and (o in (tr, sym) or s in classes)):
            counts["schema"] += 1
        elif p == t:
            others = types[s] - {o}
            # Strictly below: D ⊑ C and not C ⊑ D (an equivalent keeps the least id).
            below = any(o in supers.get(d, ()) and (d not in supers.get(o, ()) or d < o) for d in others)
            counts["type, inherited" if below else "type, rule-produced"] += 1
        elif any(has(s, q, o) for q in sub_props.get(p, ())):
            counts["property, sub-property"] += 1
        elif any(has(o, q, s) for q in inverses.get(p, ())):
            counts["property, inverse"] += 1
        elif p in transitive and any(has(m, p, o) for m in out_edges.get((p, s), ())):
            counts["property, transitive"] += 1
        else:
            counts["property, other"] += 1
    total = len(inferred)
    print(f"asserted {len(asserted):,} facts, closure {len(closure):,}, inferred {total:,}")
    for kind in ("type, inherited", "type, rule-produced", "property, sub-property",
                 "property, inverse", "property, transitive", "property, other", "schema"):
        n = counts.get(kind, 0)
        print(f"  {kind:24} {n:>12,}  {100 * n / max(total, 1):5.1f} % of inferred, "
              f"{100 * n / max(len(closure), 1):5.1f} % of all stored")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
