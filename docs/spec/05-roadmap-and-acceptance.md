# Roadmap and Acceptance Criteria

## Objective

Provide a capability-driven delivery sequence from current prototype maturity to a full Rust-native Fuseki replacement with stronger reasoning potential.

The canonical replacement-readiness tracking document is [06-fuseki-replacement-gap-matrix.md](./06-fuseki-replacement-gap-matrix.md).
The compact document map and ownership rules live in [../../Spezifikation.md](../../Spezifikation.md).

## Release Stages

### R0: Foundation and Contracts

- Workspace boundaries established
- Core contracts and typed result models stabilized
- Initial documentation split complete

### R1: Protocol Baseline

- SPARQL query endpoint productionized
- SPARQL update endpoint productionized
- First-class `TELL` ingest endpoint added on the staged validation/publish path
- Health, readiness, and version surfaces available
- Initial operator frontend shell (`/ops`) reachable in controlled environments

### R2: Graph Store Completion

- Full Graph Store method support
- Named/default graph lifecycle behavior covered by tests
- Media-type parse/serialize coverage improved
- Version and service-description endpoints exposed with capability reflection
- Operator API endpoints for dataset summary and capability introspection available

### R3: Transaction and Operability Hardening

- Revision and snapshot publication contracts explicit
- Observability baseline complete (metrics + traces + correlation)
- Backup/restore workflow is implemented and validated through recurring drills
- Problem Details error surface aligned with protocol contracts
- Host-specific development shortcuts removed from committed runtime configuration
- Durable deployment profile validated (restart persistence, startup integrity checks, recovery signaling)
- External operator exposure model documented (auth, TLS, proxy, CSP, path prefix)
- Central policy layer enforces auth, request size limits, timeout ceilings, and endpoint exposure, with bounded `bearer-jwt`, bounded `oidc-introspection`, and bounded proxy-terminated `mtls` support added alongside `bearer-static`
- Initial rate-limit buckets for read, write, and admin traffic are enforced from the same policy path as auth
- Server runtime configuration now supports file-based `config.toml` input with environment overrides on the same typed parser path, so deployment policy stays external to handler/runtime logic
- User-facing `/console` frontend exists as a separate browser surface from `/ops`, with optional server-side AI query suggestions through a typed provider abstraction
- `/console` frontend now owns an explicit TypeScript client boundary and runtime-configurable backend base URL, so browser hosting and backend hosting can be separated without rewriting components
- `/console` frontend now also ships a small CLI on the same TypeScript client boundary for fast query/update/tell/graph/runtime access without separate backend scripts
- `rules-mvp` now supports explicit presets on top of the feature-policy path, so runtime defaults can be standardized without hard-coding product opinion into the engine

### R4: Reasoning Integration

