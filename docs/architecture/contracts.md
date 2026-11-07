# Cross-cutting contracts

This page is a source reference at engine-v2 `a3453435` (7 November 2026). [ARCHITECTURE.md](../ARCHITECTURE.md) owns the layer model and crate
boundaries; [ADRs](../adr/README.md) own design decisions. Change a shared contract at its owner, then migrate callers. This inventory does not approve
API changes or classify the listed bypasses as defects.

Tracking links below refer to the local Harness [contract map][map], whose row IDs identify inspected sites. Its [consolidation note][backlog] owns
work orders. Line numbers refer to the baseline above; proposed APIs and guards are not implemented contracts.

## Cancellation

**Owner:** `nrese_exec::stop::{StopSource, StopCheck, Ticker, Stop}` in `crates/nrese-exec/src/stop.rs:65`, :104, :126 and :28; contract at :1. Use one
source per request and one ticker per sequential loop or parallel task. Tick attempted work, check before and after opaque calls, and finish before
accepting results. The interval is 4096 work units per executing task; one large tick does not bound the opaque work inside it. Retain the original
stop reason when adapting bool callbacks. See [execution-core](../design/execution-core.md).

Known bypasses ([C01–C24][c]):
- At this baseline, no production loop uses `Ticker`; `Resources::stop` at `crates/nrese-sparql/src/query.rs:37` only creates a source. Local cadence
  and callback adapters remain in exec, DL, SPARQL, store and SHACL (C01–C20).
- Reasoner `walk`/`walk_until` omit attempted-work polling (`crates/nrese-reasoner/src/eval.rs:321`, :350; C21–C22).
- Graph output has unbounded inner enumeration or no stop argument (`crates/nrese-exec/src/graph.rs:155`, :118; C23–C24).

## Memory

