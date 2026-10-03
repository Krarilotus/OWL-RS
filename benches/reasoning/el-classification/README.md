# EL classification against ELK

Checks NRESE's OWL 2 EL classifier (`crates/nrese-reasoner/src/classify.rs`) against
ELK on random EL ontologies: conjunctions, existential restrictions, equivalences,
property hierarchies, chains, transitivity and domains. On 30 September 2026, 300 of 300
ontologies (10,540 subsumptions) agreed; a classifier with the chain rule removed
disagreed on 2 of 60, so the comparison sees a missing rule.

```bash
cargo run --release -p nrese-reasoner --example classify -- --help   # builds the example
python el_check.py generate 300 cases
for f in cases/case-*.ttl; do
  ../../../target/release/examples/classify --out "${f%.ttl}.nrese.tsv" "$f"
done
docker run --rm -v "$PWD/cases:/work" -v "$PWD/run_elk.sh:/run_elk.sh" \
  obolibrary/robot:v1.9.8 bash /run_elk.sh      # ELK's hierarchies, via ROBOT
python el_check.py compare cases
rm -rf cases
```

Ranges and disjointness are covered by the classifier's unit tests, not here (ROBOT
stops at an unsatisfiable class).
