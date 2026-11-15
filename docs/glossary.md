# Words used in these pages

These are short meanings of the words, as these pages use them. The standards behind them are
linked, and the [RDF primer](https://www.w3.org/TR/rdf11-primer/) is a good first read.

### Statement

A statement is a fact in three parts, a subject, a predicate and an object
(`ex:tom ex:name "Tom"`), and it is also called a triple. With the name of the graph it is in,
it is a quad.

### IRI

An IRI is the global name of a thing, such as `http://example.org/tom`, and `ex:tom` is its
short form after `PREFIX ex: <http://example.org/>`. SPARQL's JSON results call it
`"type":"uri"`.

### Graph

A graph is a set of statements. A store holds one default graph and any number of named graphs,
each named by an IRI. A query without `GRAPH` or `FROM` reads the default graph, or every graph
with `store.default_graph = "union"` ([configuration](ops/config-reference.md#store)).

### Class, instance, subclass

`ex:tom a ex:Cat` makes `ex:tom` an instance of the class `ex:Cat`, and
`ex:Cat rdfs:subClassOf ex:Animal` makes `ex:Cat` a subclass of `ex:Animal`, so every cat is
an animal. Subclass statements form a class hierarchy.

### Ontology

An ontology is made of statements about classes and properties themselves (subclasses, domains,
disjointness), from which reasoning derives statements about the data. NRESE stores it like any
other statements, and its rules read every graph.

### Asserted, inferred

Asserted statements are the ones written. Inferred ones follow from them under the reasoning
mode, and they are stored apart and kept current by every commit. `infer=false` reads the
asserted ones only.

### Reasoning mode, profile

A reasoning mode is the set of rules that derives the inferred statements, from `rdfs` up to
`owl2-dl` ([modes](spec/reasoning-semantics.md#modes)). OWL 2 QL, RL and DL are parts of OWL 2
that the [OWL 2 profiles](https://www.w3.org/TR/owl2-profiles/) standard defines, QL for query
rewriting, RL for rules and DL for the whole decidable language. A mode's rules are complete
only for an ontology inside its profile, so an answer from the rules alone says `sound-only`.

### Commit

A commit is one write (a SPARQL update, a Graph Store write or a client transaction's commit),
applied whole or not at all, with its inferences and checks
([what a commit does](what-it-covers.md#what-a-commit-does)).

### Completeness status

The completeness status is the `NRESE-Completeness` header on every SPARQL answer, and it says
what the answer promises ([what an answer promises](what-it-covers.md#what-an-answer-promises)).

### Shape, shapes graph, SHACL gate

A [SHACL](https://www.w3.org/TR/shacl/) shape is a constraint on the data, for example that
every animal has a name. Shapes are stored in the shapes graph, which is
`http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` by default (`shacl.shapes_graph`). When the gate
is on (`shacl.gate`), it checks every commit against them.

### Repository

A repository is one dataset with its own reasoning. A server holds one or more, and the default
one is `nrese`, the one `/dataset/…` serves ([repositories](ops/http-api.md#engine-api-apiv1)).

### Turtle

Turtle is a text syntax for RDF statements ([Turtle](https://www.w3.org/TR/turtle/)), and the
`.ttl` files of these pages are written in it. The other syntaxes are under
[RDF syntaxes](what-it-covers.md#rdf-syntaxes).

### HTTP request, response, status

Each `curl` command sends an HTTP request to a URL of the server. The server answers with a
status line, headers such as `content-type` and a body, and `curl -i` prints all three. Among
the statuses are `200 OK`, `204 No Content` when there is nothing to return and `400` when the
request is wrong ([status codes](ops/http-api.md#status-codes)).

### JSON

JSON is a text format for data, with objects in `{…}` that have named fields and lists in `[…]`.
A SPARQL answer comes as JSON unless you ask for another format. Its `head.vars` names the
columns and `results.bindings` holds one object per row, and each value has a `type` (`uri`,
`literal`, `bnode`) and a `value`
([SPARQL results in JSON](https://www.w3.org/TR/sparql11-results-json/)).

### Image, container, volume

These are Docker's words. The image is the built program, a container runs it and a volume keeps
its data when the container is removed
([Docker overview](https://docs.docker.com/get-started/docker-overview/)).
