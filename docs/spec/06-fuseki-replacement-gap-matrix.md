# Fuseki Replacement Gap Matrix

## Purpose

This document translates the replacement criteria for "NRESE as a real Apache Fuseki replacement" into an explicit readiness matrix.
It exists to prevent ambiguity between:

- interesting prototype capability
- production-capable semantic server
- fully validated Fuseki replacement

## Status Legend

- `done`: implemented and already backed by tests/docs in the current codebase
- `partial`: meaningful implementation exists, but replacement-grade validation or completeness is still missing
- `missing`: required for replacement readiness but not yet implemented to a meaningful degree

## 1. SPARQL and Fuseki Protocol Coverage

Status: `partial`

Current coverage:
- Query endpoint exists and handles `SELECT`, `ASK`, graph-producing queries, content negotiation, and error responses
- Update endpoint exists and is wired through the reasoner-aware update pipeline
- A first-class `TELL` assertion-ingest endpoint exists for RDF payloads on the same staged reasoner-aware publish path as update
- Graph Store HTTP endpoint exists for default and named graph operations including `GET`, `PUT`, `POST`, `DELETE`, and `HEAD`
- Graph Store writes/deletes and admin restore now also converge on the same reasoner-aware mutation pipeline as update and `TELL`, so semantic reject behavior, revision publication, and runtime bookkeeping do not drift by write entry point
- Service description, health, readiness, version, metrics, and operator APIs exist
- Protocol compatibility harness now covers query parity, update-effect parity, graph-store read/head/delete/put/post-effect parity, bounded graph-store failure semantics, and bounded query/update failure semantics (status/content-type/body-class) against Fuseki-style dataset endpoints
- graph-producing parity comparators now canonicalize Turtle, N-Triples, and RDF/XML payloads onto one triples-set comparison path
- The generic protocol fixture set now also covers a broader SPARQL Update parity slice for `DELETE DATA`, `DELETE/INSERT WHERE`, `CLEAR`, `COPY`, `MOVE`, and `ADD`
- query parity coverage now also includes a dedicated limit/offset semantics suite using bindings-set comparison instead of count-only summaries
- a staged real-world ontology catalog now exists to harden parity runs incrementally from small to broad official ontology sources, with typed serialization/dialect/reasoning/service metadata
- official ontology fixtures now back explicit cross-service checks for store preload, `tell`, graph-store roundtrip, and bounded reasoner/runtime validation, including RDF/XML ingest/preload coverage
- official ontology-backed service tests now also cover SKOS RDF/XML graph-store roundtrip on the real HTTP surface
- ontology-backed compat/parity suites are now grouped on a dedicated harness path and baseline packs now carry ontology-specific suites across the checked-in official catalog set on top of the shared ontology baseline suite
- the harness now supports catalog-driven `pack-matrix` execution over baseline ontology packs, producing an aggregate `pack-matrix-report.json` evidence index for a selected catalog tier
- `pack-matrix` execution can now also be filtered by typed ontology metadata such as semantic dialect, reasoning feature, and service coverage, so catalog metadata drives targeted parity evidence instead of only catalog descriptions
- `pack-matrix` can now also be scoped to one exact ontology name on the same selector path, so secured live replacement runs can narrow to a single official ontology without cloning pack manifests
- `pack-matrix` now also validates catalog-backed baseline packs before execution so dataset identity and required compat-suite coverage cannot drift silently from the ontology catalog
- the harness now also exposes `pack-validate` as a dedicated preflight command so live connection-profile wiring and invocation-profile references can be verified before seed/compat/bench execution
- workload-pack execution now has explicit `full` and `compat-only` modes so real secured parity runs can compare existing deployments without forcing seed or benchmark stages on the same path
- a reusable live connection-profile registry now exists for secured parity runs, owning NRESE/Fuseki base URLs, auth, timeout defaults, and invalid-auth invocation profiles outside workload-pack manifests
- versioned secured live-auth and secured live-auth-plus-timeout workload-pack templates now exist on the same manifest path as the generic packs and bind to that connection-profile registry instead of carrying live transport settings directly
- secured workload-pack compat suites are now preflight-validated against the selected connection profile plus pack-local invocation profiles, so profile drift fails before seed/bench execution
- the harness now exposes that preflight as a first-class `pack-validate` step with optional machine-readable report output for CI and deployment workflows
- workload-pack execution now emits a canonical `pack-report.json` evidence index that ties compat suites and benchmark artifacts together
- local live side-by-side parity has now been demonstrated against a local Apache Jena Fuseki 6.0.0 install on official FOAF, ORG, SKOS, Time, SOSA, DCAT, vCard, DCMI Terms, PROV-O, SSN, and ODRL packs, with report artifacts emitted from the same `pack-matrix` / `pack-report.json` path used for the generic harness runs
- graph-result parity now canonicalizes blank nodes before triples-set comparison, so official ontology `CONSTRUCT` cases compare graph isomorphism instead of raw blank-node labels
- official ontology live seeding now supports explicit RDF base IRIs through `Content-Location` and harness-side `dataset_base_iri`, so relative-IRI vocabularies such as PROV-O run on the same live Graph Store parity path as the rest of the catalog

