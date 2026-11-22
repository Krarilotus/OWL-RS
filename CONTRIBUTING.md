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
owns its tests, and benchmarks never run in a gate. You can run the first and the last tier
yourself ([on your own machine](#on-your-own-machine)); the middle one runs on the project's own
machines, for every pull request before it merges ([below](#what-runs-only-on-the-projects-machines)).

| Tier | When | What |
|---|---|---|
| `commit` | the pre-commit hook, also called by pre-merge-commit for clean merges | seconds, on your machine, with no build: rustfmt; for the changed files, the lock check and the Docker-name check (`scripts/check-names.sh`). A branch may hold commits this tier didn't check: what lands is checked as a whole, by its exact tree, in the `push` tier and the Windows check |
| `push` | before a commit is pushed to this repository; on the project's machines | changes since the upstream branch: rustfmt and clippy on changed crates (every crate when the root `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/config.toml` or `clippy.toml` changed); all their tests (slow suites too), dependents' fast tests, additional random seeds for changed crates, and the lock check; the benchmark-suite contract tests when `benches/suite/`, `scripts/` or `.githooks/` changed |
| `all` | milestones, merges to `main`, CI; and on your own machine | everything: both Cargo workspaces, doc tests, the lock check, `cargo-deny` (licences, advisories, bans, sources; `deny.toml`), benchmark-suite contract tests, the console's typecheck and tests |

- A crate declares its slow test binaries (conformance suites, fuzz campaigns: over ~15 s)
  in its manifest, `[package.metadata.nrese] slow-tests = [...]`.
- Tests run under `cargo-nextest` where it is installed (`cargo install --locked
  cargo-nextest`): every test of every binary in parallel. Without it, `cargo test` runs
  crate by crate.
- Commit messages carry no AI-attribution trailers (`Co-Authored-By` naming a model,
  "Generated with" lines); a maintainer check refuses them.

## Building and measuring

- Build through `scripts/cargo-guarded.sh`: a disk budget, a memory cap, and at most
  `NRESE_BUILD_SLOTS` builds at once per machine.
- Validate in batches: quick checks while working, the `push` tier once per batch (on your own
  machine, the `all` tier).
- Measure counts first. Time only to answer a stated question, through `benches/suite/abba.py`
  ([benches/abba/README.md](benches/abba/README.md): A-B-B-A families on one of the fleet's timing
  hosts, never on this machine).
- Every number names the commit its binary was built from and whether the tree was clean; a binary
  kept for comparison is rebuilt after its source changes or is reverted.
- Keep a separate Cargo target directory for each worktree; share downloaded registry dependencies
  through `CARGO_HOME`, never build artifacts of different revisions.

CI (`.github/workflows/ci.yml`, the same steps on Linux) runs at milestones and on merges to
`main`, started by the maintainer (`gh workflow run ci.yml --ref <branch>`); the Jena oracle
likewise (`oracle.yml`).

## On your own machine

You need no access to the project's machines. The `commit` and `all` tiers run on one Linux
machine of your own. Install:

- Rust through [rustup](https://rustup.rs): `rust-toolchain.toml` selects the toolchain, with
  rustfmt and clippy.
- Node from nodejs.org at the version `scripts/lib/gate-tools.txt` names (the console refuses
  others).
- Python 3.13, with the suite's packages in an environment of their own that comes first on
  `PATH`: `python3.13 -m venv ~/.venvs/nrese-suite`, then
  `~/.venvs/nrese-suite/bin/pip install --require-hashes -r benches/suite/requirements.txt`. An
  older Python can't install the pinned packages.
- cargo-deny at its pinned version:
  `cargo install --locked cargo-deny@$(sed -n 's/^cargo-deny //p' scripts/lib/gate-tools.txt)`.

Then run `bash scripts/check.sh commit` and `NRESE_BUILD_HOST=local bash scripts/check.sh all`.
The full tier fetches the pinned W3C test suites and runs every step on this machine. From a
clean start, with 6 build jobs, it took about 20 minutes (15 Nov 2026).

Leave the repository's hooks off (don't set `core.hooksPath` to `.githooks`): its pre-push hook
asks the project's machines for a verdict and refuses a push without one. Push to a fork of your
own and open a pull request from it.

## What runs only on the project's machines

The `push` tier and the Windows check run as jobs on the project's own build and test machines,
and only their verdict lets a commit be pushed to this repository: no verdict, no push. This
doesn't stop you: you don't push here. Before your pull request merges, both run on its commits.

For the maintainer's checkouts:

- The hooks: if `core.hooksPath` isn't set, set it with `git config core.hooksPath .githooks`;
  if it is already set, keep it and make sure the hooks it runs also run `.githooks`.
- With `git config nrese.buildHost <ssh alias>`, `scripts/check.sh all` and
  `scripts/lib/remote.sh check|run` send the working tree to a build machine and run the command
  in a worktree of its own; `NRESE_BUILD_HOST=local` builds on this machine instead.
- `.githooks/pre-push` gates each pushed commit, by its own tree and the files from where the
  remote has its branch, as a job on the project's machines (`FLEET_OWNER` names who asked). A
  pass is remembered for the same tree, files and toolchain, so run `scripts/check.sh push` (it
  gates HEAD) before a long push: the hook then passes at once. A push that only deletes refs
  needs no gate.
- `scripts/check.sh windows [COMMIT]` (default HEAD) runs COMMIT's own `windows` tier on a Windows
  machine of the project's: the whole workspace built, all its tests, its doc tests and the suite
  contracts. A commit whose inputs (every file but the docs no other file names,
  `scripts/lib/gate-key.sh`) passed before carries that pass. A push to `refactor/engine-v2` needs
  both: the hook also runs the Windows part for the pushed commit, which a pass for the same
  inputs (the merge check's, run first) answers at once.
