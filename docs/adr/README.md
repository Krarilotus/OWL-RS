# Architecture Decision Records

One file per decision that is expensive to reverse. Format: context → decision → consequences. Status is `accepted`, `superseded by NNNN`, or `proposed`.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-own-engine-reuse-oxigraph-parsers.md) | Own storage engine; reuse Oxigraph's parsers and SPARQL evaluator | accepted |
| [0002](0002-engine-storage-lsm-permutations.md) | Dictionary encoding + LSM runs over six quad permutations + WAL | accepted |
| [0003](0003-materialised-reasoning.md) | Materialised, incrementally maintained reasoning with asserted/inferred separation | accepted |
| [0004](0004-parity-targets-qlever-graphdb.md) | Parity targets are QLever (performance) and GraphDB (semantics), not Fuseki | accepted |
| [0005](0005-builtin-shacl.md) | SHACL validation is built in and runs as a commit gate | accepted |
| [0006](0006-full-text-search-and-researchspace.md) | Built-in full-text search; ResearchSpace is adapted to NRESE | accepted |
| [0007](0007-one-engine-api.md) | One engine API; protocols and the frontend translate to it | accepted |
| [0008](0008-users-workspaces-policies.md) | Users, workspaces and graph policies in the store | accepted |
| [0009](0009-owl2-dl-reasoning.md) | OWL 2 DL reasoning: one OWL model, hypertableau, consequence-based classification, certain answers with datalog bounds | proposed |
| [0010](0010-standing-product-decisions.md) | Standing product decisions: reasoning leads, configurable defaults, one execution core, Rust natively, no C/C++ speed dependencies, licence | accepted |
| [0011](0011-equality-one-contract-three-backends.md) | Equality: one contract (reasons, deltas, owner domains), backends per lifetime (monotone, persistent, rollback) | proposed |
