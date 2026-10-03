# Client kits

Real clients against NRESE, end to end: the connectors' contract.

| Kit | What it exercises |
|---|---|
| `rdf4j/` | RDF4J's Java client: `HTTPRepository`, `RemoteRepositoryManager`, statements, contexts, transactions. This is what GraphDB tooling, ResearchSpace and other RDF4J applications do. It runs once against the store's default graph and once against the union. |
| `jena/` | Apache Jena 6.2's `RDFConnection`: updates; SELECT, ASK, CONSTRUCT and DESCRIBE; results in XML, JSON and TSV; literals (language tags, datatypes, non-ASCII text); the Graph Store Protocol (PUT, GET, POST, DELETE). |
| `rdflib/` | rdflib's `SPARQLUpdateStore` under a `Graph` and a `Dataset`: writes, queries, the default graph by rdflib's own name, named graphs, and the Graph Store Protocol over plain HTTP. |
| `dmw` | The Datamodel Workflow's export, questions, updates and validation (`scripts/smoke-dmw-export.sh`). |

```bash
cargo build --release -p nrese-server
benches/clients/run-all.sh [target/release/nrese-server] [rdf4j|jena|rdflib|dmw ...]
```

Each kit gets a fresh in-memory server of its own. The kits need Java 21 or later and
Maven, which is downloaded to `TOOLS` if missing; Python 3 with rdflib; and curl. The run
prints a summary and exits 1 if a kit failed.