Still required:
- formal replacement-grade validation against the live Fuseki deployment
- complete documented coverage for the intended SPARQL 1.1 Update operation set in target scope
- stronger compatibility checks for timeout, limit, and edge-case semantics
- broader timeout/limit/error-semantic coverage beyond the current graph-store failure baseline; bounded invalid-auth and oversize-payload fixture support now exists, but live validation on the real project workload is still required
- broader query/update failure coverage beyond the current invalid-syntax fixture slice
- explicit live replacement evidence for `TELL` as a project-level ingestion contract distinct from Graph Store and raw SPARQL Update usage
- a project-specific production workload parity pack, with versioned seed/workload/case inputs and reproducible report artifacts as defined in `docs/ops/benchmark-and-conformance.md`
- one machine-readable replacement evidence index so verified ontology parity is not tracked only through scattered artifact directories and prose

Replacement gate:
- NRESE reproduces the expected results and stable error semantics for the project's real query/update set against the same dataset used in Fuseki

## 2. Production-Ready Reasoner

Status: `partial`

Current coverage:
- reasoning modes are explicit and observable
- update path no longer bypasses reasoner orchestration semantics
- consistency gate contract and explanation architecture are specified
- hybrid target architecture is now fixed: RL operational track + EL classification track + deeper DL validation track
- `rules-mvp` now performs real named-node `rdfs:subClassOf` and `rdfs:subPropertyOf` closure, property propagation, and `rdf:type` / `rdfs:domain` / `rdfs:range` type inference, with live snapshot capture from the store
- `rules-mvp` now includes a bounded OWL standards slice for `owl:equivalentClass`, `owl:equivalentProperty`, `owl:inverseOf`, `owl:SymmetricProperty`, and `owl:TransitiveProperty`
- `rules-mvp` now includes bounded explicit `owl:sameAs` support via canonical equality handling for named resources, plus bounded equality entailment from `owl:FunctionalProperty` and `owl:InverseFunctionalProperty`
- `rules-mvp` now includes bounded binary `owl:propertyChainAxiom` support over named properties with well-formed named-node RDF lists, and deterministic diagnostics for longer or malformed chains
- `rules-mvp` now includes bounded `owl:ReflexiveProperty` inference over observed named resources and commit-path `owl:differentFrom` consistency rejection against the effective equality closure
- `rules-mvp` now expands bounded `owl:AllDifferent`, `owl:AllDisjointClasses`, and `owl:AllDisjointProperties` declarations over RDF list members into the existing equality, class-disjointness, and property-disjointness consistency gates
- `rules-mvp` now rejects `owl:Nothing` and `owl:disjointWith` conflicts over effective types and keeps rejected updates out of the live dataset through shadow-store validation
- `rules-mvp` now rejects bounded property-characteristic conflicts for `owl:IrreflexiveProperty`, `owl:AsymmetricProperty`, and `owl:propertyDisjointWith`
- `rules-mvp` now emits deterministic diagnostics for a known set of unsupported OWL constructs, with the remaining unsupported constructs tracked in the reasoner support boundary spec
- official ontology-backed reasoner fixtures now validate bounded subclass, inverse-property, transitive-property, subproperty, equivalent-property, and domain/range slices on FOAF, W3C Time, W3C ORG, SKOS, PROV-O, DCAT, vCard, DCMI Terms, SOSA, SSN, and ODRL; bounded property-chain support remains covered by dedicated rule tests because the official SOSA/SSN encodings use broader RDF-list forms than the current named-node bounded slice
- operator diagnostics can expose the latest reasoning run baseline
- reject-path HTTP problem responses and operator diagnostics now share a structured heuristic blame payload for consistency failures
- reject-path HTTP problem responses and operator diagnostics now also share explicit conflict evidence triples from the reasoner-owned explanation model
- staged update previews now allow reject-path responses and operator diagnostics to surface a likely commit-local trigger triple and ranked attribution candidates when the inserted delta can be isolated heuristically
- rules-mvp cache reuse is now surfaced as typed execution/schema telemetry in reasoning diagnostics and Prometheus metrics
- rules-mvp runtime diagnostics now also surface the configured semantic tier for the active preset, so bounded RDFS vs bounded OWL slices are externally visible

