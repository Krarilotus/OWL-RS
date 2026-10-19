# Noisy series: the release gate's work list

The release gate tolerates no loss (benches/PROTOCOL.md §5, "The release gate"); a series whose
run-to-run noise is large can't be decided at a practical number of passes, so it ends as
*suspect* and blocks. These are to be fixed, not tolerated. The list: every series whose noise
(the standard deviation of the log of its four per-pass medians) exceeds 5 % in the A-B-B-A
passes of 14 Oct 2026 on phuoc-yu: 137 of the 621 series of at least 5 µs, each
binary measured on its own (v1.5 0f465633 and v2 32ecf1e5 on the fast suite; main f9b515e8 and v2
32ecf1e5 on the perf-lab datasets). Re-measure on the head before fixing: some came with defects
fixed since (q02, the text filter). Grouped by likely cause, the cause inferred from the shape
(the pass medians, the series' size and the case's tool), not proven.

| Group | Series |
|---|---|
| Bimodal between processes: a plan or schedule that depends on arrival | 3 |
| One pass apart: a process that started differently | 26 |
| Microsecond-scale series: the timer's steps | 45 |
| DL pipeline phases timed once per run | 13 |
| Dataset queries with five runs a process | 44 |
| Concurrency: readers and clients | 0 |
| Others: profile each | 6 |

## Bimodal between processes: a plan or schedule that depends on arrival (3)

The pass medians fall into two clusters (one process fast, the next slow), the sign of a plan or a schedule chosen by timing rather than by the data: dbpedia q02-born-in-berlin was one (the characteristic sets built in the background, 6b7fcaef; fixed on refactor/engine-v2 at ae1ad24b), and these are the same shape. Fix: find what the plan depends on (EXPLAIN per process, `benches/probes/query-swing.py`) and make it depend on the data alone. More passes don't help.

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| dl-roles | blocking | v2 32ecf1e5 | 0.1162 | 0.649 | 0.056, 0.058, 0.1745, 0.176 |
| lab-dbpedia-core | q03-top-birthplaces | main f9b515e8 (perf-lab datasets) | 3.224 | 0.161 | 2.648, 3.65, 2.859, 3.589 |
| writes-under-readers | --readers 32 | v1.5 0f465633 | 0.1638 | 0.213 | 0.1937, 0.1925, 0.1352, 0.1318 |

**dl-roles blocking: a probable engine effect, by run order within a process** (19 Oct 2026; for a
DL owner, not the harness). The tableau tool runs the case's file four times in one process
(`repeat = 4`). In every pass, of v1.5 0f465633, v2 32ecf1e5 and c9be1609 alike, the first two runs'
blocking phase takes 0.053–0.062 ms, the third 0.32–0.36 ms (six times as long), the fourth
either 0.29–0.31 ms or 0.053–0.060 ms again; the other phases don't move with it (saturate
7.4–8.7 ms in every run), and the work counters are the same in every process. The "bimodal"
pass medians (0.056 against 0.175 ms) are only the median of four flipping with the fourth run.
Per pass, the runs' blocking times in ms (phuoc-yu):

| Pass | Binary | Runs 1-4 |
|---|---|---|
| 14 Oct, 1 | v1.5 0f465633 | 0.053, 0.053, 0.346, 0.290 |
| 14 Oct, 6 | v1.5 0f465633 | 0.053, 0.053, 0.331, 0.293 |
| 14 Oct, 7 | v1.5 0f465633 | 0.054, 0.053, 0.324, 0.301 |
| 14 Oct, 12 | v1.5 0f465633 | 0.053, 0.053, 0.350, 0.053 |
| 14 Oct, 3 | v2 32ecf1e5 | 0.056, 0.056, 0.333, 0.056 |
| 14 Oct, 4 | v2 32ecf1e5 | 0.056, 0.055, 0.319, 0.060 |
| 14 Oct, 9 | v2 32ecf1e5 | 0.056, 0.055, 0.347, 0.293 |
| 14 Oct, 10 | v2 32ecf1e5 | 0.056, 0.056, 0.347, 0.296 |
| 18-19 Oct A/A, 1-5 | c9be1609 | 0.059, 0.057, 0.328, 0.291 / 0.059, 0.058, 0.351, 0.300 / 0.058, 0.058, 0.329, 0.305 / 0.059, 0.062, 0.357, 0.303 / 0.059, 0.056, 0.328, 0.056 |

