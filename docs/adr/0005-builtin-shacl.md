# ADR-0005: SHACL validation is built in and runs as a commit gate

Status: accepted (2026-09-25)

## Context

GraphDB validates SHACL on commit. The main downstream user, the datamodel workflow feeding ResearchSpace, versions SHACL shapes next to its ontology modules and needs a store-side safety net. QLever has no SHACL.

## Decision

- **Crate and inputs.** New crate `nrese-shacl` (L2). Shapes graphs are loaded from designated named graphs in the store (configurable) and compiled into an ID-based constraint program per shapes revision.
- **Coverage.** SHACL Core first:
  - targets and property paths
  - value type, cardinality, value range, string, property pair
  - logical and shape-based constraints
  - `sh:closed`, `sh:in`, `sh:hasValue`
  - severities

  SHACL-SPARQL constraints come later, evaluated through `nrese-sparql`.
- **Incremental validation.** On commit the gate validates only the focus nodes the delta affects: nodes in the delta, and nodes that reach them through the compiled shape paths. Full validation is available on demand and for restores.
- **Blocking and reports.** Results with severity `sh:Violation` block the commit by default; the blocking severity is configurable. Reports are standard `sh:ValidationReport` graphs plus a JSON projection.

## Consequences

- The mutation pipeline gets one more gate. The reasoner runs first, so shapes can see inferences if configured; SHACL runs second.
- Differential tests against the W3C SHACL test suite are required before the gate is enabled by default.
