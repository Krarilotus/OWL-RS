# Working in OWL-RS

Read [docs/README.md](docs/README.md) (which document answers what),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) (who owns what) and
[CONTRIBUTING.md](CONTRIBUTING.md) (building, the gate, measuring).

## Principles

- Understand a problem on every level it touches, from the question asked down to the
  machine code, and across the owners involved, until its general cause is clear. Fix it in
  the layer that owns that cause; prefer the change that improves several paths, and remove
  what it makes redundant.
- Keep the measured wins ([performance.md](docs/design/performance.md) §0). A new win comes
  with its guard, a cheap deterministic test, in the same commit.
- Correctness before speed: an optimisation has a differential test against an oracle, a fix
  a test that fails without it.
- No hard-coded limits: every budget (time, memory, nodes, candidates) is a setting with an
  "unlimited" value and a documented default. Benchmarks give every system the same limits.
- Evidence decides: counts before times; a published result is a hypothesis until measured
  here; kept and rejected ideas both go into the lab log.
- When something is hard, record what you tried and the next angle, then keep going.

## Boundaries

- Work in your own branch and worktree. The maintainer merges and pushes. Never skip the hooks.
- Delete only what you created unless the user authorizes a wider cleanup; stop
  processes by PID.
- Builds, tests, benchmarks, reasoners, containers and large data run as fleet jobs, never
  started by hand on a host: `fleet rules` says what may run where, `fleet why <job>` explains
  a placement. The push tier and the Windows check already run that way
  ([CONTRIBUTING.md](CONTRIBUTING.md#building-and-measuring)); a change is timed only through
  [benches/abba/README.md](benches/abba/README.md). Verified shared inputs are read-only.
- The machine you run on is the owner's workstation: its commit tier needs no build, and its
  guard stops our heavy processes when memory or disk run low.
- Before building a tool, look in [docs/README.md](docs/README.md#where-is) for the one that
  exists.
- `literature/` is read-only. Results of licensed systems are never committed.
- Before writing a research prompt or a design from scratch, ask what prior research already
  found: `literature/wiki/00 Index.md`, the research rounds in
  `literature/wiki/research/Research reports.md`. It is untracked and lives in the main
  checkout only: in a linked worktree, look under the first path `git worktree list` prints.
- Write files with the editor tools; the shell mangles backslashes.
- Commit-message rules: [CONTRIBUTING.md](CONTRIBUTING.md).

## Reporting

What changed, the evidence for it, what is left, and any decision you need.
