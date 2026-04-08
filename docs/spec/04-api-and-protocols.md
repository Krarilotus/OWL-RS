# API and Protocol Specification

## Scope

Define a standards-aligned, enterprise-ready HTTP API layer suitable for a full Rust-native Fuseki-class replacement.

## Tell / Ask / Services Boundary

The API surface separates three concerns that should not be collapsed into one another:

- `ASK`
  - remains standard SPARQL `ASK` query semantics on `/dataset/query`
- `TELL`
  - is a first-class assertion-ingest interface for RDF payloads
  - is separate from raw SPARQL Update text and separate from Graph Store transport semantics
  - must run through the same staged validation and publish gate as other write paths
  - currently accepts Turtle, N-Triples, and RDF/XML on the same typed RDF-format path used by ontology preload and Graph Store writes
  - accepts `Content-Location` as an explicit base-IRI hint when relative IRIs are present in RDF payloads
- `SERVICES`
  - splits into:
  - inbound service metadata and capability advertisement
  - outbound federated query capabilities such as SPARQL `SERVICE`, which remain a later bounded slice

The first implementation slice for this concern is:

- explicit `TELL` ingest endpoint
- explicit `ASK` documentation on the existing query endpoint
- explicit inbound service metadata surface
- no outbound federated `SERVICE` execution yet

## Standards Alignment

Primary conformance targets:
- SPARQL 1.1 Query Protocol
- SPARQL 1.1 Update Protocol
- SPARQL 1.1 Graph Store HTTP Protocol
- RDF serialization interoperability (Turtle, N-Triples, N-Quads, TriG, RDF/XML where supported)
- HTTP/1.1 and HTTP/2 semantics

## Required Endpoint Surface

- `GET|POST /dataset/query`
- `POST /dataset/update`
- `POST /dataset/tell`
- `GET|POST|PUT|DELETE|HEAD /dataset/data`
- `GET /dataset/info`
- `GET /dataset/service-description`
- `GET /console`
- `GET /api/ai/status`
- `POST /api/ai/query-suggestions`
- `GET /ops` (operator frontend shell, externally reachable)
- `GET /ops/api/capabilities`
- `GET /ops/api/dataset/summary`
- `GET /ops/api/health/extended`
- `GET /ops/api/admin/dataset/backup`
- `POST /ops/api/admin/dataset/restore`
- `GET /healthz`
- `GET /readyz`
- `GET /version`
- `GET /metrics` (configurable exposure)

## Capability Tiers

### API Tier A1

- Query + update parsing and execution
- First-class `TELL` assertion ingest on the same staged publish path as update
- Typed error model
- Basic health/readiness

### API Tier A2

- Full graph store method coverage
- Content negotiation for query and graph responses
- Stable status code/error class behavior
- Explicit base-IRI handling for RDF upload paths through `Content-Location`
- Durable deployment profile support (persistent store mode, startup validation, recovery signaling)

### API Tier A3

- Authn/Authz role policies
- Bounded `bearer-jwt` support in the current implementation slice
- Bounded proxy-terminated `mtls` support in the current implementation slice
- Request size/time limits
- Structured request correlation and tracing
- Operator endpoint exposure model (edge/TLS/proxy-aware headers and trusted-forwarded strategy)
- Centralized policy configuration for endpoint exposure, auth mode, timeouts, and payload limits

### API Tier A4

- Protocol conformance suite coverage in CI
- Service description/capability advertisement endpoint
- Tenant and policy hooks for enterprise routing
- RFC 9457-compatible problem details responses
- Operator UI/API parity checks to ensure frontend actions map to documented public contracts

## Response and Error Contracts

- Errors SHOULD use `application/problem+json`.
- Reasoner-gated consistency rejects SHOULD include a structured `reasoner_reject` payload with violated constraint, focus resource, heuristic blame fields, explicit conflict evidence triples, and a likely commit-local trigger triple when the staged delta admits a high-confidence attribution.
- `TELL` failures MUST use the same typed reject/error surface as the staged update pipeline, rather than inventing a separate ingest-only reject shape.
- AI query suggestion responses MUST be explicitly treated as assistive UX output, not authoritative execution results.
- All requests SHOULD carry correlation IDs.
- Protocol and policy failures must map to deterministic status codes.
- Graph Store write failures for malformed RDF and unsupported RDF media types MUST stay deterministic and be comparable at least by status code, normalized content type, and coarse response-body class.
- Covered query and update failure cases, currently limited to invalid-syntax fixtures in the compat harness, MUST stay deterministic and be comparable at least by status code, normalized content type, and coarse response-body class.
- Media-type parsing for query, update, graph-store, and dataset-restore entry points SHOULD stay centralized so parameterized headers and accept lists do not drift into handler-specific semantics.
- Operator API failures must expose actionable remediation metadata without leaking sensitive internals.
- Policy failures SHOULD distinguish `401`, `403`, `404`, `408`, and `413` where applicable.

