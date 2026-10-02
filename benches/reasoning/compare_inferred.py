"""Compares a system's inferred triples with an oracle's (RT1 correctness).

    python compare_inferred.py oracle.nt system.nt [--input asserted.nt...] [--label NAME] [--json out.json]

Reports how many of the system's inferences the oracle also derives (soundness, precision),
and how many of the oracle's the system derives (completeness, recall), per predicate.

**Two levels, reported separately:**
- **instance**: facts about individuals. This is the headline: it's what queries see.
- **schema**: facts about classes and properties (subClassOf, domain, inverseOf, ..., and
  typing with RDF/RDFS/OWL vocabulary). Reasoners legitimately differ here by profile, e.g.
  Jena states `inverseOf` and `disjointWith` symmetrically, and OWL 2 RL/RDF doesn't.

**Normalisation**, applied to both sides because reasoners differ in these trivial facts:
- `x owl:sameAs x`, `C rdfs:subClassOf C`, `p rdfs:subPropertyOf p`, and the reflexive
  equivalence axioms: tautologies (OWL 2 RL eq-ref, scm-cls, scm-op/dp)
- `x rdf:type rdfs:Resource | owl:Thing | rdfs:Literal`, `C rdfs:subClassOf owl:Thing`,
  `owl:Nothing rdfs:subClassOf C`, `p rdfs:domain | rdfs:range rdfs:Resource | owl:Thing`:
  true of everything and never queried. GraphDB's "optimized" rulesets drop them too.
- triples whose subject is RDF/RDFS/OWL/XSD vocabulary (`rdf:Bag rdfs:subClassOf
  rdfs:Container`, `xsd:Name rdf:type rdfs:Datatype`): axiomatic triples, which systems
  emit to different extents whatever their settings
- triples with a literal subject: generalised RDF, not valid RDF
- triples with blank nodes: labels aren't comparable across systems

**Entailed, but outside the OWL 2 RL/RDF rules.** With `--input`, a system's extra instance
facts are checked against two structural entailments, and counted as *explained* rather than
as errors:
- `a owl:differentFrom b` where a and b are listed in the same `owl:AllDifferent` (RL only
  checks it for consistency: eq-diff2/3)
- `x p x` where p is an `owl:ReflexiveProperty` (not in RL; true of every x under the
  RDF-Based Semantics)
- `a owl:differentFrom b` where a and b have types (asserted, or inferred by the reference)
  that are disjoint (`owl:disjointWith`, `owl:AllDisjointClasses`); RL only checks it for
  consistency (cax-dw, cax-adc)

Precision counts explained facts as correct; the report gives their number separately.

Everything else must match exactly. The normalisation is part of the benchmark definition
(docs/design/reasoning-benchmark.md §4); changing it changes every published comparison.
"""

import argparse
import collections
import json
import re
import sys

RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
OWL = "http://www.w3.org/2002/07/owl#"
XSD = "http://www.w3.org/2001/XMLSchema#"
VOCABULARY = (f"<{RDF}", f"<{RDFS}", f"<{OWL}", f"<{XSD}")

RDF_TYPE = f"<{RDF}type>"
SUB_CLASS = f"<{RDFS}subClassOf>"
DOMAIN = f"<{RDFS}domain>"
RANGE = f"<{RDFS}range>"
THING = f"<{OWL}Thing>"
RESOURCE = f"<{RDFS}Resource>"
NOTHING = f"<{OWL}Nothing>"
DIFFERENT = f"<{OWL}differentFrom>"
REFLEXIVE = {
    f"<{OWL}sameAs>",
    SUB_CLASS,
    f"<{RDFS}subPropertyOf>",
    f"<{OWL}equivalentClass>",
    f"<{OWL}equivalentProperty>",
}
TRIVIAL_TYPES = {RESOURCE, THING, f"<{RDFS}Literal>"}
SCHEMA_PREDICATES = {
    SUB_CLASS,
    f"<{RDFS}subPropertyOf>",
    DOMAIN,
    RANGE,
    f"<{OWL}equivalentClass>",
    f"<{OWL}equivalentProperty>",
    f"<{OWL}inverseOf>",
    f"<{OWL}disjointWith>",
    f"<{OWL}propertyDisjointWith>",
    f"<{OWL}complementOf>",
}

# subject, predicate, rest-of-line object (IRIs, blank nodes or literals with escapes)
_TRIPLE = re.compile(r'^(<[^>]*>|_:\S+|"(?:[^"\\]|\\.)*"\S*)\s+(<[^>]*>)\s+(.+?)\s*\.\s*$')


def triples(path: str):
    with open(path, encoding="utf-8", errors="surrogateescape") as lines:
        for line in lines:
            match = _TRIPLE.match(line)
            if match:
                yield match.groups()


