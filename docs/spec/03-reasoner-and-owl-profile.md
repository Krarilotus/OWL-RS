# Reasoner and OWL Profile

## Objective

Build a Rust-native reasoning subsystem that scales from practical enterprise closure to advanced OWL capabilities with explicit profile and support boundaries.

## Design Principles

- Clear profile declarations over implicit behavior
- Typed run reports, metrics, and diagnostics
- No silent fallback for unsupported constructs
- Strict separation between asserted and inferred knowledge
- Hard reject on confirmed inconsistency in the default commit path
- Fast heuristic diagnostics in-path, deeper justifications on demand
- Snapshot-keyed memoization for reusable reasoning preparation artifacts, with explicit invalidation on snapshot-content change
- Runtime behavior is configured outside the reasoner crate; typed feature policy lives in `nrese-reasoner`, while env parsing lives in `nrese-server`

## Reasoning Capability Tiers

### Tier R0: Disabled

- No inference
- Contracts, plans, and reports still available for observability

### Tier R1: Rules MVP

- Deterministic closure over documented subset
- Basic consistency checks
- Reproducible benchmark fixtures
- Current implemented baseline:
- named-node `rdfs:subClassOf` transitive closure
- named-node `rdfs:subPropertyOf` transitive closure
- property propagation across effective subproperty hierarchies
- `rdf:type` propagation across subclass hierarchies
- `rdfs:domain` and `rdfs:range` driven type inference over effective properties
- bounded OWL rule support for `owl:equivalentClass`, `owl:equivalentProperty`, `owl:inverseOf`, `owl:SymmetricProperty`, and `owl:TransitiveProperty`
- bounded explicit `owl:sameAs` support with canonical equality handling over named resources
- bounded identity entailment from `owl:FunctionalProperty` and `owl:InverseFunctionalProperty`, producing canonical effective equality links over named resources
- bounded `owl:ReflexiveProperty` support over observed named resources, producing effective self-loop assertions for the declared reflexive properties
- bounded `owl:propertyChainAxiom` support for named properties over well-formed named-node RDF lists of length 2 that are visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDifferent` expansion into pairwise effective `owl:differentFrom` constraints over RDF list members visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDisjointClasses` expansion into pairwise class disjointness constraints over RDF list members visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDisjointProperties` expansion into pairwise property-disjointness constraints over RDF list members visible through the current named-node-oriented snapshot boundary
- commit-path `owl:Nothing` conflict detection over effective types, including named classes whose taxonomy closes to `owl:Nothing`
- commit-path `owl:disjointWith` conflict detection over effective types
- commit-path `owl:differentFrom` conflict detection against the effective `owl:sameAs` equality closure, including equality implied by bounded functional / inverse-functional property semantics
- commit-path consistency checks for `owl:IrreflexiveProperty` self-loops, `owl:AsymmetricProperty` reverse-pair collisions, and `owl:propertyDisjointWith` assertion collisions
- Shadow-store validation before publish so rejected mutations do not leak into the live dataset
- deterministic unsupported-construct diagnostics for explicitly known out-of-scope OWL constructs in `rules-mvp`
- structured reject explanation payloads with heuristic blame assignment, explicit conflict evidence triples, and ranked commit-delta attribution for the current bounded reject set
- staged-mutation delta inspection so reject reports can surface a likely commit-local trigger triple and ranked candidate set when the previewed mutation makes that isolation possible
- snapshot-keyed memoization of prepared `rules-mvp` artifacts and inference output over the current asserted-only, graph-flattened, named-node-oriented reasoning snapshot so repeated runs over identical snapshot content can reuse indexed closures and consistency inputs instead of rebuilding them from scratch
- schema-keyed memoization of TBox-stable `rules-mvp` preparation artifacts so ABox-only changes can reuse taxonomy and schema-closure preparation while still recomputing ABox-sensitive equality, property-closure, effective-type, and consistency stages
- bounded multi-entry execution and schema caches so alternating snapshot/workload patterns can reuse more than only the immediately previous run
- runtime cache telemetry for execution/schema reuse is now explicit, typed, and exposed through operator diagnostics plus Prometheus metrics instead of only note strings
- prepared property-consistency indexing over effective assertions for constrained predicates so irreflexive, asymmetric, functional, inverse-functional, and property-disjoint checks share one grouped ABox view per run instead of rescanning closure data separately
- schema-cached property-constraint planning so the set of predicates participating in property-characteristic consistency gates is prepared once with other TBox-stable artifacts and reused by ABox-sensitive consistency indexing
- schema-cached class-consistency preparation so `owl:Nothing` and disjoint-type checks reuse precomputed class-side constraints instead of rebuilding schema-side inputs per run
- externally configurable `rules-mvp` feature policy so closure, consistency, equality, and unsupported-diagnostic behavior can be switched at runtime without changing reasoner code
- externally visible resolved reasoner profile/tier metadata so the active bounded RDFS/OWL slice is inspectable through runtime diagnostics and general runtime payloads instead of being inferred from feature flags alone
- official ontology-backed fixtures now validate the current bounded `rules-mvp` slice on FOAF, W3C Time, W3C ORG, SKOS, PROV-O, DCAT, vCard, DCMI Terms, SOSA, SSN, and ODRL; these fixtures are evidence for implemented reasoning slices, not a claim of full ontology semantic completeness

### Current Snapshot Coverage Boundary

- the current mutation-time reasoning snapshot is `asserted-only`
- supported indexed triples must currently have named-node subjects and named-node objects
- triples with blank-node subjects, blank-node objects, or literal objects are skipped and counted explicitly in runtime diagnostics
- named-graph quads are currently flattened into the asserted triple snapshot, so graph names do not yet participate in reasoner indexing
- these boundaries are observable through runtime/operator diagnostics and are an explicit precursor to the later quad-aware snapshot/index track

### Tier R2: Enterprise Rule Expansion

- Broader RDFS/OWL-RL-style practical rules
- Improved invalidation boundaries
- Incremental refresh planning hooks
- cache-friendly preparation boundaries for indexed dataset state, equality clusters, taxonomies, property closure, effective types, and consistency fast-path inputs
- explicit invalidation layers:
- full-snapshot cache key for exact dataset-state reuse
- schema/TBox cache key for partial reuse under ABox-only change
- bounded multi-entry cache residency for hot repeated workloads, instead of a single last-entry cache
- next optimization target after the current prepared property-consistency index:
- delta-aware invalidation for ABox-sensitive consistency preparation so localized changes can refresh only affected property-group slices
- Transaction-oriented consistency gate for commit-time validation
- Next bounded implementation block target:
- unsupported-construct diagnostics coverage expansion beyond the current deterministic known-construct list
- broader commit-blame and deeper minimal-justification trails beyond the current structured heuristic baseline, explicit conflict evidence triples, and ranked preview attribution for rejected updates
- broader OWL-RL / EL-oriented rules beyond the current RDFS + bounded OWL-property baseline

### Current `rules-mvp` Support Boundary

- Supported inference:
- `rdfs:subClassOf`
- `rdfs:subPropertyOf`
- `rdf:type` propagation
- `rdfs:domain`
- `rdfs:range`
- `owl:equivalentClass`
- `owl:equivalentProperty`
- `owl:inverseOf`
- `owl:ReflexiveProperty` over observed named resources
- `owl:SymmetricProperty`
- `owl:TransitiveProperty`
- bounded binary `owl:propertyChainAxiom` over named properties and well-formed named-node RDF lists visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDifferent` over RDF list members visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDisjointClasses` over RDF list members visible through the current named-node-oriented snapshot boundary
- bounded `owl:AllDisjointProperties` over RDF list members visible through the current named-node-oriented snapshot boundary
- explicit `owl:sameAs` canonical equality handling (bounded to named-node resources)
- bounded `owl:FunctionalProperty` equality entailment
- bounded `owl:InverseFunctionalProperty` equality entailment
- Supported hard consistency gates:
- `owl:disjointWith`
- `owl:differentFrom` against the effective equality closure
- `owl:Nothing`
- `owl:IrreflexiveProperty`
- `owl:AsymmetricProperty`
- `owl:propertyDisjointWith`
- Deterministic unsupported diagnostics:
- `owl:propertyChainAxiom` beyond binary chains or over malformed / unsupported RDF list encodings
- malformed RDF lists attached to `owl:AllDifferent`, `owl:AllDisjointClasses`, or `owl:AllDisjointProperties`
- `owl:allValuesFrom`
- `owl:someValuesFrom`
- `owl:onProperty`
- `owl:hasValue`
- `owl:unionOf`
- `owl:intersectionOf`
- `owl:complementOf`
- `owl:oneOf`
- OWL cardinality restrictions

### Tier R3: Hybrid DL Path

- Constraint-driven consistency workflows
- Partial tableaux-oriented satisfiability checks
- Structured explanation anchors
- OWL 2 EL-oriented classification path for large terminology workloads

### Tier R4: OWL 2 DL Target

- Wider class expression handling
- Property characteristics and cardinality-centric reasoning
- Advanced consistency and model checks
- Rich explanation and trace support

## Profile Contracts

Each profile must declare:
- Supported features
- Unsupported features
- Runtime guarantees
- Expected failure semantics
- Performance envelope assumptions

Primary target architecture:
- Operational track: OWL-RL/RDFS-style incremental rule closure for query/update performance
- Classification track: OWL 2 EL-oriented taxonomy/classification acceleration
- Deep validation track: selective DL-oriented consistency and explanation workflows

## Execution Contracts

- Plan generation must be explicit and typed.
- Run report must include:
- status
- profile/mode identity
- revision target
- metric summary
- diagnostic notes
- Commit-time inconsistency checks must block publication when confirmed
- Reject reports must identify the highest-confidence commit-local cause where possible

## Validation Requirements

- Fixture suites for TBox normalization, ABox closure, and consistency outcomes
- Cache-key and invalidation fixtures for repeated runs over identical and changed snapshot content
- Differential tests between profiles where applicable
- Regression detection for inferred triple drift
- Explanation fixtures for heuristic blame assignment and on-demand deeper justifications

## Acceptance Criteria

- Reasoning mode is explicitly selectable and observable.
- External reasoner inputs resolve to one runtime profile/tier contract that diagnostics and runtime payloads share.
- The current read model is explicitly `asserted-only`: reasoning affects commit acceptance and diagnostics, not the default query result set.
- Unsupported constructs return deterministic diagnostics.
- Inference output remains isolated from asserted source facts.
- Runtime reports expose enough data for operations and audit trails.
- Default production path prioritizes transactional query/update performance plus hard consistency gating.
- Explanation model is hybrid: fast commit-path diagnostics plus deeper justification retrieval on demand.
- `rules-mvp` must reject disjoint effective type conflicts with stable diagnostics and no partial publication.
- Reject-path diagnostics must be emitted from a single structured source that can drive both HTTP problem responses and operator diagnostics.
- If a staged update preview yields a high-confidence trigger triple, reject-path diagnostics should surface it consistently in both HTTP and operator channels.