**Owner:** `nrese_exec::budget::{SharedBudget, Envelope, Budget, Reservation, ChargedVec, Gauge}` (`crates/nrese-exec/src/budget.rs:8`); `MemoryWatch`
observes process memory (`crates/nrese-exec/src/memory.rs:297`). Reserve before allocation, retain the reservation with memory, free before release,
and account old-plus-new transient buffers and nested owned payloads. Request parts share one envelope under the shared root. `Gauge` estimates and
sampled `MemoryWatch` are not exact pre-allocation admission. See [resource lifetimes](../design/execution-core.md#execution-and-resource-lifetime).

Known bypasses ([M01–M23][m]):
- Upload parsers return uncharged `Vec<Quad>` (`crates/nrese-store/src/rdf_io.rs:59`, :76, :89, :101); HTTP input-byte limits do not bound decoded
  growth (M01–M04).
- Store collections, access encoders, U1 facts, vector growth and SHACL class lists have separate retention/admission boundaries (M05–M15, M19–M20).
- Store/DL adapters use manual or sampled estimates (M16–M18); DL explicitly documents sampled accounting in `crates/nrese-dl/src/memory.rs:1`.
- `Solutions` and graph outputs do not carry their retained charge (M21–M23).

## Lock order

**Owners:** `Engine`, `Transaction`, `Versions` in `crates/nrese-engine/src/engine/mod.rs:21`; `Dictionary` in
`crates/nrese-engine/src/term/dictionary.rs:24`. Follow writer → checkpoint → compaction → WAL → current version → dictionary; inside the dictionary,
one index → inner → order → derived bookkeeping, never two locks of one level. Rayon workers use fallible slot admission rather than waiting;
compaction holders run serial work. Vector filters use `SearchInput` after index locks are released (`crates/nrese-engine/src/term/vectors.rs:480`).

Known bypasses and gaps ([L01–L05][l]):
- `with_view` runs a callback under inner read (:1437); `with_views` has explicit restrictions (:1448). No in-tree interning callback was found (L01).
- Scan, prefix order, text order and pending numbering retain guards across parallel work (:1161, :1252, :1294, :1533; L02–L05).
- `Bounds` state/view mutexes (`crates/nrese-store/src/dl/bounds.rs:65`) have no documented place in the combined hierarchy. This is not a reproduced
  deadlock; speculative transactions do not acquire the writer slot.

## Durability

**Owners:** `Durable` (`crates/nrese-engine/src/durability/mod.rs:105`), `Transaction::commit` (`crates/nrese-engine/src/engine/transaction.rs:477`),
`BulkLoad` (`crates/nrese-engine/src/engine/bulk.rs:3`), and application `MutationPipeline`/`MutationTicket`
(`crates/nrese-store/src/mutation/pipeline.rs:36`; `crates/nrese-store/src/mutation/ticket.rs:21`). Ordinary commit builds, appends/syncs WAL, then
publishes. `EveryCommit` survives power loss; `OsBuffered` may lose recent commits after OS/power failure. Checkpoint publication writes aside, syncs,
renames and syncs the directory. Bulk/rematerialisation checkpoint before publication without a large WAL record. If cancellation loses to commit,
await the actual outcome. See [storage ADR](../adr/0002-engine-storage-lsm-permutations.md) and [one engine API](../adr/0007-one-engine-api.md).

Known bypasses ([D01–D04][d]):
- Rebuildable derived files repeat atomic placement mechanics (D01).
- Ontology preload and crate-private `apply_ungated` deliberately bypass public mutation gates (D02–D03); they are internal operations, not endpoint
  authority.
- Bulk publishes data before quarantine commit (D04); joint acknowledgement needs a contract. Post-publication cleanup must not describe committed data
  as uncommitted.

## HTTP errors

**Owners:** `StoreError` cause classification (`crates/nrese-store/src/error.rs:161`, :172, :183) and `ApiError` response rendering
(`crates/nrese-server/src/error.rs:138`). Preserve typed causes and operation context. Query/mutation adapters distinguish shared memory (503), request
memory (413), forbidden (403), request fault (400) and internal fault (500); cancellation and incomplete answers have dedicated meanings. Do not
classify formatted error strings. Transport ownership follows [ARCHITECTURE.md](../ARCHITECTURE.md#3-where-to-fix-what).

Known bypasses ([E01–E05][e]):
- Query and mutation repeat common classification; no universal transport classifier exists at this baseline (E01).
- RDF4J, backup and SHACL retain separate branches (E02–E05). A shared classifier must preserve operation semantics, including query-specific request
  responses.

## Work admission and limits

**Owners:** `Resources` (`crates/nrese-sparql/src/query.rs:14`), `RowLimit` (`crates/nrese-exec/src/join.rs:60`), `Workers`
(`crates/nrese-exec/src/workers.rs:9`), checked narrowing (`crates/nrese-exec/src/narrow.rs:9`), and `RequestLimits`
(`crates/nrese-server/src/policy.rs:83`). Pass one request envelope and cancellation through evaluations; apply row bounds across parallel outputs and
refuse at exact representation boundaries. `Workers::limited` narrows a handle, not nested kernels. QL `Work` (`crates/nrese-owl/src/ql/witness.rs:33`)
and rewrite `Budget` (`crates/nrese-owl/src/ql/rewrite.rs:122`) own domain-specific deterministic work.

Known bypasses ([W01–W02][w]):
- WCOJ counts emitted rows locally; native joins derive remaining-byte row limits and synthetic refusal locally. Output limits do not bound
  unsuccessful probes.
- No general admission `Work` type exists; QL, solver and kernel units remain distinct.

## Datatypes

**Owner:** `nrese-xsd`, especially `owl::{Datatype, Value, ValueSet, Rational}` (`crates/rdf/nrese-xsd/src/owl/mod.rs:20`); `Value::parse` is at
`crates/rdf/nrese-xsd/src/owl/value.rs:56`. Reuse lexical parsers and datatype facts; preserve exact OWL identity and ill-typed versus unsupported
distinctions. The DL caller is `crates/nrese-dl/src/datatypes/ranges.rs:217`. Keep SPARQL promotion and canonical-only inline packing/RDF term identity
separate. See [RDF bundle](../design/rdf-bundle.md) and [DL design](../design/owl2-dl.md).

Known bypasses ([X01–X12][x]):
- SHACL lexical grammar/bounds, SPARQL derived-type tables and inline validation repeat datatype facts (X01–X08). `Value` alone cannot validate
  arbitrarily long valid-but-unrepresentable lexical values.
- RL's private `Data`, literal conversion and membership duplicate OWL value semantics (`crates/nrese-reasoner/src/engine/datatypes.rs:29`, :90, :128;
  X09–X12).

## Reasoner verdicts

**Owners:** store `Verdict/Checked` (`crates/nrese-store/src/dl/consistency.rs:14`, :42), query `Completeness/Regime`
(`crates/nrese-sparql/src/completeness.rs:12`, :50), tableau `Answer` (`crates/nrese-dl/src/tableau/mod.rs:229`) and beyond `Verdict`
(`crates/nrese-dl/src/beyond/mod.rs:65`). Consistent requires established consistency; unknown includes unsupported or exhausted runs. Completeness
means all certain answers under the regime; soundness means each returned answer is certain. `ReasonerRunStatus::Completed`
(`crates/nrese-reasoner/src/capability.rs:33`) is lifecycle telemetry. Preserve reasons and revision/regime identity; map tableau Unsupported/GaveUp to
Unknown (:158 in consistency.rs), lower guarantees for unknown consistency, and refuse exact answers without completeness. See [reasoning
semantics](../spec/reasoning-semantics.md).

Known bypasses ([V01–V02][v]):
- Draft reports derive consistency from RL violation count (`crates/nrese-store/src/draft_check.rs:441`; V01).
- Classification defaults to consistent while recording undecided classes (`crates/nrese-dl/src/classify/driver.rs:230`; V02). These belong to the
  existing consistency-producer work, not serializer inference.

## Dataset and bench inputs

**Owners:** Python case registry `suitekit.cases::{load, load_at, problems}` (`benches/suite/suitekit/cases.py:63`, :79, :97), frozen dataset `Store`
(`benches/suite/suitekit/datasets.py:58`), result `Result` (`benches/suite/suitekit/schema.py:38`), and prepared-run inputs
(`benches/fast/fast.py:1486`). Load common case identities, use `load_at` for historical jobs, retain frozen bytes/hash/provenance/source version,
prepare once with sidecars, and verify each pass. Size checks are not full hash verification. See [case schema](../../benches/cases/README.md) and
[benchmark protocol](../../benches/PROTOCOL.md).

Known bypasses ([T01–T07][t]):
- Datalog/ORE kits have separate case manifests; registry coverage is incomplete (T01–T02).
- Catalogue readers, query-pin readers and input-spec expansion repeat boundary logic (T03–T07). Keep kit-specific generation in adapters; no unified
  parsed `InputSpec` exists at this baseline.

[map]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md
[backlog]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-v15-consolidation.md
[c]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#1-cancellation-and-stop-polling
[m]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#2-memory-budget-and-charging
[l]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#3-lock-order
[d]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#4-durability-and-commit-points
[e]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#5-error-mapping-to-http
[w]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#6-work-admission-and-limits
[x]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#7-datatype-parsing-and-value-space
[v]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#8-reasoner-verdict-complete--undecided--consistent
[t]: file:///C:/Users/Johannes/Desktop/Harness/notes/2026-11-07-contract-map.md#9-dataset-and-bench-registry-inputs