Still required:
- broader unsupported-construct coverage and stronger explainability beyond the current deterministic known-construct list
- deeper minimal-justification and commit-delta blame beyond the current heuristic baseline, explicit conflict evidence triples, and bounded ranked preview attribution
- on-demand deeper justifications beyond the synchronous heuristic path
- broader EL/RL rule coverage beyond the current RDFS + bounded OWL-property baseline
- an explicit replacement-grade read model for inferred state instead of relying primarily on write-time validation and diagnostics
- a fuller RDF dataset/snapshot model for reasoning, especially around named-graph and term coverage

Replacement gate:
- inferred knowledge is materially produced, isolated from asserted knowledge, and validated through reproducible fixtures

## 3. Persistence and Data Safety

Status: `partial`

Current coverage:
- revision tracking exists
- durable storage path exists behind the `durable-storage` feature
- store-level backup/export and restore/import flows exist with admin-scoped HTTP control paths
- metrics, readiness, and update-path contracts are in place
- failed store/server mutation paths are test-covered for revision stability and no partial publication on parse/reasoner failure
- Graph Store semantic rejects and restore semantic rejects are now HTTP-test-covered on the same shared mutation gate as update and `TELL`

Still required:
- restart-after-restore validation on the target durable mode
- crash/interrupted-operation recovery test coverage
- rollback/no-partial-publish guarantees across failure scenarios
- drill evidence artifacts captured per [docs/ops/backup-restore-drills.md](../ops/backup-restore-drills.md)

Replacement gate:
- no in-memory-only dependency remains for production use, and all listed persistence checks are automated or drill-backed and reproducible

## 4. Operator and Admin Surface

Status: `partial`