- Reasoner plans/reports integrated in update pipeline
- Inference visibility modes documented and test-covered
- Consistency gate semantics enforced
- Update path no longer bypasses reasoner orchestration semantics
- Hybrid explanation model available: fast commit-path diagnostics plus deeper justification retrieval
- Reject-path staging prevents inconsistent updates from being published before validation completes
- Commit-path reasoner rejects now expose structured problem details and operator diagnostics from the same reasoner-owned explanation payload
- The shared reject payload now includes explicit conflict evidence triples, so HTTP and operator surfaces do not reconstruct reject causes from free-text summaries
- Staged update previews are available to enrich reject-path diagnostics with a likely commit-local trigger triple and ranked attribution candidates where attribution is strong enough
- `rules-mvp` property-characteristic consistency checks now cover bounded `owl:IrreflexiveProperty`, `owl:AsymmetricProperty`, and `owl:propertyDisjointWith` rejection semantics
- `rules-mvp` now includes bounded explicit `owl:sameAs` support through canonical equality handling
- `rules-mvp` now includes bounded equality entailment from `owl:FunctionalProperty` and `owl:InverseFunctionalProperty`, with downstream rejection only if the resulting effective equality conflicts with other bounded consistency rules such as `owl:differentFrom`
- `rules-mvp` now includes bounded binary `owl:propertyChainAxiom` support over named properties with well-formed named-node RDF lists, behind the same typed feature-policy path as the rest of `rules-mvp`
- `rules-mvp` now includes bounded `owl:ReflexiveProperty` inference over observed named resources and commit-path `owl:differentFrom` consistency rejection against the effective equality closure
- `rules-mvp` now expands bounded `owl:AllDifferent`, `owl:AllDisjointClasses`, and `owl:AllDisjointProperties` declarations over RDF list members into the same existing `owl:differentFrom`, `owl:disjointWith`, and `owl:propertyDisjointWith` consistency gates
- `rules-mvp` now includes bounded `owl:Nothing` rejection over effective types, including named classes that close to `owl:Nothing` through the current taxonomy support
- `rules-mvp` emits deterministic unsupported diagnostics for constructs that remain out of scope in the current bounded slice
- `rules-mvp` now memoizes prepared reasoning artifacts and inference output by snapshot content key so repeated runs over identical staged dataset state avoid rebuilding indexing, taxonomy, equality, and closure inputs
- `rules-mvp` now also memoizes schema-stable preparation artifacts by a dedicated schema/TBox content key, so ABox-only changes can reuse taxonomy and schema-closure preparation without unsafe full-result reuse
- `rules-mvp` now keeps a bounded multi-entry execution/schema cache so alternating hot snapshot patterns do not immediately evict each other
- `rules-mvp` cache reuse is now exposed as structured runtime telemetry and Prometheus metrics, so execution/schema cache behavior can be observed without parsing note strings
- `rules-mvp` property-characteristic consistency checks now build one prepared assertion index per run for constrained predicates, so irreflexive, asymmetric, functional, inverse-functional, and property-disjoint rejection gates reuse a single grouped property view
- `rules-mvp` now also caches a schema-stable property-constraint plan, so ABox-sensitive property-consistency indexing no longer has to rediscover the relevant constrained predicate set on each run
- `rules-mvp` runtime behavior is now externalized behind a typed feature policy, so closure, equality, consistency gates, and unsupported-construct diagnostics can be configured without embedding product opinion into reasoner code
- official ontology fixtures now provide bounded release-evidence for `rules-mvp` reasoning/runtime validation on FOAF, W3C Time, W3C ORG, SKOS, PROV-O, and DCAT

### R5: Advanced Reasoning and Conformance

- Broader reasoning profile support
- Unsupported-construct diagnostics are deterministic for the current explicit known-construct set in `rules-mvp`
- Richer reject explanations beyond the current structured heuristic baseline plus explicit conflict evidence triples, ranked commit-delta attribution, and broader unsupported-construct coverage remain active work
- EL/RL expansion beyond current `rules-mvp` baseline
- Current bounded OWL standards slice in `rules-mvp` covers:
- `owl:equivalentProperty` / `owl:equivalentClass`
- `owl:inverseOf`
- bounded `owl:SymmetricProperty` / `owl:TransitiveProperty`
- bounded binary `owl:propertyChainAxiom` over named properties with named-node RDF lists
- bounded `owl:ReflexiveProperty` inference over observed named resources
- bounded explicit `owl:sameAs` canonical equality handling
- bounded equality entailment from `owl:FunctionalProperty` and `owl:InverseFunctionalProperty`
- bounded `owl:differentFrom` consistency rejection against the effective equality closure
- bounded `owl:AllDifferent` expansion into pairwise `owl:differentFrom` constraints
- bounded `owl:Nothing` consistency rejection over effective types
- bounded `owl:AllDisjointClasses` and `owl:AllDisjointProperties` expansion into the existing class/property disjointness gates
- bounded property consistency gates for `owl:IrreflexiveProperty`, `owl:AsymmetricProperty`, and `owl:propertyDisjointWith`
- deterministic unsupported diagnostics for the remaining out-of-scope construct set
- W3C protocol conformance harness in CI
- Benchmark suite for query/update/reasoning paths
- Protocol-compatibility harness covers query parity, update-effect parity, graph-store read/head/delete/put/post-effect parity, a bounded graph-store failure-semantics slice, and bounded query/update failure semantics (status/content-type/body-class) against Fuseki-style dataset endpoints
- workload-pack runs now emit a canonical `pack-report.json` index so one parity run can be treated as a coherent evidence unit instead of a loose set of per-suite report files

