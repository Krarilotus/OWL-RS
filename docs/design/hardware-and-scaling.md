# Hardware awareness, scaling and integration interfaces

The owner's requirements (1 October 2026): the store uses whatever the hardware offers,
detected by itself and overridable by configuration for a use case; it scales to any
number of cores and any amount of memory, and beyond one machine or onto GPUs where a
use case gains from it (as AnzoGraph and QLever do for theirs); every layer is optimised
for the machine code it runs as; and other software integrates through proper interfaces
that cost no performance.

## Where NRESE stands

Verified against `0f46563` (9 October 2026). The plan below retains the owner's direction;
it is not a list of implemented backends.

- **Cores.** Bulk load, index building, joins, filters, aggregates and closures run in
  parallel where their operators support it (Rayon). There are no independently
  configured pools for queries, rule reasoning, loading and compaction. DL classification
  has a worker setting of its own.
- **Memory.** Per-query, shared-query and process limits exist. On-disk checkpoints map
  compressed indexes and dictionary entries; the OS can page them. Empty/replacement
  bulk loads can spill quad chunks, but the dictionary and other working state still
  consume memory. General query-operator spilling is unfinished.
- **CPU.** Code is generated for a CPU level, not generic `x86-64`
  (`scripts/lib/target-cpu.sh`): local builds, the perf lab and benchmark images for the
  building machine (`target-cpu=native`, every instruction set it has), the Docker image
  for `x86-64-v3` (AVX2) or `neoverse-n1`, `portable` on request; the server refuses to
  start on a CPU without what it was built for. That is the compiler's vectorisation: no
  kernel has explicit SIMD yet, and `cpu.rs` only detects features. The kernels are
  written for the cache instead: the radix sort keeps all but its first pass in cache
  (`nrese_exec::sort`), probes seek from where the last one landed (`ProbeCursor`). No
  awareness of NUMA nodes or cache sizes.
- **GPU.** No execution backend or placement gate.
- **Several machines.** WAL read replicas and SPARQL `SERVICE` federation exist. They do
  not partition a store or distribute a query's operators or reasoning rounds.