## Operator Frontend Requirements

- `/ops` MUST serve an ergonomic browser-based operator interface for query, update, graph management, and runtime diagnostics.
- `/ops` MUST be disabled or access-restricted by policy in hardened deployments, while still supporting external reachability when explicitly enabled.
- Operator actions MUST call documented server endpoints and MUST NOT bypass API contracts through private backdoors.
- Operator API and page assets MUST support reverse-proxy deployments (path prefix, forwarded host/proto, and strict CSP compatibility).
- Initial operator page load and core interactions SHOULD remain responsive under normal administrative usage profiles.

## User Frontend Requirements

- `/console` MUST serve a user-facing browser interface for query, tell, update, graph-store, and dataset inspection workflows.
- `/console` MUST stay API-driven and same-origin with documented HTTP contracts.
- The frontend package MUST also be able to target a non-same-origin backend through explicit frontend-owned runtime configuration, so browser hosting and backend hosting remain separable concerns.
- Styling, language strings, and API transport code MUST remain separated so the frontend is customizable without rewriting behavior.
- Frontend endpoint paths, request construction, and connector logic MUST remain frontend-owned in one explicit client boundary rather than being reimplemented across components or scripts.
- AI-assisted query suggestion support MUST remain optional and provider-agnostic at the server boundary.
- The frontend MUST continue working when AI suggestions are disabled.
- Fast operator and developer access SHOULD also be available through a frontend-owned CLI wrapper that uses the same typed HTTP client boundary as the browser frontend.

## Deployment Durability Requirements

- Persistent mode MUST survive process restarts without data loss for committed operations.
- On startup, the service MUST validate dataset integrity and expose degraded readiness if recovery is required.
- Backup and restore interfaces MUST be documented and scriptable through stable endpoints or CLI wrappers.
- Backup export/import endpoints MUST stay transport-thin, with dataset artifact semantics owned by the store layer.
- Version/capability endpoints MUST clearly indicate whether runtime is ephemeral or durable.

## Conformance Requirements

- W3C protocol behavior tests for supported features MUST pass.
- Graph Store interoperability tests MUST be automated.
- Covered query/update failure-parity fixtures and graph-store failure-parity fixtures MUST be automated.
- Unsupported protocol features MUST return explicit, stable diagnostics.
- `/version` and service-description surfaces MUST reflect actual enabled capabilities, not aspirational ones.
- Capability surfaces MUST distinguish between:
- SPARQL query/update support
- first-class `TELL` ingest support
- inbound service metadata support
- outbound federated service support

## Security and Operational Requirements

- TLS at edge or in-process with trusted proxy policy, with the current `mtls` slice explicitly defined as proxy-terminated mTLS rather than direct in-process client-certificate termination
- Role-based access mapping for query/update/data endpoints, currently implemented through `none`, `bearer-static`, bounded `bearer-jwt`, and bounded proxy-terminated `mtls`
- Rate and concurrency controls for abuse protection
- Audit-friendly request logging with sensitive data redaction policy
- CSRF/session considerations documented for browser-based operator flows
- Operator endpoint may be exposed externally only with explicit auth policy and transport hardening enabled
- Minimum baseline implementation must support:
- configurable payload ceilings for query/update/RDF upload
- configurable timeout ceilings for query/update/graph operations
- configurable enable/disable policy for `/ops` and `/metrics`
- request correlation via `X-Request-Id`

## Acceptance Criteria

- Query, update, and data operations are independently testable.
- `TELL` ingest is independently testable from Graph Store writes and SPARQL Update text.
- Media-type negotiation behavior is deterministic and documented.
- Endpoint-level policy enforcement is explicit, not hidden in storage code.
- Covered query/update failure cases are parity-tested through the compatibility harness, while payload/rate/auth policy contracts remain explicitly HTTP-test-covered in the server crate.
- Operational endpoints reflect true runtime readiness.
- Operator frontend can perform core admin workflows against public API endpoints without privileged bypasses.
- Durable deployment behavior is test-covered for restart/recovery and accurately reflected in readiness/capability endpoints.
- Policy controls for auth, exposure, payload size, and timeouts are test-covered at HTTP level, including bounded `bearer-jwt` and bounded proxy-terminated `mtls` authorization behavior for the covered role mappings.
- Admin backup/restore endpoints are auth-restricted, return stable media types, and preserve no-partial-publication semantics on restore failure.
