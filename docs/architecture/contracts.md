# Cross-cutting contracts

This page is a source reference at engine-v2 `a3453435` (7 November 2026). [ARCHITECTURE.md](../ARCHITECTURE.md) owns the layer model and crate
boundaries; [ADRs](../adr/README.md) own design decisions. Change a shared contract at its owner, then migrate callers. This inventory does not approve
API changes or classify the listed bypasses as defects.

Line numbers refer to the baseline above; proposed APIs and guards are not implemented contracts.

## Cancellation

**Owner:** `nrese_exec::stop::{StopSource, StopCheck, Ticker, Stop}` in `crates/nrese-exec/src/stop.rs:65`, :104, :126 and :28; contract at :1. Use one
source per request and one ticker per sequential loop or parallel task. Tick attempted work, check before and after opaque calls, and finish before
accepting results. The interval is 4096 work units per executing task; one large tick does not bound the opaque work inside it. Retain the original
stop reason when adapting bool callbacks. See [execution-core](../design/execution-core.md).

Known bypasses:

- `crates/nrese-exec/src/graph.rs:269`: SCC root modulo polling of a bool callback.
- `crates/nrese-exec/src/graph.rs:432`: Reachability component modulo polling.
- `crates/nrese-exec/src/join.rs:91`: Private cancellation flag polling in join `Limit`, beside row accounting.
- `crates/nrese-dl/src/context/links.rs:164`: Poll/observe each 4096 found conclusions.
- `crates/nrese-dl/src/context/rules/hyper.rs:108`: Same 4096-found checkpoint, with detached scratch.
- `crates/nrese-dl/src/datatypes/theory.rs:526`: `STOP_EVERY` assignment counter and bool stop.
- `crates/nrese-dl/src/tableau/engine.rs:489`: Wrapping saturation counter checks time and memory every 4096 steps.
- `crates/nrese-sparql/src/native/mod.rs:5501`: Probe loop polls cancellation every 4096 input rows, then accounts output growth.
- `crates/nrese-sparql/src/native/spatial.rs:1693`: Local 1024-row `Context::check` cadence.
- `crates/nrese-sparql/src/native/wcoj.rs:488`: Private 1024-candidate poll combines sibling cancellation and caller callback.
- `crates/nrese-store/src/dl/query/gap.rs:89`: Lower-row hash construction cancellation every 1024 rows.
- `crates/nrese-store/src/dl/query/gap.rs:108`: Upper-row term collection repeats the same cadence.
- `crates/nrese-store/src/dl/query/gap.rs:131`: Candidate selection repeats the same cadence.
- `crates/nrese-store/src/dl/upper.rs:394`: Local bool stop plus process-memory fallback collapsed to `GaveUp`.
- `crates/nrese-store/src/dl/upper.rs:480`: Collecting all view facts polls each 1024 facts.
- `crates/nrese-store/src/dl/upper.rs:557`: Copying derived triples polls each 1024 triples.
- `crates/nrese-store/src/dl/upper.rs:564`: Equality-member expansion repeats the same cadence.
- `crates/nrese-store/src/support.rs:995`: Support decisions check local `Work::go_on` every 4096 quads.
- `crates/nrese-dl/src/context/engine.rs:350`: Locally composes cancellation, deadline, process-memory sampling and exhausted latch.
- `crates/nrese-shacl/src/validate.rs:179`: Validator has its own cancellation-to-latched-error adapter.
- `crates/nrese-reasoner/src/eval.rs:321`: Recursive `walk` has no stop argument, even while scanning unsuccessful bindings.
- `crates/nrese-reasoner/src/eval.rs:350`: `walk_until` stops only when emit returns true; an already-running scan is passed over, and rejected bindings never reach emit.
- `crates/nrese-exec/src/graph.rs:155`: Transitive output checks once per 256 components; one component can emit quadratically without another poll.
- `crates/nrese-exec/src/graph.rs:118`: Plain closure has no cancellation argument and pushes all reachable pairs.

At this baseline no production loop uses `Ticker`; `Resources::stop` at `crates/nrese-sparql/src/query.rs:37` only creates a source.

## Memory

