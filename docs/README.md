# NRESE documentation index

Where each concern is decided, and in which document the binding state lives.

## Goal

NRESE is an RDF database and ontology platform that **reads like QLever and governs data
like GraphDB**: its own storage engine, materialised and incrementally maintained
reasoning (RDFS, OWL 2 RL, user rules) on the way to OWL 2 DL, SHACL at commit, full-text
search, the SPARQL 1.2 and RDF4J protocols, and one engine API under every client. It is a
platform in its own right; ResearchSpace and the datamodel workflow are integrations that
must work end to end ([vision](spec/00-vision-and-scope.md)). Fuseki is not a parity
target ([ADR-0004](adr/0004-parity-targets-qlever-graphdb.md)).

## Binding sources

| Question | Document |
|---|---|
| Who owns what, in which layer is a fix made? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Why was something decided so? | [adr/](adr/) |
| What is done, deferred, open? | [STATUS.md](STATUS.md), the one place for status |
| What is built in which order? | [The roadmap](plan/2026-10-02-roadmap.md) (the current order), [ROADMAP.md](ROADMAP.md) and the plans in [plan/](plan/) (design and order, no status) |
| Where do we stand against QLever and GraphDB? | [spec/06-target-capability-matrix.md](spec/06-target-capability-matrix.md) |
| What reasoning computes (rules, omissions, graph scope, consistency) | [spec/reasoning-semantics.md](spec/reasoning-semantics.md) |
| Behaviour of the v1 implementation (historical) | [spec/02](spec/02-storage-and-transactions.md), [03](spec/03-reasoner-and-owl-profile.md), [04](spec/04-api-and-protocols.md) |
| Operation and configuration | [ops/](ops/), above all [config-reference.md](ops/config-reference.md) |
| The HTTP interface (endpoints, formats, status codes) | [ops/http-api.md](ops/http-api.md) |
| ResearchSpace and the datamodel workflow | [integration/](integration/) |
| Code rules | [dev/code-structure-guidelines.md](dev/code-structure-guidelines.md) |

Superseded documents (the Fuseki gap matrix, the old implementation plan) are in
[archive/](archive/).

## When documents disagree

ARCHITECTURE before ADRs before the specs. The capability matrix alone holds the status,
the roadmap and the current plan alone the order. Whoever finds a contradiction corrects
the lower-ranked document in the same change.

## Reading order

1. this file;
2. [ARCHITECTURE.md](ARCHITECTURE.md);
3. [ROADMAP.md](ROADMAP.md) and the newest plan in [plan/](plan/);
4. the spec or ADR of the concern at hand.