def normalised(path: str) -> set[tuple[str, str, str]]:
    kept = set()
    for s, p, o in triples(path):
        if s.startswith('"') or s.startswith("_:") or o.startswith("_:"):
            continue
        if s.startswith(VOCABULARY):
            continue
        if p in REFLEXIVE and s == o:
            continue
        if p == RDF_TYPE and o in TRIVIAL_TYPES:
            continue
        if p == SUB_CLASS and (o in (THING, RESOURCE) or s == NOTHING):
            continue
        if p in (DOMAIN, RANGE) and o in (THING, RESOURCE):
            continue
        kept.add((s, p, o))
    return kept


def is_schema(triple) -> bool:
    _, p, o = triple
    return p in SCHEMA_PREDICATES or (p == RDF_TYPE and o.startswith(VOCABULARY))


class Entailments:
    """The structural entailments outside OWL 2 RL/RDF.

    Read from the asserted input, plus the reference's inferred types (for disjointness).
    """

    def __init__(self, inputs, reference):
        self.reflexive = set()
        self.all_different = []
        self.disjoint = set()  # (C1, C2) pairs, both directions
        self.types = collections.defaultdict(set)
        first, rest, lists = {}, {}, []  # lists: (subject, predicate, list head)
        kinds = collections.defaultdict(set)
        for path in inputs:
            for s, p, o in triples(path):
                if p == RDF_TYPE:
                    self.types[s].add(o)
                    kinds[s].add(o)
                    if o == f"<{OWL}ReflexiveProperty>":
                        self.reflexive.add(s)
                elif p == f"<{RDF}first>":
                    first[s] = o
                elif p == f"<{RDF}rest>":
                    rest[s] = o
                elif p in (f"<{OWL}distinctMembers>", f"<{OWL}members>"):
                    lists.append((s, p, o))
                elif p == f"<{OWL}disjointWith>":
                    self.disjoint |= {(s, o), (o, s)}
        for s, p, o in triples(reference):
            if p == RDF_TYPE:
                self.types[s].add(o)
        for subject, predicate, head in lists:
            members, node = [], head
            while node in first:
                members.append(first[node])
                node = rest.get(node)
            if predicate == f"<{OWL}distinctMembers>" or f"<{OWL}AllDifferent>" in kinds[subject]:
                self.all_different.append(set(members))
            elif f"<{OWL}AllDisjointClasses>" in kinds[subject]:
                self.disjoint |= {(a, b) for a in members for b in members if a != b}

    def explains(self, triple) -> bool:
        s, p, o = triple
        if s == o and p in self.reflexive:
            return True
        if p == DIFFERENT and s != o:
            if any(s in members and o in members for members in self.all_different):
                return True
            return any((a, b) in self.disjoint for a in self.types[s] for b in self.types[o])
        return False


def level_report(oracle, system, entailments):
    common = oracle & system
    extra, missing = system - oracle, oracle - system
    explained = {t for t in extra if entailments and entailments.explains(t)}
    unexplained = extra - explained
    return {
        "oracle": len(oracle),
        "system": len(system),
        "common": len(common),
        "explained": len(explained),
        "precision": (len(common) + len(explained)) / len(system) if system else 1.0,
        "recall": len(common) / len(oracle) if oracle else 1.0,
        "not_in_oracle_by_predicate": by_predicate(unexplained),
        "missing_by_predicate": by_predicate(missing),
        "not_in_oracle_sample": sorted(" ".join(t) for t in unexplained)[:5],
        "missing_sample": sorted(" ".join(t) for t in missing)[:5],
    }


def by_predicate(facts, top=8):
    return collections.Counter(p for _, p, _ in facts).most_common(top)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("oracle")
    parser.add_argument("system")
    parser.add_argument("--input", nargs="*", default=[])
    parser.add_argument("--label", default="system")
    parser.add_argument("--json")
    args = parser.parse_args()
    oracle, system = normalised(args.oracle), normalised(args.system)
    entailments = Entailments(args.input, args.oracle) if args.input else None
    report = {"label": args.label}
    for level, keep in (("instance", lambda t: not is_schema(t)), ("schema", is_schema)):
        report[level] = level_report(
            {t for t in oracle if keep(t)}, {t for t in system if keep(t)}, entailments
        )
    instance, schema = report["instance"], report["schema"]
    print(
        f"{args.label}: instance {instance['system']} inferred, oracle {instance['oracle']}, "
        f"common {instance['common']}, explained {instance['explained']} | "
        f"precision {instance['precision']:.4f} | recall {instance['recall']:.4f} || "
        f"schema precision {schema['precision']:.4f} recall {schema['recall']:.4f}"
    )
    for level in ("instance", "schema"):
        for key in ("not_in_oracle_by_predicate", "missing_by_predicate"):
            if report[level][key]:
                print(f"  {level} {key}: {report[level][key]}")
        for key in ("not_in_oracle_sample", "missing_sample"):
            for fact in report[level][key][:3]:
                print(f"  {level} {key}: {fact}")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as out:
            json.dump(report, out, indent=2)


if __name__ == "__main__":
    sys.exit(main())
