# Replacement Implementation Plan

## Purpose

This document turns the replacement gap analysis into an implementation plan with explicit tracks, ownership boundaries, and evidence gates.

It is intentionally separate from the gap matrix:

- `06-fuseki-replacement-gap-matrix.md` says what is still missing
- this document says how NRESE closes those gaps without mixing concerns or creating technical debt

## Planning Rules

- Each track owns one concern boundary.
- Each implementation block must ship with code, tests, and doc/spec updates together.
- No track is allowed to claim replacement readiness from feature presence alone; evidence is required.
- New runtime behavior must have a single source of truth per concern.
- Every bounded implementation block is followed by a reactor step:
  - collapse duplicate mapping/helpers introduced during the slice
  - remove stale scaffolding and dead code exposed by the slice
  - split mixed-responsibility files before the next feature block starts
  - re-run lint/test gates before claiming the slice is complete

## Cross-Cutting Reactor Step

Objective:

- keep the workspace modular and prevent replacement work from accreting parallel implementations

Reactor checklist:

- move duplicated projection/mapping logic into a shared module
- delete helpers that are now shadowed by stronger single-source-of-truth paths
- keep tests in dedicated files when runtime modules start mixing behavior and verification
- flag oversized files for the next bounded split instead of letting them quietly become permanent

Current priority candidates:

- `crates/nrese-server/src/error.rs` and `crates/nrese-server/src/http/operator_diagnostics.rs`
- `crates/nrese-reasoner/src/class_consistency/mod.rs`
- `crates/nrese-server/tests/http_api_tests.rs`
- `apps/nrese-console/src/App.tsx`

## Track A: Protocol Replacement Evidence

Objective:

- move from "feature exists" to "behavior matches Fuseki closely enough to replace it"

Implementation blocks:

- fixture-driven comparison suites for query/update/graph-store edge cases
- explicit Tell/Ask/Services contract definition so assertion ingest, query semantics, and service metadata/federation do not drift into one another
- bounded query/update failure-parity fixtures on top of the shared response-semantics comparator
- separate policy-failure parity fixtures for invalid-auth and oversize-payload cases on the same shared response-semantics comparator
- shared per-case request-header customization in the compat harness so auth/proxy/live comparison cases stay on the same comparator path
- service-level request-header profiles in workload-pack manifests so secured compare stacks stay on the same comparator path as unsecured runs
- timeout and payload-limit comparison cases
- manifest-driven production workload parity pack support so seed, compat, and bench runs use one versioned evidence unit instead of ad hoc fixture combinations
- broader SPARQL Update parity fixtures for the intended replacement scope before moving to live project-specific packs
- staged real-world ontology catalog with small-to-broad official ontology sources to harden parity and performance work incrementally
- project-realistic compatibility packs against the live Fuseki deployment
- CI wiring for repeatable compatibility execution
- keep response-semantics normalization in one harness path so failure-parity checks do not fork into endpoint-specific comparators

Primary modules:

- `benches/nrese-bench-harness`
- `docs/ops/benchmark-and-conformance.md`
- `docs/spec/04-api-and-protocols.md`

Evidence gate:

- automated report artifacts show stable equivalence for the intended project query/update surface

## Track B: Reasoner Expansion Beyond `rules-mvp`

Objective:

- move from bounded rule closure to a stronger enterprise-ready reasoning stack

Implementation blocks:

- broader EL/RL-oriented rule coverage
- bounded identity/role slices such as `owl:differentFrom` consistency against equality closure and bounded `owl:ReflexiveProperty` inference over observed resources
- bounded equality-entailment slices such as `owl:FunctionalProperty` / `owl:InverseFunctionalProperty` implying canonical effective `owl:sameAs`
- bounded unsatisfiable-class slices such as `owl:Nothing` rejection over effective types and schema-cached class-consistency preparation
- stronger reject explanations and minimal-justification trails
- richer unsupported-construct diagnostics
- `owl-dl-target` replacement of scaffold-only behavior with explicit staged capability slices

Primary modules:

- `crates/nrese-reasoner/src/rules.rs`
- `crates/nrese-reasoner/src/class_consistency/mod.rs`
- `crates/nrese-reasoner/src/property_consistency/`
- `crates/nrese-reasoner/src/service.rs`
- `docs/spec/03-reasoner-and-owl-profile.md`

Evidence gate:

- reproducible fixture packs prove inference, reject behavior, and explanation stability on real RDF inputs

## Track C: Persistence, Backup, and Recovery

Objective:

- raise durable storage from available feature path to replacement-grade operational behavior

Implementation blocks:

- C1: store export/import primitives with stable formats
- C2: server admin endpoints as thin wrappers with no persistence logic
- C3: integration tests for backup -> wipe -> restore -> query parity
- C4: durable restart/recovery test suite
- C5: drill evidence pipeline and retention rule

Primary modules:

- `crates/nrese-store`
- `crates/nrese-server/src/http/admin_dataset.rs`
- `docs/ops/backup-restore-drills.md`
- `docs/spec/02-storage-and-transactions.md`

Ownership boundaries:

- `nrese-store` owns artifact format and import/export behavior
- `nrese-server` owns authorization and transport only
- `docs/ops/backup-restore-drills.md` owns procedures and evidence requirements

Evidence gate:

- documented and tested restore path from a real durable dataset state

## Track D: Security and Hardening

Objective:

- close the gap between development-safe controls and production-ready traffic governance

Implementation blocks:

- rate limits and quotas
- bounded `bearer-jwt`, bounded `oidc-introspection`, and bounded proxy-terminated `mtls` implementations behind explicit contracts, followed by broader auth expansion such as direct mTLS if needed
- hardened deployment defaults and release checks
- auditable operator/admin access behavior

Primary modules:

- `crates/nrese-server/src/policy.rs`
- `crates/nrese-server/src/rate_limit.rs`
- `crates/nrese-server/src/config/`
- `docs/ops/server-setup.md`
- `docs/ops/config-reference.md`

Evidence gate:

- policy tests and deployment docs cover auth, request governance, and controlled external exposure

## Track E: Performance and Resource Evidence

Objective:

- replace assumptions with measured migration evidence

Implementation blocks:

- startup latency measurement
- CPU/RAM capture during workload runs
- reasoning cost measurements by mode
- regression thresholds and CI artifacts
- production workload parity pack execution against the real ontology/workload as the evidence carrier for replacement-grade comparisons

Primary modules:

- `benches/nrese-bench-harness`
- `docs/ops/benchmark-and-conformance.md`
- `docs/spec/06-fuseki-replacement-gap-matrix.md`

Evidence gate:

- benchmark reports cover the real ontology and real workload patterns, not only synthetic seeds

## Track G: User Frontend and Assisted UX

Objective:

- provide a replacement-grade browser frontend that is usable without RDF/SPARQL expertise while remaining API-driven and configurable

Implementation blocks:

- frontend shell for query, tell, update, graph, and runtime visibility
- provider-agnostic server-side AI suggestion layer
- string/style separation and extension points
- customization guide and frontend tests
- production validation behind reverse proxy and auth policy

Primary modules:

- `apps/nrese-console`
- `crates/nrese-server/src/ai/`
- `crates/nrese-server/src/http/ai.rs`
- `docs/dev/frontend-extension-guide.md`

Evidence gate:

- frontend workflows run against the public server API, remain usable without AI, and are reproducibly build- and test-validated

## Track F: Migration and Operational Readiness

Objective:

- prove that a real Fuseki deployment can move to NRESE and be operated safely

Implementation blocks:

- pilot migration against the real dataset
- signed rollback evidence
- incident/recovery operating notes
- operator workflows for day-two support

Primary modules:

- `docs/ops/migration-fuseki-to-nrese.md`
- `docs/ops/rollback-runbook.md`
- `docs/ops/operational-readiness-handbook.md`

Evidence gate:

- one real pilot run completes with successful rollback validation and retained evidence artifacts

## Current Execution Priority

Priority order for the next replacement-focused runs:

1. protocol replacement evidence
2. persistence/backup/recovery primitives
3. security hardening
4. reasoner expansion beyond `rules-mvp`
5. performance evidence on the real ontology
6. migration and operational sign-off

## Implemented This Round

- grouped `identity/` modules so equality indexing, equality entailment, and `owl:differentFrom` consistency are no longer spread across root files
- moved bounded `owl:FunctionalProperty` / `owl:InverseFunctionalProperty` handling from hard reject semantics to canonical equality entailment, with fixed-point preparation and end-to-end tests
- grouped `class_consistency/` modules so `owl:Nothing` and disjoint-type checks now share one schema-cached class-side preparation path instead of living in a root file
- bounded reasoner/runtime consistency cleanup and TTL-based service fixtures
- typed `rules-mvp` presets on top of the explicit feature-policy path
- test/file separation improvements in the reasoner slice
- shared minimal TTL fixture for Store, Reasoner, and Server service-level tests
- in-memory rate limiting in the server policy path as the first concrete quota-control step
- store-owned dataset backup/restore primitives with admin-only server transport
- shared reject-evidence mapping in the server so HTTP problem details and operator diagnostics no longer maintain parallel field-by-field projections
- bounded `bearer-jwt` support in the server policy path with explicit config and dedicated HTTP/config tests
- bounded proxy-terminated `mtls` support in the server policy path with explicit subject-header config and dedicated HTTP/config tests
- grouped `property_consistency/` modules plus a schema-cached property-constraint plan and constrained-predicate prepared consistency index in the reasoner cache path
- grouped `effective_types/` modules so direct-type collection, equality propagation, origin ranking, and origin rendering are no longer mixed in one file
- grouped `property_closure/` modules so closure state, build-time queue/adjacency management, and equality expansion are no longer mixed in one file
- centralized server env-var names in `crates/nrese-server/src/config/env_names.rs` so config parsing uses one source of truth for knob names
- grouped `rules_mvp_cache/` modules so cache policy, schema-prepared artifacts, and prepared-run assembly are no longer mixed in one file
- externalized `rules-mvp` runtime behavior into a typed reasoner-owned feature policy with server-owned env parsing, policy-aware cache identity, and operator diagnostics that expose the configured reasoning policy
- moved the class-consistency test block into a dedicated `src/tests/consistency_tests.rs` file to match the repo’s topic-adjacent unit-test convention
- initialized git versioning for the repository and updated `.gitignore` for Rust, frontend, runtime, and local-secret artifacts
- added a separate `/console` frontend application with modular React/TypeScript structure, separated styles/i18n, and basic build/test coverage
- added typed server-side AI provider integration for Gemini and OpenRouter plus public AI suggestion/status endpoints on the same config path as the rest of the server
- added a staged real-world ontology catalog sync path in the harness for official FOAF, W3C ORG, W3C Time, and PROV-O sources