Current coverage:
- `/ops` UI exists
- `/console` user-facing frontend exists for query, tell, update, graph, and dataset workflows
- `/console` now owns an explicit frontend-side TypeScript client boundary plus runtime-configurable backend base URL instead of relying only on implicit same-origin transport assumptions
- `/console` now also ships a small CLI on the same typed client boundary for query, update, tell, graph, runtime, and capability workflows
- `/console` now includes guided workbench examples so non-specialist users can load explicit query/update/tell/graph templates instead of starting from empty editors
- `/console` now supports explicit persisted locale selection for the currently shipped `en`/`de` language set instead of relying only on browser locale detection
- `/console` now reads reasoning preset/policy/cache state from the server diagnostics surface instead of maintaining local pseudo-config state
- `/console` now also exposes the server-advertised reasoning capability set, so configured bounded reasoning slices are inspectable from the user-facing runtime view
- operator APIs expose capabilities, dataset summary, and extended health
- `/version`, service description, and runtime diagnostics now also expose the active deployment posture and the enabled mutation/admin surface set from the same runtime-owned source
- query/update/graph workflows are browser-accessible
- optional server-side AI query suggestions exist behind typed provider config and a dedicated API surface, with provider/model visibility and clearer frontend empty-state handling
- reasoner presets are externally configurable and visible in the user-facing frontend/runtime metadata

Still required:
- operator flows for revisions, diagnostics, and deeper incident support
- documented role model for read-only, writer, and admin operators
- production validation behind reverse proxy and hardened auth policy
- non-demo workflow hardening for everyday operations
- broader user-console workflow evidence and extension/customization guidance under real project usage
- live preset application remains external/config-driven rather than runtime-mutable
- broader i18n coverage beyond the current shipped language set and stronger assistant UX around task templates/history under real project usage
- timeout-oriented parity evidence against real secured stacks is still pending even though generic and policy failure suites are now pack-addressable
- real deployment evidence for standalone frontend hosting against a separately hosted backend is still pending even though runtime-configured API base support now exists

Replacement gate:
- admin and user workflows are reliably executable in-browser behind real deployment controls

## 5. Security and Hardening

Status: `partial`

Current coverage:
- centralized policy layer for auth, endpoint exposure, payload ceilings, and timeout ceilings
- `bearer-static` auth baseline plus bounded `bearer-jwt` support and bounded proxy-terminated `mtls`
- in-memory rate limiting / quota baseline for read, write, and admin request buckets
- request correlation via `X-Request-Id`
- `oidc-introspection` auth now exists as a bounded introspection-backed mode on the same policy/auth path as the existing static bearer, JWT, and proxy-terminated mTLS modes
- file-based `config.toml` runtime configuration now exists with environment overrides on the same parser path as env-only startup, which reduces deployment-specific duplication and keeps runtime opinion external to the implementation
- runtime posture for optional operator/metrics exposure is now derived through one shared server-owned source that feeds guards, capability payloads, service description, and runtime diagnostics
- deployment posture is now explicit and externally configurable, with startup validation for `internal-authenticated` and `replacement-grade` plus runtime-driven read-only mutation gating for `read-only-demo`

Still required:
- broader production auth beyond the current bounded `bearer-jwt`, bounded `oidc-introspection`, and bounded proxy-terminated `mtls` slices, including stronger JWT/OIDC integration depth and direct/in-process mTLS if required
- rate limiting / quota controls
- stronger secrets and deployment hardening validation
- release-time security scanning and documented rollback/security process
- broader startup posture hardening beyond the current validated combinations, especially around external-bind/proxy trust constraints and stronger mTLS deployment contracts

Replacement gate:
- documented, testable, production-grade authn/authz and hardening baseline with minimal-footprint deployment

## 6. Performance and Resource Evidence

Status: `partial`

Current coverage:
- Rust-native benchmark harness exists for reproducible query/update runs
- fixture-based workload definitions exist
- optional NRESE-vs-Fuseki comparison path is implemented
- shared seed dataset and local Fuseki comparison stack definition now exist for reproducible dataset-parity setup
- local compatibility and latency evidence can be emitted via report artifacts
- authenticated local side-by-side runs against a dedicated Fuseki dataset are now supported through optional harness Basic-Auth flags
- compatibility fixtures now support explicit shared per-case request-header overrides through the shared harness request path, which prepares live auth/proxy parity without endpoint-specific compare logic
- production workload packs now support service-level invocation defaults plus named invocation profiles, and live secured parity can select reusable connection profiles instead of copying transport/auth settings into each pack
- production workload packs now also support service-level and named-profile timeout budgets on the same profile path, so secured/live compare runs can keep transport ceilings external to case fixtures
- production workload packs now support multiple compatibility suites in one manifest, so protocol, ontology, and policy/e2e parity can be staged without ad hoc command chaining
- versioned secured live-auth and secured live-auth-timeout pack templates now exist on top of that manifest path and are designed to run with a selected connection profile
- workload-pack invocation headers now support env-placeholder interpolation so authenticated parity runs can be scripted without committed credentials

