# Probes

Small measurements outside the benchmark suite, each one script with a usage line at its
top: per-request HTTP latency, large results over HTTP, autocompletion, a count after a
restart, BSBM over HTTP and in the perf lab, resident memory over a load, load memory
against the bulk-load budget, the baseline branch against today's, plain against
FSST-compressed vocabularies.

They assume the office PC's layout by default (`~/nrese-bench` with the repository in it
and datasets under `scratch/`, prepared by `scripts/office/`); `NRESE_BENCH` and
`NRESE_SCRATCH` override it ([common.sh](common.sh)). Most need Linux (`/proc`).
Clean up after a run (`scripts/bench-cleanup.sh`); the probes remove their own stores.

## For finding regressions (3 October 2026)

Cross-platform (Windows or Linux, Python with psutil), each with its usage at its top:

| Probe | Measures |
|---|---|
| `ab-load.py` | two or more server builds loading the same file, interleaved (A B A B ...): the load log's phases (parse and encode, index build, checkpoint), median and range; the way to tell a code regression from drift on the machine |
| `cpu-use.py` | how many cores a command keeps busy over time: the serial stretches of a load |
| `serve-memory.py` | a server's resident memory every 100 ms over the suite's query mix, by phase |
| `query-memory.py` | the peak memory each query adds on a fresh server, several builds side by side |
| `perf-profile.sh` | a CPU profile (Linux perf, in Docker, works without an elevated Windows session) of any server command, built with frame pointers: flat, inclusive and by caller |
| `soak.py` | a server under a mixed load for a while (queries, updates, Graph Store, sessions, EXPLAIN, listings over kept-alive connections): errors, resident memory's slope, handles and threads, and whether the soak graph holds exactly what the workers committed |
| `image-cap-check.sh` | a server image under a container memory cap without swap: a release's bulk load, its queries over HTTP (sequential and 8 streams), a restart, the cgroup peak of each ([the Zebratlas record](../baselines/zebratlas/2026-10-04-image-4g.md)) |
