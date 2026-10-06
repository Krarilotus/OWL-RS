# Contributing

NRESE is licensed under the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence ([LICENSE-MIT](LICENSE-MIT)), at your option.

## Contributions

- Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in NRESE by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions. No separate contributor agreement is needed.
- Don't paste code from other projects into an issue or pull request unless its licence is MIT, Apache-2.0, BSD or similar, and say where it comes from.

## Dependencies

- A new dependency must be permissively licensed, so that NRESE stays usable under MIT or Apache-2.0 alone: MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, and similar. MPL-2.0 only unmodified, and only after asking.
- GPL, LGPL (statically linked), AGPL and source-available dependencies aren't accepted: they would impose their terms on everyone who uses NRESE.
- Prefer none at all: the rules are in [ADR-0010](docs/adr/0010-standing-product-decisions.md) (libraries behind our own traits; no C or C++ for speed).

## Working rules

Layer ownership, where to fix what, and the evidence a change needs are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/dev/code-structure-guidelines.md](docs/dev/code-structure-guidelines.md).

## Checks

`scripts/check.sh` is the gate, in three tiers. A test runs where it matters: each crate
owns its tests, and benchmarks never run in a gate.

| Tier | When | What |
|---|---|---|
| `commit` | the pre-commit hook | rustfmt; clippy (warnings are errors) on the changed crates and every crate depending on them; the changed crates' fast tests |
| `push` | the pre-push hook | the same for everything changed since the upstream branch, with all the changed crates' tests (their slow suites too), the dependents' fast tests, and the lock check |
| `all` | milestones, merges to `main`, CI | everything: both Cargo workspaces, doc tests, the lock check, `cargo-deny` (licences, advisories, bans, sources; `deny.toml`), the console's typecheck and tests |

- A crate declares its slow test binaries (conformance suites, fuzz campaigns: over ~15 s)
  in its manifest, `[package.metadata.nrese] slow-tests = [...]`.
- Tests run under `cargo-nextest` where it is installed (`cargo install --locked
  cargo-nextest`): every test of every binary in parallel. Without it, `cargo test` runs
  crate by crate.
- Enable the hooks with `git config core.hooksPath .githooks`.

## Building and measuring on a shared machine

- **Batch, then validate.** Implement with quick checks (`check`, the touched crate's tests),
  batch related fixes, and run the expensive validation once per batch: the push tier, a
  release build, a benchmark.
- **A/B only for a question.** Run an A/B when a change has a concrete performance question
  (a hypothesis with the measurement that decides it), not after every fix; counts first,
  timings to confirm.
- **Use a wait for review, when there is something to review.** While a build or a slot is
  pending: read your own diff again. Is the change at the layer that owns the concept
  (ARCHITECTURE §3)? Would a broader fix solve this and its neighbours? What would make it
  simpler, or the next step better? Don't invent work when there is nothing meaningful to
  reconsider.
- **At most three builds at once on a machine** (`NRESE_BUILD_SLOTS`, default 3):
  `scripts/cargo-guarded.sh` waits for a free build slot; waits are logged in
  `tmp/guard-slots.log`.

- **While editing:** `scripts/cargo-guarded.sh check` and the changed crate's tests; for a
  long edit loop on one crate, `CARGO_INCREMENTAL=1` (the build directory's budget still
  applies). The full suite runs at the push tier and at milestones.
- **Iterating on an A/B:** `--profile lab` (release code with thin LTO and parallel code
  generation, much faster to link). Numbers that are kept, logged or published are measured
  on `release` (fat LTO, one codegen unit).
- **Timings in a quiet slot:** `scripts/quiet-slot.sh <command>` takes a machine-wide lock
  and waits until no compiler of ours runs; builds started meanwhile wait for the slot. Counts
  and other deterministic measures need no slot.
- **The build directory's budget:** over it, `scripts/cargo-guarded.sh` first removes the stale
  builds cargo keeps of every crate, then the debug output, then everything; each removal is
  logged in `tmp/guard-wipes.log`.
- CI (`.github/workflows/ci.yml`, the same steps on Linux) runs at milestones and on
  merges to `main`: `gh workflow run ci.yml --ref <branch>`; the Jena oracle likewise
  (`oracle.yml`).