- **Interfaces.** HTTP (SPARQL 1.1 Protocol, Graph Store Protocol, the console's API)
  and the Rust crates themselves.

## Configuration and strategy ownership

[ARCHITECTURE.md §3](../ARCHITECTURE.md#3-where-to-fix-what) remains the ownership policy:
behaviour is typed in its owning crate; environment/file/CLI parsing is in the server's
[`config/`](../../crates/nrese-server/src/config). Its
[`settings.rs`](../../crates/nrese-server/src/config/settings.rs) lists operator-facing
keys; [config-reference.md](../ops/config-reference.md) documents their use. An internal
Rust option or a hard-coded heuristic is not automatically a server setting.

| Concern and owner | Controls already present | Automatic choice and remaining gap |
|---|---|---|
| Resource limits: server policy, store allocation, exec accounting | `budgets.query_memory`, `total_query_memory`, `process_memory`, `bulk_load_memory`, `result_cache`, `huge_pages`; request timeouts | Server parsing resolves memory shares against available machine/process memory: defaults for total queries, process and bulk quads are 50%, 75% and 25%. Result-cache sizing has a bounded automatic default. These are separate budgets, not a global reservation scheduler or calibrated hardware profile |
| Storage: engine, wired by `StoreConfig` | `store.map_checkpoints`, `verify_on_open`, `checkpoint_after_wal`, `index_encoding`, `vocabulary`, `wal_archive` | Checkpoints and size-tiered compaction respond to WAL/run sizes. `EngineConfig`/`CompactionPolicy` expose maintenance, fanout and inline-merge controls to Rust callers; the server catalog does not expose all of them. Mapped reads and bulk spill exist; query spill and NUMA-aware placement do not |
| Query strategies: SPARQL | `QueryOptions` carries budgets, cancellation and `as_written`; `stream_rows` and `cross_chunk_rows` are Rust options used by tests, not server configuration keys | Join order, index/merge/hash/WCOJ paths and aggregation routes follow data estimates and shape. DP limit 12, probe factor 32 and probe cost 4 are code constants. A complete physical plan and hardware-calibrated strategy controls remain unfinished |
| Shared kernels: exec; index building: engine | Callers supply data, sortedness and applicable limits | Joins choose merge/hash from sortedness; sort chooses packed radix/comparison from key width and input size. Exec's sort cutoff (4,096 keys), sort chunks (65,536 keys), parallel join threshold (16,384 rows) and the engine's parallel index-build threshold are code constants, not measured cache/core settings |
| Rule reasoning and its store integration | `ReasonerConfig`, `StoreConfig`; `reasoner.mode`, `rules`, `equality`, `equality_answers`, `equality_expansion`, `ql_rewriting`, `support_sets` | QL `auto` follows reasoning mode; rule evaluation and equality handling retain their own algorithms. Rule-evaluation morsels are fixed at 4,096 in `eval.rs`; no per-workload pool or hardware-derived morsel control |
| DL services: store task policy, DL search | `DlConfig`: `dl.answers`, `consistency`, `timeout`, `memory`, `max_candidates`, `threads`, `max_nodes`, `max_branch_points` | `threads = 0` resolves to available parallelism for classification. Bounds and search portfolios choose semantic routes within their owners; this is not a CPU/GPU placement policy |
| Vector search: engine dictionary integration, vector kernels | Rust `VectorQuery` carries `strategy` (`Auto`, `Exact`, `Approximate`), metric, `k` and `ef` | `Auto` uses space size and accepted-filter share to choose exact scan or HNSW. Cutoffs (20,000 vectors, 2% accepted) are code constants. This CPU strategy does not provide a GPU backend |
| CPU: build tooling and engine feature check | `NRESE_TARGET_CPU` at build time, including `native` and `portable` | Compiler target selection and startup compatibility checks exist; runtime kernel multiversion dispatch, installation calibration and NUMA topology/pinning remain planned |
| Replication and federation: server transport, store operations, engine WAL / SPARQL `SERVICE` | `ReplicationConfig` with `replication.mode`, `primary`, `token`, `poll`, `batch_bytes`; `FederationConfig` with `federation.allow`, `timeout`, `max_rows` | Replica bootstrap and WAL following are implemented. Automatic failover/re-bootstrap, sharding, distributed operator exchange and distributed reasoning are not |

Source contracts: [`StoreConfig`](../../crates/nrese-store/src/config.rs),
[`DlConfig`](../../crates/nrese-store/src/dl/config.rs),
[`QueryOptions`](../../crates/nrese-sparql/src/query.rs),
[`DurabilityConfig`](../../crates/nrese-engine/src/durability/mod.rs),
[`CompactionPolicy`](../../crates/nrese-engine/src/index/compaction.rs),
[`server memory defaults`](../../crates/nrese-server/src/config/store_env.rs),
[`BGP planner`](../../crates/nrese-sparql/src/native/plan.rs),
[`native executor`](../../crates/nrese-sparql/src/native/mod.rs),
[`sort`](../../crates/nrese-exec/src/sort.rs),
[`joins`](../../crates/nrese-exec/src/join.rs),
[`rule evaluation`](../../crates/nrese-reasoner/src/eval.rs), and
[`vector strategy`](../../crates/nrese-engine/src/term/vectors.rs). See also
[read-replica operations and limits](../ops/replication.md).

Hardware/distributed extensions must preserve these owners: settings describe policy,
the owning planner or algorithm makes the choice, and kernels implement the work.
Existing id columns, engine access APIs and workload-specific executors are reuse points;
they do not yet provide device buffers, network exchange or distributed commit semantics.
H1/H3/H5/H6 below identify that unfinished work without introducing a universal executor.

## Plan

**Owner's decision (7 October 2026):** the hardware is measured once, when the store is
installed, and kept as its profile (re-measured on request); every layer may assume it is
known. The targets are Intel and AMD desktops and servers, ARM servers and cluster nodes. The
GPU is used for parallel workloads wherever it measures better (H5), as part of using all
the hardware a machine has, with the CPU path always available.

**H1. Hardware profile (planned).** At installation the store measures cores (physical
and logical), NUMA nodes, RAM, cache sizes, CPU features, GPUs and their memory, and the
disks' kind, and derives its settings from them: thread pools per workload (queries,
bulk load, reasoning, compaction), memory budgets,
run sizes, whether to memory-map. Every derived value can be overridden in the
configuration, and `nrese-server check-config` will show what was detected and chosen
(today it reports effective settings, not an installation profile). One
place owns this (`nrese-engine`'s hardware module), so the layers don't each guess. Small
machines are a profile of their own, not an afterthought (the owner: clever design should
stay fast whatever it runs on): compressed runs, memory mapping instead of loading,
budgets that spill rather than fail, and plans chosen for few cores.

**H2. Machine code per CPU (runtime dispatch planned).** Hot kernels (dictionary hashing,
run merging and search, intersections of sorted id lists, filters over id columns,
decoding packed runs) get
explicit SIMD versions with runtime dispatch on the detected features (AVX2, AVX-512,
NEON) and a portable fallback, so one binary is fast everywhere. Release builds for a
known target (the Docker image, a cluster) can also be compiled for that CPU. Each kernel
is measured in the perf lab against its scalar version, and kept only where it wins.

**H3. NUMA and many cores (planned).** Thread pools pinned per NUMA node, data partitioned
so a thread mostly reads memory near it, morsel-driven parallelism inside a query (a large scan
or join split into pieces any free thread takes), so that 64–256 cores are used by one
query, not only by many.

**H4. Larger than RAM (partly built).** Mapped compressed checkpoints and bulk-load quad
spilling are implemented. Remaining work includes spilling sorts, `DISTINCT` and grouping
when a query's budget is reached, and bounding the other resident working state. Mapping
the stored data alone does not bound a load's, query's or materialisation's memory.

**H5. GPU, where a use case gains (planned).** Behind a feature and with the CPU path always
available: candidates are vector similarity search (V1), very large hash joins and
aggregations, and semi-naive rule materialisation over huge closures. Each one only after
the perf lab shows the CPU version is the bottleneck on that workload; the GPU's transfer
cost decides as much as its speed. The research so far (wiki: *R3-S6 GPU*): a recursive
stratum stays resident on the GPU (joins and merging both, GDlog's profile is about 40 %
each); dispatch by bytes moved and reuse, not by rows; dense recursive closures (transitive
closure, same generation) gain most (FVLog, SRDatalog), LUBM-style closures may not pay. Open
decision: the GPU toolchain must fit ADR-0010 (Rust, no C/C++ build dependency, vendor
neutral where possible): candidates are wgpu compute shaders and CubeCL (Rust kernels for
CUDA, ROCm and wgpu), measured on the main PC's RTX 4090.

*The GPU gate* (owner, 7 October 2026), built first, before any GPU kernel pays off: one
decider, owned by the hardware module, that places each piece of work on the CPU, the GPU,
or both.
- **Inputs:** the operator's estimated bytes moved and reused, its rows and work per byte
  (from the planner's and the rule engine's estimates), the free GPU memory, and the
  measured speeds of both sides from the installation profile (calibrated per kernel on the
  machine, not assumed).
- **Hybrid placement:** work that only partly pays is split. Examples: the GPU takes the
  dense partitions of a join, a closure's large strongly connected components or a batch of
  vector distances, while the CPU takes the sparse rest and everything that doesn't fit the
  card's memory. The results meet in the shared id columns.
- **Every placement is visible** in EXPLAIN, can be forced or forbidden per request and per
  workload, and is checked against the CPU result in the differential tests.
- **v3's GraphRAG components** (embeddings, vector search, graph algorithms) use the same
  gate rather than a GPU path of their own.

**H6. Several machines (partly built).**
- *Read replicas (built):* the whole store on several machines; one machine takes the
  writes and the others follow its WAL, including inferred changes, and answer queries.
  Each machine still holds the whole dataset. Failover and recovery from a missing log
  range require operator action; see [replication.md](../ops/replication.md).
- *Sharding (planned):* the data split across machines (by subject hash, as AnzoGraph does), each
  query and each reasoning round run on all of them in parallel and their partial results
  exchanged. For data or computations beyond one machine; costs network exchange in
  joins. A large project of its own, after H1–H4.

**H7. Integration interfaces (planned beyond HTTP and the existing Rust crates).** Other
software uses NRESE either over the network (HTTP, any language) or in its own process
(no network, no serialisation):
- *Rust*: a stable embedding API (`nrese` facade crate: open a store, load, query, update,
  validate, reason) over the existing crates.
- *Python* (PyO3) and *C* (a C ABI, through which Java, .NET, Go and C++ can call): thin
  layers over the Rust API. Results cross the boundary in batches as Apache Arrow
  columns (term ids and decoded values), not one object per value, so a binding adds no
  per-row cost; the engine itself is the same machine code.
- *WebAssembly*: the parsers, writers and an in-memory engine for browsers and edge
  runtimes.
Which of these come first depends on who integrates (DMW, ResearchSpace, others).

Remaining sequencing belongs to [ROADMAP.md](../ROADMAP.md). The RDF migration's step 5
is complete; it did not deliver H1's installation profile or H7's stable facade/bindings.
Hardware and distributed backends still need workload-specific evidence and their own
implementation decisions.
