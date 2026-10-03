# ADR-0007: One engine API; protocols and the frontend translate to it

Status: accepted (2026-10-02); `/ops/api/*` and `/api/ai/*` replaced on 2026-10-03 (aliases
with `Deprecation` and `Link` headers for one release). The owner's direction: one consolidated engine API that every
connector and the coming frontend build on, connectors shimmed to a minimum, the logic in
the core ([gap plan G2](../plan/2026-10-02-engine-gaps.md)).

## Context

The server answers four families of routes, grown one feature at a time:

- `/dataset/*`: SPARQL Protocol, Graph Store Protocol, tell, SHACL, autocomplete,
  classification, service description — for the default repository only.
- `/repositories/{id}/*`: the RDF4J REST protocol, for every repository.
- `/ops/api/*`: administration and diagnostics (backups, images, restore, capabilities,
  runtime and reasoning diagnostics), for the default repository.
- `/api/ai/*`: query suggestions.

So a capability's reach depends on the protocol a client speaks: SHACL validation or text
search on a second repository is only reachable through SPARQL over RDF4J, backups only
for the default repository. Semantics leaked into the adapters (RDF4J's transactions and
namespaces lived in server state until 2 October). A frontend built on this would have to
know all four families and their gaps, and every new capability would be added up to four
times.

## Decision

- **The core owns every capability as an operation** of a repository's store service:
  data (statements, graphs, imports, exports), queries and updates, sessions (client
  transactions), namespaces, reasoning (ruleset, state, rematerialisation, explanation of
  inferences and rejects), shapes (graph, validation, gate), search (text, autocomplete),
  classification, access (policies, users, workspaces), backups and images, statistics
  and diagnostics. State that belongs to a repository lives in its store, not in the
  server.
- **One management API**, versioned and repository-scoped, JSON over HTTP:
  `/api/v1/repositories/{id}/…` for everything the standard protocols don't cover, and
  `/api/v1/…` for the server (repositories, users, capabilities, health). Errors are
  `application/problem+json`. Its description (OpenAPI 3.1) is generated from the
  server's request and response types, and published at `/api/v1/openapi.json`, so the
  frontend, the CLI and third-party clients use one typed contract.
- **The standard protocols stay first-class and become translations:** SPARQL 1.1/1.2
  Protocol and the Graph Store Protocol at `/repositories/{id}/…` and, for the default
  repository, at `/dataset/…`; the RDF4J REST protocol; GraphDB-compatible extensions
  (`infer`, `onto:explicit`/`onto:implicit`, Workbench endpoints where a client needs
  them). An adapter parses the protocol's request, calls the core operation, and writes
  the protocol's response; it holds no state and no semantics.
- **Every capability reaches every repository** through the same path; nothing is
  default-repository-only.
- **`/ops/api/*` is replaced** by `/api/v1`; its routes stay as aliases for one release,
  then go. The operator page and the console are replaced by the one frontend (to be
  decided in its own ADR, after the engine gaps: owner, 2 October).

## Consequences

- New capabilities are added once, in the core, with one management route; protocols
  gain them where the protocol has a place for them.
- The server crate shrinks to routing, authentication, request limits and protocol
  translation; its tests become protocol conformance tests, the semantics' tests move to
  the store.
- The DMW and ResearchSpace flows are tested end to end against the protocols they use,
  and every management operation through `/api/v1`.
- A generated OpenAPI description needs a schema crate (`utoipa`, MIT/Apache-2.0) in the
  server; its types are the contract, so changing them is an API change.
