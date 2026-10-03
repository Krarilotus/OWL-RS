# Run records

One TOML file per benchmark run. A record says what the run covered, on which commit and
machine, under which protocol, and how each pair came out. It never holds times: those
stay in the run's results directory, which git ignores (some vendors forbid publishing
them). [../STATUS.md](../STATUS.md) is generated from these records and the registries.

```sh
python benches/suite/suite.py ledger benches/suite/results/<run>   # write or refresh a suite run's record
python benches/suite/suite.py ledger --all                         # every local results directory
python benches/suite/suite.py status --write                       # regenerate ../STATUS.md
```

Refreshing a record keeps the hand-written fields. A run made on another machine (the
office PC, Draco) gets its record there; the records come here by git.

## Fields

| Field | Source | Meaning |
|---|---|---|
| `id` | generated | the results directory's name |
| `kit` | generated / by hand | `suite`, or the kit's directory (`reasoning/dl`) |
| `purpose` | **by hand** | why the run was made |
| `baseline` | **by hand** | the run it is compared with |
| `findings` | **by hand** | what the comparison showed: regressions, wrong answers, fixes made, with commits |
| `started`, `finished` | manifest | times of the first and last invocation |
| `host`, `machine` | manifest | CPU, threads, RAM, OS, Docker's limits |
| `commit`, `dirty` | manifest | the commit(s), and how many tracked files differed from it |
| `protocol` | manifest | repetitions, measured query runs, cache modes, order and seed, limits, runtime |
| `commands` | manifest | the driver invocations |
| `results` | generated | where the raw results are |
| `publishable` | generated | `all`, or `free systems only` when a licensed system ran |
| `pairs` | generated | one entry per workload, tier and system (below) |

Runs from before the manifests (before 3 October 2026) have `commit = "unknown (before run
manifests)"` unless it was added by hand.

## A pair's outcome

| Outcome | Meaning |
|---|---|
| `ok` | every repetition loaded, every query answered, every checked count right |
| `partial` | loaded, but some queries failed or timed out (`note` names them), or a corpus with unsolved tasks |
| `wrong` | an answer count differs from the expected one: a correctness finding |
| `failed`, `timeout` | the load, or the kit, failed or hit its limit |
| `skipped` | the suite didn't run it (`note` says why: a missing capability, licence, regime or input) |
| `restricted` | a licensed system ran; how it came out stays local until the vendor permits publishing |

`runs` counts the repetitions that loaded, and `items` gives the queries or tasks that were
`ok` out of those run.

## Kits with their own driver

These kits write their record by hand, in the same format: `kit` names the directory,
`pairs` uses the workload names of `suite/workloads.toml` and the system names of
`suite/systems.toml`, and `results` points to the kit's result files.
[2026-10-03-dl-reference.toml](2026-10-03-dl-reference.toml) is the example.
