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

1. Write a family, `benches/abba/NAME.toml`. Its format is in `abba.py`'s docstring:
   - sides a (the base) and b (the change), as commit ids;
   - one exclusive timing host;
   - blocks: at least 3, in whole squares;
   - its cases, each from benches/cases/ or of its own, each with a content check.
2. Check it: `python benches/suite/abba.py check benches/abba/NAME.toml`. An invalid family exits 2
   and says why.
3. Write the jobs: `python benches/suite/abba.py jobs benches/abba/NAME.toml --out DIR`. `--host
   H` moves every block to another exclusive host. The command writes a build job and a timed job
   (after it). Submit both: `fleet submit DIR/*.toml`.
4. Read the verdict: `fleet results abba-NAME-<b8>` holds abba-verdict.md, abba-verdict.json and
   abba-exit.
   - 0: no loss shown.
   - 1: a loss shown. Run a confirmation round: `abba.py jobs FAMILY --round 2`, then `abba.py
     report FAMILY R1 R2`. A round-2 job's own report alone says 3: it isn't rounds 1..2.
   - 2: the family is invalid.
   - 3: no verdict: evidence missing, incomplete or failed, a content check without a passing
     result, rounds that aren't distinct runs, or an A-A' arm that itself differs. That is a failed
     experiment, never a pass.
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

  Today a family's blocks are one fixed count for every case (`blocks`, at least 3). The
  adaptive sizing (per-case looks, a target of 3 % by default) comes with harness/abba-adaptive.
  Until then, choose `blocks` by the noisiest case you declare.

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