**Owner:** `nrese_exec::budget::{SharedBudget, Envelope, Budget, Reservation, ChargedVec, Gauge}` (`crates/nrese-exec/src/budget.rs:8`); `MemoryWatch`
observes process memory (`crates/nrese-exec/src/memory.rs:297`). Reserve before allocation, retain the reservation with memory, free before release,
and account old-plus-new transient buffers and nested owned payloads. Request parts share one envelope under the shared root. `Gauge` estimates and
sampled `MemoryWatch` are not exact pre-allocation admission. See [resource lifetimes](../design/execution-core.md#execution-and-resource-lifetime).

Known bypasses:

- `crates/nrese-store/src/rdf_io.rs:59`: `parse_graph` collects an owned `Vec<Quad>` with no envelope.
- `crates/nrese-store/src/rdf_io.rs:76`: `parse_payload` does the same for uploads.
- `crates/nrese-store/src/rdf_io.rs:89`: Preserved-blank-node parser duplicates the uncharged collection.
- `crates/nrese-store/src/rdf_io.rs:101`: Dataset restore parser collects the same way.
- `crates/nrese-store/src/statements.rs:101`: Context fan-out clones every parsed quad into every context then collects, beside the original parse.
- `crates/nrese-store/src/statements.rs:212`: Pattern matching materializes decoded quads in a plain growing vector.
- `crates/nrese-store/src/dl/source.rs:109`: Transaction ontology reader collects all asserted `Statement`s before `nrese_owl::read`, without an envelope parameter.
- `crates/nrese-store/src/dl/source.rs:126`: Snapshot ontology reader repeats the full uncharged collection.
- `crates/nrese-store/src/dl/upper.rs:478`: U1 `view_facts` grows a plain whole-view triple vector; process stop is not an allocation reservation.
- `crates/nrese-store/src/dl/bounds.rs:163`: Inserted/deleted asserted triples collected without a reservation at preparation.
- `crates/nrese-store/src/access/control.rs:203`: Whole before/after encoded access state held in two plain `BTreeSet`s (:204).
- `crates/nrese-store/src/access/control.rs:205`: Removed-state difference collects decoded owned quads.
- `crates/nrese-store/src/access/control.rs:206`: Added-state difference independently repeats that collection.
- `crates/nrese-store/src/access/rdf.rs:82`: Access-state RDF encoder grows a plain quad vector.
- `crates/nrese-store/src/access/queries.rs:141`: Saved-query RDF encoder returns a plain quad vector with owned text.
- `crates/nrese-store/src/support.rs:206`: `Following` hand-maintains mutex reservation/resize/refusal state for capacity callbacks.
- `crates/nrese-dl/src/context/memory.rs:11`: `Observer` maintains its own sampled capacity counters and per-task limit beside a shared `Gauge`.
- `crates/nrese-dl/src/tableau/engine.rs:628`: Solver checks estimated current bytes then `Gauge::follow` (:645); this observes after growth rather than enforcing every allocation beforehand.
- `crates/nrese-engine/src/term/vectors.rs:136`: Vector-space growth parses literals and pushes persistent vectors without a budget parameter; :287 only reports bytes.
- `crates/nrese-shacl/src/validate.rs:143`: Validator precomputes class/subclass lists into a plain map at :159 without reserving this container there.
- `crates/nrese-sparql/src/native/mod.rs:1173`: `Solutions` owns a plain IdTable without carrying its charge; manual produced/consumed accounting elsewhere must stay synchronized.
- `crates/nrese-exec/src/graph.rs:118`: Plain closure produces a growing IdTable with no budget argument.
- `crates/nrese-exec/src/graph.rs:148`: Transitive closure produces a plain Vec of potentially quadratic pairs without reservation; :155 constructs the output.

DL documents sampled accounting in `crates/nrese-dl/src/memory.rs:1`; it is not exact pre-allocation admission.

## Lock order

**Owners:** `Engine`, `Transaction`, `Versions` in `crates/nrese-engine/src/engine/mod.rs:21`; `Dictionary` in
`crates/nrese-engine/src/term/dictionary.rs:24`. Follow writer → checkpoint → compaction → WAL → current version → dictionary; inside the dictionary,
one index → inner → order → derived bookkeeping, never two locks of one level. Rayon workers use fallible slot admission rather than waiting;
compaction holders run serial work. Vector filters use `SearchInput` after index locks are released (`crates/nrese-engine/src/term/vectors.rs:480`).
A commit's note (`Transaction::note_commit`, `crates/nrese-engine/src/engine/writer.rs`) runs with the writer slot, the WAL and, where the
commit converts the stack, the compaction slot held: it takes no lock and calls nothing in the engine, and its journal's sync holds back
compaction, checkpoints and the served log for its length (quarantine readmissions only). State that changes only between commits takes a
`WriterHeld` proof (the store's reasoning certificate: `StoreService::invalidate_held`, `record_reasoning` under `Rematerialisation::finish_held`).

**A version's own slots** (`OwnSlots`, `crates/nrese-engine/src/engine/snapshot/own_state.rs:92`, P2 3.0c) have a leaf lock: held only to look a
value up or store one (:102), never while a value is made or dropped, and nothing is taken while it is held. `make` runs unlocked, so it may read
the snapshot and its other slots. The engine takes it under none of its own locks: an installed version is a value no snapshot held
(`Versions::install`), so nothing drains it.

**The `owl2-dl` bounds (U1):** `Bounds` in `crates/nrese-store/src/dl/bounds.rs:145` (P2-C6b). Its `state` and `view` mutexes guard
serial work only: taking what a read or a commit needs, a revision check, publishing, applying a computed change. The engine's writer slot and
the mutation pipeline's ground cache (`crates/nrese-store/src/mutation/pipeline.rs:227`) come before the mutation-side `state`, which `install`
takes after the commit returns (:363). A build cell's `building` mutex (`crates/nrese-exec/src/cell.rs:145`) comes before the `state`/`view`
check its `kept` closure makes. No path that holds a bounds guard enters the Rayon pool, the engine or the dictionary: U1's change, its
builds, the taxonomy and a view's runs hold none, and a revision's builds go through the `BuildCell` under the bounded rule (a Rayon worker
builds its own instead of waiting). "Before the dictionary" is the guards' place in the hierarchy, not permission to call the dictionary or a
dictionary callback under one. Until P2-C6b, `state` was held across that parallel work. At pool widths 1 and 2 the cycle with pool readers
was reproduced on `a3453435` (red proof `c6b-u1-red-3-94d752b7`).

Known bypasses and gaps:

- `crates/nrese-engine/src/term/dictionary.rs:1437`: Public with_view runs a caller closure while inner.read is held (:1441–1445), beyond the module's named callback exceptions.
- `crates/nrese-engine/src/term/dictionary.rs:1161`: String scan retains inner.read across strings::scan (:1200), whose parallel scan/sort is at `crates/nrese-engine/src/term/strings.rs:225`.
- `crates/nrese-engine/src/term/dictionary.rs:1252`: Prefix path retains inner and order guards through order::extend (:1266), which can enter parallel ordering.
- `crates/nrese-engine/src/term/dictionary.rs:1294`: text_order retains inner/order while sorted_within (:1313) enters parallel work (`crates/nrese-engine/src/term/order.rs:131`).
- `crates/nrese-engine/src/term/dictionary.rs:1533`: Pending numbering and remap execute under upgradable inner read (:1536–1540); numbering is parallel (`crates/nrese-engine/src/term/pending.rs:220`).

## Durability

**Owners:** `Durable` (`crates/nrese-engine/src/durability/mod.rs:105`), `Transaction::commit` (`crates/nrese-engine/src/engine/transaction.rs:477`),
`BulkLoad` (`crates/nrese-engine/src/engine/bulk.rs:3`), and application `MutationPipeline`/`MutationTicket`
(`crates/nrese-store/src/mutation/pipeline.rs:36`; `crates/nrese-store/src/mutation/ticket.rs:21`). Ordinary commit builds, appends/syncs WAL, then
publishes. A journal beside the log that must agree with it (the quarantine's resolutions) is written as the commit's note, before the record,
and holds a record in effect only where the recovered store holds its revision; the log is poisoned wherever a journal may name a revision
no commit published. `EveryCommit` survives power loss; `OsBuffered` may lose recent commits after OS/power failure. Checkpoint publication writes aside, syncs,
renames and syncs the directory. Bulk/rematerialisation checkpoint before publication without a large WAL record. If cancellation loses to commit,
await the actual outcome. See [storage ADR](../adr/0002-engine-storage-lsm-permutations.md) and [one engine API](../adr/0007-one-engine-api.md).

Known bypasses:

- `crates/nrese-engine/src/term/derived.rs:115`: Derived index files repeat write-aside/sync/rename/directory-sync at :137–141.
- `crates/nrese-store/src/loader.rs:40`: Ontology preload commits directly without `MutationPipeline`.
- `crates/nrese-store/src/service.rs:1512`: `apply_ungated` commits without ticket/gates.
- `crates/nrese-store/src/bulk_load.rs:187`: Bulk load has its own final stop check and commits via `finish`; quarantine commit follows at :192.

## HTTP errors

**Owners:** `StoreError` cause classification (`crates/nrese-store/src/error.rs:161`, :172, :183) and `ApiError` response rendering
(`crates/nrese-server/src/error.rs:138`). Preserve typed causes and operation context. Query/mutation adapters distinguish shared memory (503), request
memory (413), forbidden (403), request fault (400) and internal fault (500); cancellation and incomplete answers have dedicated meanings. Do not
classify formatted error strings. Transport ownership follows [ARCHITECTURE.md](../ARCHITECTURE.md#3-where-to-fix-what).

Known bypasses:

- `crates/nrese-server/src/http/mutation.rs:46`: Repeats query adapter's memory/forbidden/request/server classification.
- `crates/nrese-server/src/http/rdf4j.rs:259`: Namespace-change adapter maps every store error to internal.
- `crates/nrese-server/src/http/rdf4j.rs:264`: Separate read adapter recognizes only forbidden versus internal.
- `crates/nrese-server/src/http/admin_dataset.rs:207`: Backup adapter maps all errors to internal.
- `crates/nrese-server/src/http/shacl.rs:122`: Hand-selects evaluation errors for query mapper, then repeats request/internal split.

No universal transport classifier exists at this baseline; operation semantics must remain explicit.

## Work admission and limits

**Owners:** `Resources` (`crates/nrese-sparql/src/query.rs:14`), `RowLimit` (`crates/nrese-exec/src/join.rs:60`), `Workers`
(`crates/nrese-exec/src/workers.rs:9`), checked narrowing (`crates/nrese-exec/src/narrow.rs:9`), and `RequestLimits`
(`crates/nrese-server/src/policy.rs:83`). Pass one request envelope and cancellation through evaluations; apply row bounds across parallel outputs and
refuse at exact representation boundaries. `Workers::limited` narrows a handle, not nested kernels. QL `Work` (`crates/nrese-owl/src/ql/witness.rs:33`)
and rewrite `Budget` (`crates/nrese-owl/src/ql/rewrite.rs:122`) own domain-specific deterministic work.

Known bypasses:

- `crates/nrese-sparql/src/native/wcoj.rs:598`: Local atomic emitted-row accounting in chunks duplicates shared join limit mechanics.
- `crates/nrese-sparql/src/native/mod.rs:1538`: Local remaining-byte→rows policy and synthetic memory refusal (:1564) reproduce allocation-admission reasoning at a caller.

No general admission `Work` type exists; QL, solver and kernel units remain distinct.

## Datatypes

**Owner:** `nrese-xsd`, especially `owl::{Datatype, Value, ValueSet, Rational}` (`crates/rdf/nrese-xsd/src/owl/mod.rs:20`); `Value::parse` is at
`crates/rdf/nrese-xsd/src/owl/value.rs:56`. Reuse lexical parsers and datatype facts; preserve exact OWL identity and ill-typed versus unsupported
distinctions. The DL caller is `crates/nrese-dl/src/datatypes/ranges.rs:217`. Keep SPARQL promotion and canonical-only inline packing/RDF term identity
separate. See [RDF bundle](../design/rdf-bundle.md) and [DL design](../design/owl2-dl.md).

Known bypasses:

- `crates/nrese-shacl/src/datatype.rs:14`: Local integer lexical grammar.
- `crates/nrese-shacl/src/datatype.rs:20`: Local decimal grammar.
- `crates/nrese-shacl/src/datatype.rs:28`: Integer-derived range logic and i128-overflow fallback, dispatched at :57.
- `crates/nrese-sparql/src/native/value.rs:49`: Private integer-derived datatype/range table duplicates `Datatype::integer_bounds`.
- `crates/nrese-engine/src/term/inline.rs:95`: Integer parsing plus canonical grammar at :110.
- `crates/nrese-engine/src/term/inline.rs:125`: Another derived integer datatype/range table, used at :148.
- `crates/nrese-engine/src/term/inline.rs:186`: Local decimal lexical/canonical scan for bit packing.
- `crates/nrese-engine/src/term/inline.rs:320`: Private calendar/date parsing and `days_in_month` at :337, reused by date/datetime encoders (:386, :402).
- `crates/nrese-reasoner/src/engine/datatypes.rs:29`: RL has a private Data model and equality at :41, reproducing OWL value identity. nrese_xsd::owl::Value can cover the shared semantic facts; keep RL violation production local.
- `crates/nrese-reasoner/src/engine/datatypes.rs:70`: Fourth private integer-derived range table.
- `crates/nrese-reasoner/src/engine/datatypes.rs:90`: Private literal conversion, trimmed lexical forms and datatype dispatch; unsupported/ill-typed values collapse to None.
- `crates/nrese-reasoner/src/engine/datatypes.rs:128`: Private value-space membership; string-derived types all share the same Data::String test (:146–148).

`Value` alone cannot validate arbitrarily long valid-but-unrepresentable lexical values.

## Reasoner verdicts

**Owners:** nrese-dl `Verdict` (`crates/nrese-dl/src/verdict.rs:8`), re-exported as store `dl::Verdict`; store `Checked`
(`crates/nrese-store/src/dl/consistency.rs:18`); query `Completeness/Regime` (`crates/nrese-sparql/src/completeness.rs:12`, :50), tableau `Answer`
(`crates/nrese-dl/src/tableau/mod.rs:229`) and beyond `Verdict` (`crates/nrese-dl/src/beyond/mod.rs:65`, converted by `consistency()`). The verdict has
no default: classification states `Consistent`, `Inconsistent` or `Unknown(why)`, and its reports (`crates/nrese-store/src/classification.rs`) list
nothing for an inconsistent ontology and always carry a reason for an undecided one (`reasons`, :72). Consistent requires established consistency;
unknown includes unsupported or exhausted runs and parts left out of the reading. Completeness means all certain answers under the regime; soundness
means each returned answer is certain. `ReasonerRunStatus::Completed` (`crates/nrese-reasoner/src/capability.rs:33`) is lifecycle telemetry. Preserve
reasons and revision/regime identity; map tableau Unsupported/GaveUp to Unknown (:138 in consistency.rs), lower guarantees for unknown consistency,
and refuse exact answers without completeness. See [reasoning semantics](../spec/reasoning-semantics.md).

## Dataset and bench inputs

**Owners:** Python case registry `suitekit.cases::{load, load_at, problems}` (`benches/suite/suitekit/cases.py:63`, :79, :97), frozen dataset `Store`
(`benches/suite/suitekit/datasets.py:58`), result `Result` (`benches/suite/suitekit/schema.py:38`), and prepared-run inputs
(`benches/fast/fast.py:1486`). Load common case identities, use `load_at` for historical jobs, retain frozen bytes/hash/provenance/source version,
prepare once with sidecars, and verify each pass. Size checks are not full hash verification. See [case schema](../../benches/cases/README.md) and
[benchmark protocol](../../benches/PROTOCOL.md).

Known bypasses:

- `benches/reasoning/datalog/datalog.py:74`: Separate cases.toml reader/schema/source join outside common case registry.
- `benches/reasoning/dl/ore.py:175`: ORE lab index is a separate selected-case manifest.
- `benches/fast/release_gate.py:1642`: Independent frozen-dataset TOML reader beside `suitekit.datasets.load`.
- `benches/suite/suitekit/sparql_plans.py:84`: Generated-query pin parsing/directory hash comparison.
- `benches/fast/release_gate.py:1743`: Second query-pin parser/selection pipeline for preparation.
- `benches/fast/prepare-support-stress.py:37`: Script privately parses dataset catalogue and verifies its files.
- `benches/fast/release_gate.py:1635`: Private case-input expansion (`volume:`, `concat:`, extras) beside fast runner's input specification traversal at :1406 in fast.py.
