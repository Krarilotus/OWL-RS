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
| [0007](0007-one-engine-api.md) | One engine API; protocols and the frontend translate to it | proposed |
| [0008](0008-users-workspaces-policies.md) | Users, workspaces and graph policies in the store | proposed |
