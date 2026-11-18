# Glossary

Standard terms mean what their standard says. Statement (triple), quad, IRI, literal, blank node,
graph and dataset are defined in [RDF 1.1 Concepts](https://www.w3.org/TR/rdf11-concepts/). Class,
property, individual and ontology are introduced in the [OWL 2 Primer](https://www.w3.org/TR/owl2-primer/),
and shapes in [SHACL](https://www.w3.org/TR/shacl/). The words below are NRESE's own, or are used
here in a narrower sense.

### Graph

A repository holds one default graph and any number of named graphs, as an RDF dataset does. A
query without `GRAPH` or `FROM` reads the default graph, or every graph with
`store.default_graph = "union"` ([configuration](ops/config-reference.md#store)).

### Asserted, inferred

Asserted statements are the ones written. Inferred ones follow from them under the reasoning
mode. NRESE stores them apart and keeps the inferred ones current with every commit, and
`infer=false` reads the asserted ones only.

### Reasoning mode

The set of rules that derives the inferred statements, from `rdfs` up to `owl2-dl`
([modes](spec/reasoning-semantics.md#modes)). `owl2-ql`, `owl2-rl` and `owl2-dl` are named after
the [OWL 2 profiles](https://www.w3.org/TR/owl2-profiles/) and OWL 2 DL.

### Commit, revision

A commit is one write (a SPARQL update, a Graph Store write or a client transaction's commit),
applied whole or not at all, with its inferences and checks
([what a commit does](what-it-covers.md#what-a-commit-does)). Each commit makes a new revision,
and a query reads one revision as a snapshot.

### Completeness status

The `NRESE-Completeness` header on every SPARQL answer, which says whether rows may be missing
or wrong ([what an answer promises](what-it-covers.md#what-an-answer-promises)).

### Shape, shapes graph, SHACL gate

Shapes are stored in the shapes graph, by default
`http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` (`shacl.shapes_graph`). With the gate on
(`shacl.gate`, `report` or `enforce`), every commit is validated against them.

### Repository

One dataset with its own reasoning. A server holds one or more, and the default one is `nrese`,
the one `/dataset/…` serves ([repositories](ops/http-api.md#engine-api-apiv1)).

### Deployment posture

What a server exposes, chosen at start with
[`server.deployment_posture`](ops/config-reference.md#server.deployment_posture):
`open-workbench` (the default), `read-only-demo`, `internal-authenticated` or
`replacement-grade`.
