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
- Evidence decides: counts before times; a published result is a hypothesis until measured
  here; kept and rejected ideas both go into the lab log.
- When something is hard, record what you tried and the next angle, then keep going.

## Boundaries

- Work in your own branch and worktree. The maintainer merges and pushes. Never skip the hooks.
- Delete only what you created; stop processes by PID. Large datasets live once, read-only,
  in `C:\Users\Johannes\nrese-data\` on the main PC; point to them, never copy them into a
  worktree.
- `literature/` is read-only. Results of licensed systems are never committed.
- Write files with the editor tools; the shell mangles backslashes.
- A commit written by an AI agent ends with its `Co-Authored-By` trailer (Claude's:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`).

## Reporting

What changed, the evidence for it, what is left, and any decision you need.
