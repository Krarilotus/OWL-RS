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
- Hosts and roles: [benches/hosts.toml](benches/hosts.toml); verified shared inputs are
  read-only. Rust is compiled, tested and formatted on the build host: `scripts/lib/remote.sh`
  sends the working tree to it (`git config nrese.buildHost`). The machine you run on is the
  owner's workstation: it may build (`NRESE_BUILD_HOST=local`), but benchmarks, reasoners,
  containers and large data run on the measuring and preparing hosts. Its guard stops our
  heavy processes when memory or disk run low. Every run on a host goes through the
  measurement queue (`benches/suite/queue.py`), never started by hand: the queue packs
  shareable jobs side by side and gives timed ones the host alone.
- `literature/` is read-only. Results of licensed systems are never committed.
- Write files with the editor tools; the shell mangles backslashes.
- Commit messages carry no AI co-author trailers (`Co-Authored-By: Claude …` and the like);
  the `commit-msg` hook removes them.

## Reporting

What changed, the evidence for it, what is left, and any decision you need.
