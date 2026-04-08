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
- stronger query result comparators where count-only parity is insufficient, starting with a dedicated limit/offset bindings-set suite
- explicit Tell/Ask/Services contract definition so assertion ingest, query semantics, and service metadata/federation do not drift into one another
- bounded query/update failure-parity fixtures on top of the shared response-semantics comparator
- separate policy-failure parity fixtures for invalid-auth and oversize-payload cases on the same shared response-semantics comparator
- shared per-case request-header customization in the compat harness plus named invocation profiles so auth/proxy/live comparison cases stay on the same comparator path
- reusable connection-profile registry for pack and pack-matrix runs so secured compare stacks keep transport/auth policy outside workload-pack manifests
- exact ontology-name selection on top of the catalog-driven `pack-matrix` path so secured live parity can narrow to one official ontology without cloning workload packs
- service-level timeout budgets in workload-pack manifests plus named profile overrides so secured/live compare stacks can externalize transport ceilings without case-by-case duplication
- bounded secured live-deployment workload-pack templates so auth/policy/timeout parity runs reuse the same manifest model without committed secrets
- env-placeholder interpolation for pack headers so secured templates can stay versioned without embedding credentials
- timeout and payload-limit comparison cases
- manifest-driven production workload parity pack support so seed, compat, and bench runs use one versioned evidence unit instead of ad hoc fixture combinations
- workload-pack aggregate reporting so one pack run emits a canonical `pack-report.json` evidence index alongside per-suite compat reports and benchmark output
- pack-report persistence on failure so failed live parity runs still emit a top-level evidence index instead of leaving only partial suite files
- broader SPARQL Update parity fixtures for the intended replacement scope before moving to live project-specific packs
- bnode-isomorphic graph canonicalization for graph-producing parity cases so official ontology `CONSTRUCT` outputs compare semantics instead of raw blank-node labels
- staged real-world ontology catalog with small-to-broad official ontology sources plus typed processing metadata to harden parity and performance work incrementally
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
- bounded grouped-disjointness slices such as `owl:AllDifferent`, `owl:AllDisjointClasses`, and `owl:AllDisjointProperties` normalized into the same existing consistency gates
- stronger reject explanations and minimal-justification trails
- richer unsupported-construct diagnostics
- `owl-dl-target` replacement of scaffold-only behavior with explicit staged capability slices

Primary modules:

- `crates/nrese-reasoner/src/rules.rs`
- `crates/nrese-reasoner/src/class_consistency/mod.rs`
- `crates/nrese-reasoner/src/property_consistency/`
- `crates/nrese-reasoner/src/property_chain.rs`
- `crates/nrese-reasoner/src/rules_mvp_cache/`
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
- one frontend-owned TypeScript client boundary for browser and CLI access
- runtime-configured frontend API base support so frontend and backend hosting can be separated cleanly
- small CLI wrapper on the same TypeScript client boundary for fast operational and developer workflows
- provider-agnostic server-side AI suggestion layer
- string/style separation and extension points
- runtime-config and reasoning-policy visibility driven from server diagnostics instead of frontend-local shadow state
- customization guide and frontend tests
- production validation behind reverse proxy and auth policy

Primary modules:

