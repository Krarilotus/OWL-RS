"""Compares a system's inferred triples with an oracle's (RT1 correctness).

    python compare_inferred.py oracle.nt system.nt [--label NAME] [--json out.json]

Reports how many of the system's inferences the oracle also derives (soundness, precision),
and how many of the oracle's the system derives (completeness, recall), per predicate.

**Normalisation**, applied to both sides because reasoners differ in these trivial facts:
- `x owl:sameAs x`, `C rdfs:subClassOf C`, `p rdfs:subPropertyOf p`, and the reflexive
  equivalence axioms: tautologies (OWL 2 RL eq-ref, scm-cls, scm-op/dp)
- `x rdf:type rdfs:Resource | owl:Thing | rdfs:Literal`, `C rdfs:subClassOf owl:Thing`,
  `owl:Nothing rdfs:subClassOf C`: true of everything, never queried; GraphDB's
  "optimized" rulesets drop them too
- `C rdf:type rdfs:Class | owl:Class | rdf:Property | rdfs:Datatype` and
  `C rdfs:subClassOf rdfs:Resource`, `p rdfs:domain | rdfs:range rdfs:Resource`: meta-level typing. RDFS entails it, OWL 2 RL/RDF doesn't
  include it, so it differs by profile, not by correctness
- triples with a literal subject: generalised RDF, not valid RDF
- triples with blank nodes: labels aren't comparable across systems

Everything else must match exactly. The normalisation is part of the benchmark definition
(docs/design/reasoning-benchmark.md §4); changing it changes every published comparison.
"""

import argparse
import collections
import json
import re
import sys

RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
SAME_AS = "<http://www.w3.org/2002/07/owl#sameAs>"
REFLEXIVE = {
    SAME_AS,
    "<http://www.w3.org/2000/01/rdf-schema#subClassOf>",
    "<http://www.w3.org/2000/01/rdf-schema#subPropertyOf>",
    "<http://www.w3.org/2002/07/owl#equivalentClass>",
    "<http://www.w3.org/2002/07/owl#equivalentProperty>",
}
TRIVIAL_TYPES = {
    "<http://www.w3.org/2000/01/rdf-schema#Resource>",
    "<http://www.w3.org/2002/07/owl#Thing>",
    "<http://www.w3.org/2000/01/rdf-schema#Literal>",
    # meta-level typing (profile-dependent)
    "<http://www.w3.org/2000/01/rdf-schema#Class>",
    "<http://www.w3.org/2002/07/owl#Class>",
    "<http://www.w3.org/1999/02/22-rdf-syntax-ns#Property>",
    "<http://www.w3.org/2000/01/rdf-schema#Datatype>",
}
SUB_CLASS = "<http://www.w3.org/2000/01/rdf-schema#subClassOf>"
THING = "<http://www.w3.org/2002/07/owl#Thing>"
RESOURCE = "<http://www.w3.org/2000/01/rdf-schema#Resource>"
DOMAIN = "<http://www.w3.org/2000/01/rdf-schema#domain>"
RANGE = "<http://www.w3.org/2000/01/rdf-schema#range>"
NOTHING = "<http://www.w3.org/2002/07/owl#Nothing>"

# subject, predicate, rest-of-line object (IRIs, blank nodes or literals with escapes)
_TRIPLE = re.compile(r'^(<[^>]*>|_:\S+|"(?:[^"\\]|\\.)*"\S*)\s+(<[^>]*>)\s+(.+?)\s*\.\s*$')


def normalised(path: str) -> set[tuple[str, str, str]]:
    triples = set()
    with open(path, encoding="utf-8", errors="surrogateescape") as lines:
        for line in lines:
            match = _TRIPLE.match(line)
            if not match:
                continue
            s, p, o = match.groups()
            if s.startswith('"') or s.startswith("_:") or o.startswith("_:"):
                continue
            if p in REFLEXIVE and s == o:
                continue
            if p == RDF_TYPE and o in TRIVIAL_TYPES:
                continue
            if p == SUB_CLASS and (o in (THING, RESOURCE) or s == NOTHING):
                continue
            if p in (DOMAIN, RANGE) and o == RESOURCE:
                continue
            triples.add((s, p, o))
    return triples


def by_predicate(triples, top=8):
    return collections.Counter(p for _, p, _ in triples).most_common(top)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("oracle")
    parser.add_argument("system")
    parser.add_argument("--label", default="system")
    parser.add_argument("--json")
    args = parser.parse_args()
    oracle, system = normalised(args.oracle), normalised(args.system)
    common = oracle & system
    extra, missing = system - oracle, oracle - system
    report = {
        "label": args.label,
        "oracle": len(oracle),
        "system": len(system),
        "common": len(common),
        "precision": len(common) / len(system) if system else 1.0,
        "recall": len(common) / len(oracle) if oracle else 1.0,
        "not_in_oracle_by_predicate": by_predicate(extra),
        "missing_by_predicate": by_predicate(missing),
        "not_in_oracle_sample": sorted(" ".join(t) for t in extra)[:5],
        "missing_sample": sorted(" ".join(t) for t in missing)[:5],
    }
    print(
        f"{args.label}: {report['system']} inferred, oracle {report['oracle']}, common "
        f"{report['common']} | precision {report['precision']:.4f} | recall {report['recall']:.4f}"
    )
    for key in ("not_in_oracle_by_predicate", "missing_by_predicate"):
        if report[key]:
            print(f"  {key}: {report[key]}")
    for key in ("not_in_oracle_sample", "missing_sample"):
        for triple in report[key][:3]:
            print(f"  {key}: {triple}")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as out:
            json.dump(report, out, indent=2)


if __name__ == "__main__":
    sys.exit(main())
