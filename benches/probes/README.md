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
