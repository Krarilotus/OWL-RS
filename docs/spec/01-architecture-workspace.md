# Architecture and Workspace Boundaries

## Goal

Define a Rust-native architecture that can grow from current single-node capability to a full Fuseki-class replacement without accumulating structural debt.

## Crate Responsibilities

### `nrese-core`

Owns:
- Shared RDF and reasoning contracts
- Typed error and capability models
- Storage and reasoner abstraction traits

Must not own:
- HTTP transport types
- Storage engine implementation details
- Deployment/config loading logic

### `nrese-store`

Owns:
- Durable dataset lifecycle
- Snapshot and transaction boundaries
- SPARQL query and update execution adapters
- Graph materialization primitives

Must not own:
- HTTP parsing/serialization
- Authn/Authz policy enforcement
- Reasoner algorithm internals

### `nrese-reasoner`

Owns:
- Reasoning profile declarations
- Inference planning and execution contracts
- Consistency and explanation metadata outputs

Must not own:
- HTTP protocol concerns
- Persistent store internals

### `nrese-server`

Owns:
- W3C protocol transport layer
- Request validation and response shaping
- Authn/Authz middleware integration
- Runtime orchestration of store + reasoner
- Observability and readiness surfaces

## Dependency Rules

- `nrese-core` has no workspace-internal dependencies.
- `nrese-store` and `nrese-reasoner` depend on `nrese-core`.
- `nrese-server` depends on all other crates only through public APIs.
- No crate imports sibling internals.

## Missing Subsystems Required for Full Replacement

- Dataset management API and dataset catalog metadata
- Graph Store protocol coverage for default and named graphs
- Service description and feature declaration endpointing
- Policy layer for authn/authz + per-endpoint role controls
- Resource governance controls (timeouts, payload limits, concurrency budgets)
- Durable snapshot lifecycle tooling (backup, restore, compaction hooks)
- Interoperability harnesses for SPARQL protocol conformance
- Cluster-readiness contracts (state externalization and partition strategy)

## Integration Flow (Target)

1. Server receives protocol-compliant request.
2. Transport layer validates input and policy.
3. Store opens/reads candidate revision.
4. Reasoner plan executes for configured mode.
5. Store publishes revision atomically.
6. Server returns standards-compliant response with trace correlation.

## Extension Points

- Storage adapter trait: Oxigraph now, backend-pluggable later
- Reasoning mode registry: disabled, rules, advanced OWL targets
- Policy middleware chain: auth, quotas, tenant routing
- Observability sinks: tracing, metrics, audit logs

## Architecture Done Gates

- Every crate has one clear reason to change.
- New feature slices include contracts + tests + documentation.
- No protocol behavior is implemented in storage internals.
- No hidden global state shared across layers.