### R6: Enterprise Expansion

- Policy hardening beyond the current `bearer-static` + bounded `bearer-jwt` + bounded `oidc-introspection` + bounded proxy-terminated `mtls` baseline (authn/authz depth, quotas, governance)
- Cluster-readiness interfaces
- SLO and release governance maturation
- Operator UX hardening for enterprise workflows (auditable actions, safe defaults, policy-aware feature visibility)
- Fuseki-to-NRESE migration playbooks and rollback runbooks are validated in pilot execution
- Backup/restore drill cadence is institutionalized and evidence-backed

## Acceptance Matrix

### Functional

- Query/update/data operations behave per documented contracts.
- `ASK` remains standard SPARQL query semantics, while `TELL` remains a distinct assertion-ingest contract.
- Errors are deterministic and typed.
- Default and named graph semantics are preserved.
- Operator workflows (query, update, graph management, diagnostics) run through documented public contracts.
- User-facing browser workflows run through documented public contracts and remain usable with AI assistance disabled.
- Browser and CLI access paths reuse the same frontend-owned typed client contract.

### Performance

- Baseline load profiles and target thresholds defined.
- Regression thresholds enforced in CI benchmarks.
- Reasoning overhead is measurable and bounded by mode.
- Operator UI and API responsiveness budgets are defined for administrative workflows.
- Default release optimizes for transactional query/update latency plus synchronous consistency gating.
- Replacement-grade performance and protocol claims require a passing production workload parity pack with reproducible report artifacts.
- Replacement-grade parity work should start with cataloged small real-world ontologies before moving to broader ontology suites.

### Reliability

- Commit/rollback semantics are test-covered.
- Recovery behavior is documented and validated.
- No partial publish under failure conditions.
- Durable mode restart tests confirm no loss of committed data and readiness reports accurate recovery state.
- Backup export and restore import complete without data loss on the target deployment mode.
- Post-restore readiness reaches `ready` and revision continuity is verified.
- Interrupted update recovery shows no partial publish.
- Failed mutation paths keep revision stable and leave previously committed data queryable.
- Graph replace with malformed RDF is atomic and does not clear previously committed data.
- Each drill run produces evidence artifacts defined in [docs/ops/backup-restore-drills.md](../ops/backup-restore-drills.md).

Drill execution procedure and evidence format are normative in [docs/ops/backup-restore-drills.md](../ops/backup-restore-drills.md).

### Security and Governance

- Endpoint authorization matrix is enforced and test-covered for the implemented auth modes, including bounded `bearer-jwt` and bounded proxy-terminated `mtls`.
- Sensitive payload handling has explicit logging policy.
- Release gates require security and conformance checks.
- External operator exposure requires explicit policy controls and hardened transport configuration.
- Policy defaults remain benchmarked and documented against standard reverse-proxy / API hardening practice.
- HTTP-level policy contracts for auth, payload ceilings, and rate-limit baselines stay in dedicated server integration tests instead of being folded into unrelated protocol tests.

### Maintainability

- No module grows into unmanaged monolithic complexity.
- New capabilities ship with tests, docs, and ownership.
- Cross-crate boundary changes require spec updates.
- Server code does not bypass store internals through raw backend access.
- Operator frontend behavior stays API-driven and spec-aligned, preventing undocumented divergence.
- Frontend endpoint ownership, runtime config, and CLI wrappers stay in the frontend package instead of leaking transport helpers into backend modules.

## Risk Register

- OWL 2 DL full target remains a high-complexity effort.
- Over-optimizing early may lock in poor abstractions.
- Insufficient conformance testing can cause protocol drift.
- Operational debt can outpace feature delivery if unchecked.

## Governance Rules

- No stage is complete without passing acceptance checks.
- Deferred features are tracked explicitly, not silently dropped.
- Architectural shortcuts that bypass module boundaries are rejected.
- Replacement-readiness claims must be justified against the Fuseki replacement gap matrix, not inferred from partial feature presence.