So something the blocking code keeps across classifications in one process (a table's
retained capacity or layout, a cache, the allocator's state) makes the third run's blocking
slower, deterministically, and sometimes the fourth's. Not timer noise: the effect is six
times the phase, at the same place in every process. More passes won't settle it; more runs a
process (the calibration's `repeat`) make the series stable, but it then measures the slow
state. Next: profile runs 2 and 3 of one process side by side (`tableau_consistency` with the
file four times), and check whether the effect follows the run count or the input.

## One pass apart: a process that started differently (26)

Three passes agree and one is far off (slower or faster): the process met a different state (start-up, the page cache, another process on the host, frequency) rather than a different plan. Fix: find what that pass had different (its log, the host's load and governor at the time; `scripts/quiet-slot.sh`'s watcher); if it is the case's own first process, a warm-up process before the timed ones.

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| dl-datatypes | expand | v2 32ecf1e5 | 1.241 | 0.162 | 1.249, 1.233, 1.254, 0.9015 |
| dl-number | parse | v1.5 0f465633 | 0.228 | 0.470 | 0.095, 0.264, 0.214, 0.242 |
| dl-number | parse | v2 32ecf1e5 | 0.2127 | 0.319 | 0.117, 0.2105, 0.2345, 0.215 |
| dl-roles | blocking | v1.5 0f465633 | 0.1722 | 0.595 | 0.1715, 0.173, 0.1775, 0.053 |
| dl-roles | compile | v1.5 0f465633 | 6.626 | 0.167 | 6.532, 6.565, 6.688, 9.2 |
| geo-within | within-0 | v2 32ecf1e5 | 19.93 | 0.136 | 19.89, 19.97, 26.05, 19.65 |
| geo-within | within-1 | v2 32ecf1e5 | 5.613 | 0.123 | 5.63, 5.597, 7.163, 5.578 |
| horn-classify | normalise | v1.5 0f465633 | 0.7 | 0.126 | 0.7, 0.7, 0.7, 0.9 |
| horn-classify | read | v1.5 0f465633 | 2 | 0.192 | 1.4, 1.9, 2.1, 2.1 |
| index-join-keys | keys-25k | v2 32ecf1e5 | 2.057 | 0.115 | 2.034, 2.054, 2.581, 2.059 |
| lab-dbpedia-core | q02-born-in-berlin | v1.5 0f465633 | 7 | 0.417 | 7.328, 7.561, 3.139, 6.672 |
| lab-dbpedia-core | q02-born-in-berlin | v2 32ecf1e5 (perf-lab datasets) | 6.909 | 0.370 | 7.333, 7.426, 3.408, 6.484 |
| lab-dbpedia-core | q03-top-birthplaces | v1.5 0f465633 | 4.682 | 0.216 | 4.878, 4.656, 4.709, 3.09 |
| lab-dbpedia-core | q03-top-birthplaces | v2 32ecf1e5 (perf-lab datasets) | 4.885 | 0.229 | 5.146, 4.861, 4.908, 3.153 |
| lab-entities-10m | q05-group-born | v2 32ecf1e5 (perf-lab datasets) | 0.067 | 0.176 | 0.063, 0.061, 0.071, 0.09 |
| lab-entities-10m | q06-label-prefix | v2 32ecf1e5 (perf-lab datasets) | 4.489 | 0.406 | 3.833, 3.603, 8.772, 5.144 |
| lab-olympics | q08-event-hierarchy | main f9b515e8 (perf-lab datasets) | 0.2795 | 0.161 | 0.381, 0.275, 0.284, 0.272 |
| lab-olympics | q08-event-hierarchy | v2 32ecf1e5 (perf-lab datasets) | 0.3205 | 0.233 | 0.311, 0.329, 0.504, 0.312 |
| lab-olympics | q10-ask | v2 32ecf1e5 | 0.1755 | 0.159 | 0.158, 0.178, 0.173, 0.229 |
| lab-yago-tiny | q04-movie-actors | v2 32ecf1e5 | 0.187 | 0.211 | 0.181, 0.282, 0.186, 0.188 |
| lab-yago-tiny | q04-movie-actors | v2 32ecf1e5 (perf-lab datasets) | 0.2675 | 0.235 | 0.264, 0.303, 0.177, 0.271 |
| plan-estimates | chain-posts | v2 32ecf1e5 | 0.0525 | 0.213 | 0.054, 0.035, 0.051, 0.055 |
| rdf12-annotations | from-source-7 | v2 32ecf1e5 | 0.608 | 0.159 | 0.621, 0.8135, 0.595, 0.573 |
| rdf12-join-2g | asserted-and-sourced | v2 32ecf1e5 | 26.51 | 0.113 | 25.37, 32.58, 26.2, 26.81 |
| writes-under-readers | --readers 8 | v1.5 0f465633 | 0.08555 | 0.271 | 0.0849, 0.093, 0.0515, 0.0862 |
| writes-under-readers | --readers 8 | v2 32ecf1e5 | 0.05715 | 0.265 | 0.0527, 0.0616, 0.0923, 0.0527 |

## Microsecond-scale series: the timer's steps (45)

Under 50 µs a query is a few steps of the perf lab's 1 µs resolution; a pass's median of five runs moves by whole steps. Fix in the harness: time a batch of executions per sample (or report in nanoseconds) so that a sample is well above the resolution; the gate's 5 µs floor keeps them from blocking on quantisation but can't make them decide.

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| clients-sweep | 8 clients p99 | v1.5 0f465633 | 0.0075 | 0.077 | 0.007, 0.008, 0.008, 0.007 |
| clients-sweep | 8 clients p99 | v2 32ecf1e5 | 0.008 | 0.067 | 0.007, 0.008, 0.008, 0.008 |
| dl-datatypes | parse | v2 32ecf1e5 | 0.017 | 0.120 | 0.014, 0.018, 0.018, 0.016 |
| dl-number | compile | v1.5 0f465633 | 0.02375 | 0.175 | 0.022, 0.023, 0.0325, 0.0245 |
| dl-number | compile | v2 32ecf1e5 | 0.02625 | 0.170 | 0.0235, 0.029, 0.0205, 0.029 |
| dl-number | normalise | v1.5 0f465633 | 0.01825 | 0.100 | 0.017, 0.0185, 0.0215, 0.018 |
| dl-number | normalise | v2 32ecf1e5 | 0.02025 | 0.130 | 0.0225, 0.02, 0.0165, 0.0205 |
| dlmode-lubm1 | q01 | v1.5 0f465633 | 0.008 | 0.059 | 0.009, 0.008, 0.008, 0.008 |
| dlmode-lubm1 | q01 | v2 32ecf1e5 | 0.009 | 0.129 | 0.01, 0.008, 0.01, 0.008 |
| dlmode-lubm1 | q02 | v1.5 0f465633 | 0.04975 | 0.069 | 0.053, 0.049, 0.045, 0.0505 |
| dlmode-lubm1 | q10 | v1.5 0f465633 | 0.008 | 0.103 | 0.007, 0.008, 0.008, 0.009 |
| dlmode-lubm1 | q10 | v2 32ecf1e5 | 0.008 | 0.126 | 0.008, 0.01, 0.008, 0.0075 |
| dlmode-lubm1 | q12 | v1.5 0f465633 | 0.0285 | 0.129 | 0.028, 0.036, 0.029, 0.027 |
| dlmode-lubm1 | q13 | v1.5 0f465633 | 0.006 | 0.077 | 0.006, 0.006, 0.007, 0.006 |
| lab-dbpedia-core | q01-point-lookup | main f9b515e8 (perf-lab datasets) | 0.0085 | 0.068 | 0.009, 0.009, 0.008, 0.008 |
| lab-dbpedia-core | q01-point-lookup | v1.5 0f465633 | 0.009 | 0.053 | 0.009, 0.009, 0.009, 0.01 |
| lab-dbpedia-core | q01-point-lookup | v2 32ecf1e5 | 0.009 | 0.221 | 0.009, 0.009, 0.014, 0.009 |
| lab-dbpedia-core | q01-point-lookup | v2 32ecf1e5 (perf-lab datasets) | 0.009 | 0.100 | 0.009, 0.009, 0.009, 0.011 |
| lab-dbpedia-core | q07-subdivision-path | main f9b515e8 (perf-lab datasets) | 0.009 | 0.059 | 0.009, 0.009, 0.009, 0.008 |
| lab-dbpedia-core | q07-subdivision-path | v1.5 0f465633 | 0.011 | 0.109 | 0.01, 0.011, 0.013, 0.011 |
| lab-dbpedia-core | q07-subdivision-path | v2 32ecf1e5 | 0.011 | 0.155 | 0.011, 0.011, 0.011, 0.015 |
| lab-dbpedia-core | q11-count-all | v2 32ecf1e5 (perf-lab datasets) | 0.0055 | 0.105 | 0.006, 0.005, 0.005, 0.006 |
| lab-entities-10m | q01-point-lookup | v1.5 0f465633 | 0.006 | 0.194 | 0.005, 0.006, 0.008, 0.006 |
| lab-entities-10m | q01-point-lookup | v2 32ecf1e5 | 0.005 | 0.091 | 0.006, 0.005, 0.005, 0.005 |
| lab-entities-10m | q01-point-lookup | v2 32ecf1e5 (perf-lab datasets) | 0.0055 | 0.222 | 0.005, 0.005, 0.006, 0.008 |
| lab-entities-10m | q02-two-hop | main f9b515e8 (perf-lab datasets) | 0.007 | 0.067 | 0.007, 0.008, 0.007, 0.007 |
| lab-entities-10m | q02-two-hop | v1.5 0f465633 | 0.022 | 0.155 | 0.02, 0.025, 0.024, 0.018 |
| lab-entities-10m | q02-two-hop | v2 32ecf1e5 | 0.027 | 0.269 | 0.02, 0.031, 0.023, 0.036 |
| lab-entities-10m | q02-two-hop | v2 32ecf1e5 (perf-lab datasets) | 0.026 | 0.063 | 0.028, 0.024, 0.026, 0.026 |
| lab-entities-10m | q03-count-type | v1.5 0f465633 | 0.007 | 0.118 | 0.006, 0.008, 0.007, 0.007 |
| lab-entities-10m | q03-count-type | v2 32ecf1e5 | 0.006 | 0.138 | 0.005, 0.006, 0.007, 0.006 |
| lab-entities-10m | q03-count-type | v2 32ecf1e5 (perf-lab datasets) | 0.007 | 0.067 | 0.008, 0.007, 0.007, 0.007 |
| lab-entities-10m | q05-group-born | main f9b515e8 (perf-lab datasets) | 0.0495 | 0.077 | 0.049, 0.05, 0.051, 0.043 |
| lab-olympics | q01-point-lookup | v1.5 0f465633 | 0.005 | 0.166 | 0.005, 0.006, 0.004, 0.005 |
| lab-olympics | q01-point-lookup | v2 32ecf1e5 | 0.006 | 0.091 | 0.005, 0.006, 0.006, 0.006 |
| lab-olympics | q01-point-lookup | v2 32ecf1e5 (perf-lab datasets) | 0.0055 | 0.105 | 0.006, 0.005, 0.006, 0.005 |
| plan-estimates | chain-posts | v1.5 0f465633 | 0.049 | 0.074 | 0.0455, 0.052, 0.052, 0.046 |
| q-paths | broader-closure | v1.5 0f465633 | 0.005 | 0.166 | 0.005, 0.006, 0.005, 0.004 |
| ql-owl2bench | q03 | v2 32ecf1e5 | 0.008 | 0.059 | 0.009, 0.008, 0.008, 0.008 |
| ql-owl2bench | q08 | v1.5 0f465633 | 0.0055 | 0.105 | 0.006, 0.006, 0.005, 0.005 |
| ql-owl2bench | q15 | v1.5 0f465633 | 0.0165 | 0.093 | 0.016, 0.0195, 0.017, 0.016 |
| ql-owl2bench | q16 | v2 32ecf1e5 | 0.007 | 0.077 | 0.007, 0.007, 0.007, 0.006 |
| ql-owl2bench | q17 | v1.5 0f465633 | 0.005 | 0.112 | 0.005, 0.004, 0.005, 0.005 |
| text-search | docs-w19999 | v1.5 0f465633 | 0.006 | 0.194 | 0.005, 0.005, 0.007, 0.007 |
| text-search | docs-w19999 | v2 32ecf1e5 | 0.005 | 0.091 | 0.006, 0.005, 0.005, 0.005 |

## DL pipeline phases timed once per run (13)

The tableau and classifier tools time each phase (parse, read, normalise, compile, blocking, ...) once per file and process; short phases (often under a millisecond) carry the process's start-up and cache state. Fix: more repeats per process for these cases (`repeat`), or judge the phases only where they are long enough, with the whole time as the case's series.

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| dl-nominals | compile | v1.5 0f465633 | 0.7993 | 0.065 | 0.8045, 0.786, 0.794, 0.903 |
| dl-nominals | normalise | v1.5 0f465633 | 0.3915 | 0.062 | 0.376, 0.4045, 0.429, 0.3785 |
| dl-nominals | normalise | v2 32ecf1e5 | 0.4365 | 0.133 | 0.5345, 0.4065, 0.4665, 0.403 |
| dl-nominals | read | v2 32ecf1e5 | 11.95 | 0.066 | 10.98, 12.69, 11.53, 12.36 |
| dl-number | read | v1.5 0f465633 | 0.1 | 0.079 | 0.094, 0.095, 0.1105, 0.105 |
| dl-number | read | v2 32ecf1e5 | 0.1075 | 0.091 | 0.104, 0.12, 0.097, 0.111 |
| dl-number | search | v1.5 0f465633 | 3.772 | 0.054 | 3.425, 3.845, 3.834, 3.711 |
| dl-roles | compile | v2 32ecf1e5 | 6.53 | 0.100 | 7.968, 6.546, 6.498, 6.514 |
| dl-roles | normalise | v1.5 0f465633 | 13.11 | 0.059 | 13.08, 13.02, 13.14, 14.73 |
| dl-roles | normalise | v2 32ecf1e5 | 13.36 | 0.055 | 14.87, 13.33, 13.38, 13.25 |
| el-classify-irregular | read | v1.5 0f465633 | 24.7 | 0.054 | 22.2, 24.8, 24.6, 24.8 |
| horn-classify | compile | v1.5 0f465633 | 1.3 | 0.072 | 1.3, 1.3, 1.3, 1.5 |
| horn-classify | write | v1.5 0f465633 | 31.3 | 0.069 | 29.8, 32.6, 30, 34.4 |

## Dataset queries with five runs a process (44)

The perf-lab datasets (olympics, yago, dbpedia, entities) and the DL-mode and QL query cases run each query five times after one warm-up in each process (`--runs 5`, the perf lab's default); between processes the page cache, the allocator's state and the plan cache differ. Fix: more runs per process for these cases (e.g. `--runs 21`), and check each such series for the bimodal shape before calling it noise.

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| dlmode-lubm1 | q02 | v2 32ecf1e5 | 0.05275 | 0.065 | 0.056, 0.0545, 0.0485, 0.051 |
| dlmode-lubm1 | q06 | v2 32ecf1e5 | 0.4065 | 0.080 | 0.462, 0.4205, 0.3925, 0.3885 |
| dlmode-lubm1 | q07 | v1.5 0f465633 | 0.05575 | 0.081 | 0.0545, 0.048, 0.057, 0.057 |
| dlmode-lubm1 | q07 | v2 32ecf1e5 | 0.0535 | 0.075 | 0.0545, 0.0525, 0.0525, 0.0615 |
| dlmode-lubm1 | q08 | v1.5 0f465633 | 1.389 | 0.064 | 1.385, 1.361, 1.567, 1.393 |
| lab-dbpedia-core | q03-top-birthplaces | v2 32ecf1e5 | 4.588 | 0.203 | 4.269, 5.256, 3.322, 4.908 |
| lab-dbpedia-core | q04-date-range | main f9b515e8 (perf-lab datasets) | 0.08 | 0.081 | 0.077, 0.078, 0.092, 0.082 |
| lab-dbpedia-core | q04-date-range | v1.5 0f465633 | 0.089 | 0.095 | 0.088, 0.106, 0.09, 0.086 |
| lab-dbpedia-core | q04-date-range | v2 32ecf1e5 (perf-lab datasets) | 0.1195 | 0.252 | 0.159, 0.091, 0.135, 0.104 |
| lab-dbpedia-core | q05-population | v2 32ecf1e5 (perf-lab datasets) | 1.107 | 0.086 | 1.305, 1.088, 1.111, 1.103 |
| lab-dbpedia-core | q09-optional-death | v2 32ecf1e5 (perf-lab datasets) | 26.03 | 0.064 | 28.2, 27.1, 24.97, 24.7 |
| lab-dbpedia-core | q10-types-count | v1.5 0f465633 | 0.112 | 0.081 | 0.125, 0.103, 0.114, 0.11 |
| lab-dbpedia-core | q10-types-count | v2 32ecf1e5 (perf-lab datasets) | 0.103 | 0.087 | 0.106, 0.1, 0.092, 0.113 |
| lab-dbpedia-core | q13-no-death | v1.5 0f465633 | 2.742 | 0.059 | 2.701, 2.705, 3.063, 2.779 |
| lab-dbpedia-core | q13-no-death | v2 32ecf1e5 | 2.798 | 0.072 | 2.722, 2.593, 3.065, 2.875 |
| lab-entities-10m | q04-date-range | v1.5 0f465633 | 1.976 | 0.100 | 2.403, 1.969, 1.953, 1.982 |
| lab-entities-10m | q04-date-range | v2 32ecf1e5 | 2.273 | 0.062 | 2.445, 2.118, 2.331, 2.215 |
| lab-entities-10m | q04-date-range | v2 32ecf1e5 (perf-lab datasets) | 2.292 | 0.096 | 1.992, 2.178, 2.406, 2.449 |
| lab-entities-10m | q05-group-born | v1.5 0f465633 | 0.056 | 0.112 | 0.063, 0.048, 0.057, 0.055 |
| lab-entities-10m | q05-group-born | v2 32ecf1e5 | 0.0575 | 0.070 | 0.052, 0.061, 0.056, 0.059 |
| lab-entities-10m | q06-label-prefix | main f9b515e8 (perf-lab datasets) | 1.099 | 0.077 | 1.167, 1.163, 1.009, 1.034 |
| lab-olympics | q02-star-team | main f9b515e8 (perf-lab datasets) | 5.33 | 0.111 | 5.2, 5.46, 5.672, 4.409 |
| lab-olympics | q02-star-team | v2 32ecf1e5 (perf-lab datasets) | 5.2 | 0.052 | 4.758, 5.215, 5.186, 5.369 |
| lab-olympics | q03-gold-by-team | v2 32ecf1e5 (perf-lab datasets) | 2.213 | 0.175 | 2.408, 1.695, 2.018, 2.465 |
| lab-olympics | q06-text-filter | main f9b515e8 (perf-lab datasets) | 4.848 | 0.102 | 5.799, 4.678, 4.655, 5.018 |
| lab-olympics | q07-games-by-year | main f9b515e8 (perf-lab datasets) | 0.068 | 0.170 | 0.066, 0.086, 0.057, 0.07 |
| lab-olympics | q07-games-by-year | v2 32ecf1e5 | 0.0815 | 0.113 | 0.075, 0.087, 0.095, 0.076 |
| lab-olympics | q07-games-by-year | v2 32ecf1e5 (perf-lab datasets) | 0.0845 | 0.131 | 0.083, 0.086, 0.099, 0.072 |
| lab-olympics | q08-event-hierarchy | v1.5 0f465633 | 0.307 | 0.050 | 0.314, 0.3, 0.329, 0.294 |
| lab-olympics | q08-event-hierarchy | v2 32ecf1e5 | 0.295 | 0.053 | 0.321, 0.289, 0.285, 0.301 |
| lab-olympics | q10-ask | main f9b515e8 (perf-lab datasets) | 0.2305 | 0.053 | 0.218, 0.236, 0.225, 0.246 |
| lab-olympics | q10-ask | v2 32ecf1e5 (perf-lab datasets) | 0.208 | 0.114 | 0.229, 0.187, 0.188, 0.228 |
| lab-yago-tiny | q03-taxon-path | v1.5 0f465633 | 4.303 | 0.115 | 4.81, 4.36, 4.247, 3.641 |
| lab-yago-tiny | q03-taxon-path | v2 32ecf1e5 | 4.09 | 0.094 | 4.162, 4.629, 3.693, 4.017 |
| lab-yago-tiny | q03-taxon-path | v2 32ecf1e5 (perf-lab datasets) | 4.104 | 0.063 | 4.409, 3.931, 4.278, 3.873 |
| lab-yago-tiny | q04-movie-actors | main f9b515e8 (perf-lab datasets) | 0.148 | 0.098 | 0.148, 0.179, 0.148, 0.146 |
| lab-yago-tiny | q04-movie-actors | v1.5 0f465633 | 0.179 | 0.058 | 0.188, 0.181, 0.164, 0.177 |
| lab-yago-tiny | q05-population | v1.5 0f465633 | 1.521 | 0.153 | 1.931, 1.398, 1.406, 1.636 |
| lab-yago-tiny | q05-population | v2 32ecf1e5 | 1.504 | 0.088 | 1.377, 1.562, 1.445, 1.684 |
| lab-yago-tiny | q07-class-count | main f9b515e8 (perf-lab datasets) | 0.1475 | 0.222 | 0.204, 0.156, 0.139, 0.121 |
| lab-yago-tiny | q07-class-count | v1.5 0f465633 | 0.1645 | 0.118 | 0.142, 0.187, 0.157, 0.172 |
| lab-yago-tiny | q07-class-count | v2 32ecf1e5 | 0.158 | 0.099 | 0.178, 0.148, 0.168, 0.145 |
| lab-yago-tiny | q07-class-count | v2 32ecf1e5 (perf-lab datasets) | 0.163 | 0.160 | 0.199, 0.138, 0.152, 0.174 |
| ql-owl2bench | q01 | v2 32ecf1e5 | 0.427 | 0.105 | 0.4495, 0.496, 0.394, 0.4045 |

## Others: profile each (6)

No common shape; each needs a look (a profile of a slow and a fast pass side by side).

| Case | Series | Binary | Median ms | Noise (sd) | Pass medians, ms |
|---|---|---|---|---|---|
| geo-within | within-3 | v1.5 0f465633 | 23.14 | 0.059 | 22.53, 25.15, 22.06, 23.75 |
| geo-within | within-3 | v2 32ecf1e5 | 23.12 | 0.062 | 22.56, 21.99, 25.3, 23.68 |
| q-paths | follows-closure-count | v2 32ecf1e5 | 134.4 | 0.095 | 133.9, 162.2, 134.7, 134.1 |
| rdf12-annotations | from-source-7 | v1.5 0f465633 | 0.6118 | 0.073 | 0.6315, 0.586, 0.687, 0.592 |
| rl-sameas | _ | v1.5 0f465633 | 499.1 | 0.055 | 494.4, 555.2, 497.4, 500.7 |
| rule-delta-sweep | _ | v1.5 0f465633 | 7.794 | 0.089 | 7.788, 7.26, 8.974, 7.801 |