- `apps/nrese-console`
- `apps/nrese-console/src/lib/`
- `apps/nrese-console/src/cli/`
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
- integrated bounded binary `owl:propertyChainAxiom` support on the same explicit feature-policy path, with named-node RDF-list planning cached at schema level and property-closure execution reusing that prepared plan
- extracted shared RDF-list parsing inside the reasoner and added bounded `owl:AllDifferent`, `owl:AllDisjointClasses`, and `owl:AllDisjointProperties` expansion onto the existing `owl:differentFrom`, class-disjointness, and property-disjointness paths, including deterministic malformed-list diagnostics
- added dedicated timeout-failure compat suites on the shared response-semantics comparator path
- committed secured live-auth and secured live-auth-timeout pack templates on the same manifest model as generic packs
- added a reusable connection-profile registry path for pack and pack-matrix runs so real NRESE/Fuseki URLs, auth, timeout defaults, and invalid-auth invocation profiles stay outside workload-pack manifests
- exposed `pack-validate` as the explicit preflight command for selected connection-profile wiring and invocation-profile reference checks before live parity runs
- added explicit `full` and `compat-only` pack execution modes so secured live parity can reuse the same pack format without always reseeding or benchmarking a target deployment
- expanded the staged real-world ontology catalog to include SKOS, SOSA, SSN, DCAT, vCard, DCMI Terms, and ODRL alongside the earlier official ontology set
- added typed ontology-fixture metadata for serialization, semantic dialects, reasoning focus, and service coverage so catalog entries can drive test intent rather than only download paths
- added cross-service ontology fixture tests that exercise official catalog inputs through Store, Server, and Reasoner paths, including RDF/XML preload/`tell` acceptance, Turtle base-IRI preload for official PROV-O, and ontology-backed inverse/transitive/domain-range reasoning checks
- collapsed duplicated catalog-fixture test helpers into shared support modules for store-side integration tests and added an additional bounded official-ontology-backed service slice for SKOS RDF/XML graph-store roundtrip
- grouped ontology-backed compat suites under a dedicated harness path and completed ontology-specific asserted-schema parity suites across the checked-in official baseline ontology packs
- added catalog-driven `pack-matrix` execution so baseline ontology packs can be run per tier through one aggregate evidence path with a top-level matrix report
- extended `pack-matrix` so typed ontology catalog metadata can select execution subsets by semantic dialect, reasoning feature, and service coverage
- extended `pack-matrix` again so one exact ontology name can be selected on the same metadata-driven path for narrower secured live parity runs
- added catalog-backed baseline-pack validation so pack naming, dataset alignment, and required compat-suite coverage are enforced before matrix evidence runs
- expanded official ontology-backed reasoner fixture coverage to the remaining catalog vocabularies so the current bounded `rules-mvp` slice is now exercised against vCard, DCMI Terms, SOSA, SSN, and ODRL in addition to the earlier official ontology set
- added service-level timeout budgets to workload-pack target profiles so secured/live parity runs can reuse shared transport ceilings without case duplication
- added named invocation-profile merging with collision rejection, so selected live connection profiles and workload packs share one request-normalization path without silent override drift
- added preflight validation of compat-suite invocation-profile references against the selected live connection profile and workload-pack overlays, so secured parity runs fail before seed/bench execution when profile wiring drifts
- added a dedicated `pack-validate` command and report shape so deployment workflows can gate live parity runs on the same connection-profile and pack-resolution path used by real execution
- added a dedicated limit/offset semantics compat suite and a bindings-set comparator so query-window parity does not rely on count-only summaries
- replaced the harness-local blank-node canonicalization path with upstream oxrdf graph canonicalization, which reduced custom comparison code while fixing local ORG `CONSTRUCT` parity against Fuseki
- extended compat reports with optional result-count and diff-sample fields, and ensured `pack-report.json` is written even when a pack fails mid-run
- verified local live side-by-side parity against a local Apache Jena Fuseki 6.0.0 install for the official FOAF, ORG, and SKOS packs on the standard `pack-matrix` evidence path
- moved the class-consistency test block into a dedicated `src/tests/consistency_tests.rs` file to match the repo’s topic-adjacent unit-test convention
- initialized git versioning for the repository and updated `.gitignore` for Rust, frontend, runtime, and local-secret artifacts
- added a separate `/console` frontend application with modular React/TypeScript structure, separated styles/i18n, and basic build/test coverage
- added typed server-side AI provider integration for Gemini and OpenRouter plus public AI suggestion/status endpoints on the same config path as the rest of the server
- added a staged real-world ontology catalog sync path in the harness for official FOAF, W3C ORG, W3C Time, PROV-O, SKOS, SOSA, SSN, and DCAT sources
