#!/usr/bin/env bash
# Inside the ROBOT container: ELK's class hierarchy of every /work/case-*.ttl.
set -u
cat > /tmp/hierarchy.rq <<'Q'
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
SELECT ?a ?b ?kind WHERE {
  { ?a rdfs:subClassOf ?b BIND("sub" AS ?kind) }
  UNION { ?a owl:equivalentClass ?b BIND("eq" AS ?kind) }
  FILTER(isIRI(?a) && isIRI(?b))
}
Q
for f in /work/case-*.ttl; do
  base="${f%.ttl}"
  robot reason --reasoner ELK --input "$f" --axiom-generators "SubClass EquivalentClass" \
    --include-indirect true --remove-redundant-subclass-axioms false --output "$base.elk.ttl" >/dev/null 2>"$base.elk.log" \
    && robot query --input "$base.elk.ttl" --query /tmp/hierarchy.rq "$base.elk.tsv" >/dev/null 2>>"$base.elk.log"
done