Still required:
- comparisons against the real Fuseki deployment and real ontology/workload
- broader live parity coverage across the remaining official ontology packs beyond the currently verified small/medium/broad local side-by-side runs
- broader ontology-driven cross-service and reasoner validation on catalog fixtures beyond the current official baseline packs and asserted-schema parity slices now checked across FOAF, ORG, Time, SKOS, PROV-O, DCAT, SOSA, SSN, vCard, DCMI Terms, and ODRL
- latency, startup, RAM, CPU, and reasoning cost measurements
- regression thresholds in CI
- production workload parity pack execution on the real ontology/workload set, rather than only generic local fixtures

Replacement gate:
- measurable and documented runtime evidence supports the migration decision

## 7. Conformance and Compatibility

Status: `partial`

Current coverage:
- local protocol tests exist for selected endpoints and error contracts
- compatibility harness exists with fixture-based equivalence cases
- compatibility harness now includes bounded invalid-syntax parity fixtures for query and update failure paths
- compatibility harness now includes a dedicated timeout-failure suite on the same shared response-semantics comparator path, kept as an opt-in compat suite for stacks with aligned timeout ceilings
- benchmark/conformance documentation and workload fixtures exist
- reproducible seed workflow exists so side-by-side comparisons can start from the same graph state
- local side-by-side comparison against a live Fuseki dataset is now automated through the harness once parity seeding is complete, including auth-protected compare stacks

Still required:
- W3C-oriented harnesses in CI
- side-by-side comparison tests against Fuseki
- documented and intentional semantic deviations, if any
- production workload parity pack coverage for the project-specific query/update/error/auth mix

Replacement gate:
- standards and project-specific compatibility suites run automatically and stay green

## 8. Migration from Fuseki

Status: `partial`

Current coverage:
- migration runbook exists
- pilot migration report template exists
- rollback runbook exists
- backup/restore drill playbook exists

Still required:
- import/export validation for the real dataset
- pilot migration against the live project layout
- signed migration evidence package from a real pilot

Replacement gate:
- one real pilot migration completes successfully with a documented rollback path

## 9. Operational Maturity

Status: `partial`

Current coverage:
- health, readiness, metrics, logs/traces baseline, operator UI, and setup docs exist
- operational readiness handbook exists
- maintenance, rollback, and drill runbooks exist
- runtime config ownership is now split cleanly between code (`crates/nrese-server/src/config/`) and one operator-facing config reference (`docs/ops/config-reference.md`)

Still required:
- incident/recovery documentation
- upgrade and rollback procedures
- ongoing maintenance and ownership model

Replacement gate:
- the service is operable as a product, not just runnable as software

## 10. Minimum Replacement Gates

### Must-Have

- protocol-complete query/update/graph behavior for the project scope
- stable persistent operation
- hardened reverse-proxy/auth/TLS deployment pattern
- functional rule-level reasoning
- benchmark and conformance evidence
- production-grade operator surface

### Should-Have

- richer reasoning profiles
- explanatory diagnostics
- fine-grained roles and governance
- recovery automation
- CI-based security and benchmark gates

### Nice-to-Have

- stronger OWL-DL path
- HA / multi-node patterns
- deeper enterprise governance and operator UX

## Release Rule

NRESE must not be labeled a full Fuseki replacement until every `Must-Have` gate above is either:

- `done`, or
- explicitly signed off as intentionally out of scope for the project's replacement target
