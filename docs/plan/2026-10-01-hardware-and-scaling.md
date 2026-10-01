# Hardware awareness, scaling and integration interfaces

The owner's requirements (1 October 2026): the store uses whatever the hardware offers,
detected by itself and overridable by configuration for a use case; it scales to any
number of cores and any amount of memory, and beyond one machine or onto GPUs where a
use case gains from it (as AnzoGraph and QLever do for theirs); every layer is optimised
for the machine code it runs as; and other software integrates through proper interfaces
that cost no performance.

## Where NRESE stands

- **Cores.** Bulk load, index building, joins, filters, aggregates and closures run in
  parallel (rayon), on all cores by default; the thread count is not configurable per
  workload yet.
- **Memory.** Per-query and server-wide memory budgets; indexes in RAM as bit-packed
  runs. Data larger than RAM isn't possible yet (memory-mapped runs, Pf2).
- **CPU.** Generic `x86-64` builds: no use of AVX2, AVX-512 or NEON where present, no
  awareness of NUMA nodes or cache sizes.
- **GPU, several machines.** None.
- **Interfaces.** HTTP (SPARQL 1.1 Protocol, Graph Store Protocol, the console's API)
  and the Rust crates themselves.

## Plan

**H1. Hardware profile.** At start the store detects cores (physical and logical), NUMA
nodes, RAM, cache sizes, CPU features and the disks' kind, and derives its settings from
them: thread pools per workload (queries, bulk load, reasoning, compaction), memory budgets,
run sizes, whether to memory-map. Every derived value can be overridden in the
configuration, and `nrese-server --check-config` shows what was detected and chosen. One
place owns this (`nrese-engine`'s hardware module), so the layers don't each guess.

**H2. Machine code per CPU.** Hot kernels (dictionary hashing, run merging and search,
intersections of sorted id lists, filters over id columns, decoding packed runs) get
explicit SIMD versions with runtime dispatch on the detected features (AVX2, AVX-512,
NEON) and a portable fallback, so one binary is fast everywhere. Release builds for a
known target (the Docker image, a cluster) can also be compiled for that CPU. Each kernel
is measured in the perf lab against its scalar version, and kept only where it wins.

**H3. NUMA and many cores.** Thread pools pinned per NUMA node, data partitioned so a
thread mostly reads memory near it, morsel-driven parallelism inside a query (a large scan
or join split into pieces any free thread takes), so that 64–256 cores are used by one
query, not only by many.

**H4. Larger than RAM** (Pf2): memory-mapped compressed runs, so a dataset can exceed RAM
and restart takes seconds, as in QLever; spilling of sorts, `DISTINCT` and grouping to disk
when a query's budget is reached.

**H5. GPU, where a use case gains.** Behind a feature and with the CPU path always
available: candidates are vector similarity search (V1), very large hash joins and
aggregations, and semi-naive rule materialisation over huge closures. Each one only after
the perf lab shows the CPU version is the bottleneck on that workload; the GPU's transfer
cost decides as much as its speed.

**H6. Several machines (scale-out).**
- *Read replicas:* the whole store on several machines; one machine takes the writes and
  ships its write-ahead log to the others, which answer queries. More query throughput and
  availability; the data must still fit one machine. Fits the current design directly.
- *Sharding:* the data split across machines (by subject hash, as AnzoGraph does), each
  query and each reasoning round run on all of them in parallel and their partial results
  exchanged. For data or computations beyond one machine; costs network exchange in
  joins. A large project of its own, after H1–H4.

**H7. Integration interfaces.** Other software uses NRESE either over the network (HTTP,
any language) or in its own process (no network, no serialisation):
- *Rust*: a stable embedding API (`nrese` facade crate: open a store, load, query, update,
  validate, reason) over the existing crates.
- *Python* (PyO3) and *C* (a C ABI, through which Java, .NET, Go and C++ can call): thin
  layers over the Rust API. Results cross the boundary in batches as Apache Arrow
  columns (term ids and decoded values), not one object per value, so a binding adds no
  per-row cost; the engine itself is the same machine code.
- *WebAssembly*: the parsers, writers and an in-memory engine for browsers and edge
  runtimes.
Which of these come first depends on who integrates (DMW, ResearchSpace, others).

Order: H1 and H7's Rust API with the migration's step 5; H2–H4 in the performance
milestone; H5 and H6 when a workload needs them, measured.
