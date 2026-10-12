# Run records

One TOML file per benchmark run: what it compared, on which builds and host, under which
protocol, and how each case came out. Raw samples stay in the run's results directory,
which git ignores (some vendors forbid publishing them); the record names it with its
SHA-256. [../STATUS.md](../STATUS.md) is generated from these records and the registries.

```sh
python benches/suite/suite.py ledger benches/suite/results/<run>   # write or refresh a suite run's record
python benches/suite/suite.py ledger --all                         # every local results directory
python benches/suite/suite.py status --write                       # regenerate ../STATUS.md
```

Refreshing a record keeps the hand-written fields. A run on another host gets its record
there; records come here by git. A kit with its own driver writes its record by hand in
the same shape ([2026-10-11-horn-recovery.toml](2026-10-11-horn-recovery.toml) is one).

## Fields

| Field | Meaning |
|---|---|
| `id` | the file's name: date, then what was measured |
| `level` | `unit`, `focused`, `full` or `cluster` ([../README.md](../README.md#four-levels)) |
| `kit` | the driver: `suite`, `fast`, a kit's directory |
| `purpose` | the question the run answers, one sentence |
| `started`, `finished` | ISO times (a date where nothing finer is known) |
| `host`, `build_host` | [hosts.toml](../hosts.toml) keys; a list when one comparison ran per host |
| `scenario` | `native`, `matched` or `frontier` ([PROTOCOL.md §3](../PROTOCOL.md#3-fairness)) |
| `baseline`, `candidate` | each `{ commit, dirty, patch_sha256, binary_sha256, note }`; a suite run's `baseline` may name the run it is compared with |
| `protocol` | pairs, order, repetitions, limits, seeds: what a rerun needs |
| `inputs` | `{ name, sha256 }` or `{ name, source }` per input |
| `results` | `{ root = "host:path", ... }` with the SHA-256 of what it holds; never a path into a local `tmp/` |
| `publishable` | `all`, or `free systems only` when a licensed system ran |
| `cases` | one verdict per case (below), its numbers in `note` |
| `findings` | what the verdicts don't say, a line each |
| `pairs` | suite runs: one outcome per workload, tier and system (below), generated |

## A case's verdict

| Verdict | Meaning |
|---|---|
| `faster`, `slower` | beyond the comparison's threshold ([fast/README.md](../fast/README.md#comparing)) in every pair |
| `fewer`, `more` | a work or allocation count changed |
| `same` | within the threshold; not proof of equal performance |
| `no-verdict` | below the floor, or a diagnostic |
| `pass`, `match` | the checks pass; the answers equal the reference's |
| `wrong`, `mismatch`, `failed` | a check fails: a correctness finding, or a run that broke |
| `boundary` | the case's declared outcome (memory limit, unsupported, gave up): coverage, no time |

## A pair's outcome

| Outcome | Meaning |
|---|---|
| `ok` | every repetition loaded, every query answered, every checked count right |
| `partial` | loaded, but some queries failed or timed out (`note` names them), or a corpus with unsolved tasks |
| `wrong` | an answer count differs from the expected one: a correctness finding |
| `failed`, `timeout` | the load, or the kit, failed or hit its limit |
| `hangs` | a comparator gave no answer in a past run and isn't run again |
| `skipped` | the suite didn't run it (`note` says why: a missing capability, licence, regime or input) |
| `restricted` | a licensed system ran; how it came out stays local until the vendor permits publishing |

`runs` counts the repetitions that loaded, and `items` gives the queries or tasks that were
`ok` out of those run. Records from before 3 October 2026 have `commit = "unknown (before
run manifests)"` unless it was added by hand.
