# Timing a change: A-B-B-A families

[`benches/suite/abba.py`](../suite/abba.py) is the one way to time a change against its base:
one family file, the fleet jobs it writes, and a verdict with an exit code. Nothing in it
re-implements timing or statistics:
- the blocks are the release gate's (`release-gate.sh block`): a Latin-square palindrome A A' B
  B A' A per case, every side a fresh copy of its build each block, the quiet watcher, one repeat
  of a disturbed block;
- the tests are `release_gate.py`'s: gate(), Holm over the family, rounds by decide(), the
  host's A-A' floor.

## Use

One command, nothing to commit (Git Bash, from a checkout of engine-v2):

```bash
python benches/suite/abba.py run --a <base> --b <change> --cases rl-sameas,rl-chain
python benches/suite/abba.py run --a <base> --b <change> --areas reasoning --plan   # the plan only
fleet wait set:abba-<name>-<b8>        # as a background task; then:
fleet results set:abba-<name>-<b8>     # its member's results hold abba-verdict.md, .json and abba-exit
```

- The family is built from [benches/cases/](../cases/README.md) at the harness's revision (default b):
  `--cases` by name, `--areas` every case of the area that has a content check and no declared
  outcome. A case that isn't there goes into the registry first, reviewed; a timing never defines one.
- The plan comes first: cases, hypotheses, blocks (from the cases' calibrated spread, at least one
  square; `--blocks` overrides), the estimate in host-hours with its exclusive share, and `fleet
  check`'s answer: which timing hosts can take it, and what runs or waits there now. `--plan` stops
  there; otherwise it submits one set.
- The job asks by capability, never a host: a Linux timing host that builds (roles measure and
  build), the cases' memory and data (`has`). It starts only where it can time, builds A, B and the
  harness there first and holds that host alone to its end (the fleet keeps a timed job's host only
  between exclusive steps, while the host takes timed work); its check records the host the fleet
  chose, whose floor the report reads. `--host H` pins it, with its reason.
- The family travels in the job (`ABBA_FAMILY`); `family.toml` beside the set file (default
  `benches/suite/results/abba/NAME-B8/`, git-ignored) is the same family, for a combined report:
  `abba.py report PATH/family.toml R1 R2`.
- A confirmation round: the same command with `--round 2`.
- A reviewed recurring family stays possible: `benches/abba/NAME.toml` (format in `abba.py`'s
  docstring; cases of its own allowed there, in the registry's schema), then `abba.py jobs
  benches/abba/NAME.toml --out DIR` and `fleet submit DIR/<set>.set.toml`. A family of registry cases
  runs from any file; one with cases of its own must be committed, since the harness reads them there.

The verdict's exit codes:
- 0: no loss shown.
- 1: a loss shown. Run a confirmation round (`--round 2`), then `abba.py report FAMILY R1 R2`. A
  round-2 job's own report alone says 3: it isn't rounds 1..2.
- 2: the family is invalid.
- 3: no verdict: evidence missing, incomplete or failed, a content check without a passing result,
  rounds that aren't distinct runs, or an A-A' arm that itself differs. That is a failed experiment,
  never a pass.
- 4: a confirmed loss (Holm over the family, the rounds combined).

## Rules

- **What may be timed:**
  - a job whose benchmark mode or harness is new waits for that harness's review r1;
  - cases and harness come from the current engine-v2 head, for both sides;
  - timing runs on the fleet's timing hosts only: the main PC builds and tests, it never times.
- **The comparison:** A-B-B-A, interleaved, exclusive, on one host, with each run's spread
  reported. A timing claim names its job, family and rule (Holm, the A-A' floor).
- **Effort follows spread** (owner, 20 Oct 2026):
  - size the first round from the case's recorded spread on that host: long, stable cases get
    one Latin square (3 blocks), noisy short ones more;
  - add blocks only to undecided cases: stop on precision, not on p-values;
  - confirm only flagged cases, with fresh data;
  - run an A' control only where no stable floor is recorded;
  - no fixed heavy layout.

  Today a family's blocks are one fixed count for every case (`blocks`, at least 3); `run` takes
  it from the noisiest case's calibration (benches/fast/measure.toml). The adaptive sizing (per-case
  looks, a target of 3 % by default) comes with harness/abba-adaptive.

Don't time a change with a script of your own. Three such scripts (20 Oct 2026) failed open on
missing runs, had no interleaved A-A' arm, or counted results instead of checking them.

## Why A' and a copy per block: the session bias

Byte-identical binaries read significantly apart in both directions in different sessions:
- On office-pc, lks-abba-11 and lks-abba-13 timed the same SHA-checked pair (075f7b7c against
  c9be1609). creaders `commits.read_p50_ms` read x1.032 in one and x0.951 in the other, both at
  p = 0.001 with 8 runs a side; crdfs `dept` behaved alike (±4 to 6 %).
- lks-aa-20 timed one build against a byte-identical copy: the copy read 4.7 % apart on that
  series in every block of the session, against 0.75 % between blocks.

The offset belongs to the file's cached instance and stays while the file is in the page cache.
So interleaving within one session doesn't cancel it, and a test over that session's runs calls
it a loss.

The harness answers it in two ways:
- **A copy of each build per block** (`fast.py block --fresh-copies`) draws the offset anew each
  block, so it becomes the blocks' variance instead of a bias. Validated on 20 Oct 2026:
  - lks-aa-21, lks-aa-20 replayed with a copy per block: the 4.7 % offset was gone, within 1.2 to
    1.9 standard errors;
  - ff-phuoc-851e987d on phuoc-yu: a fixed pair kept cold-start offsets of 2.3 to 7.5 %, gone with
    copies.

  The analyses are Harness/notes/2026-10-20-lks-aa-21-copies.md and 2026-10-20-ff-phuoc-copies.md.
- **The A-A' arm** (A' is A's build again, a copy of its own, interleaved in every block) measures
  the round's own floor. A round whose A-A' arm itself differs has no verdict (3). B against A is
  judged with a spread of at least that arm's, or the host's recorded floor
  (`benches/fast/noise-floor/HOST.toml`) where larger.

Before you chase a small loss in a path the change doesn't touch:
- read its history across rounds;
- check the A-A' arm and the floor in the verdict;
- report that history with the loss.

A ±4 % row at p = 0.001, even one that passes Holm, can be this artefact when it's measured
without them.
