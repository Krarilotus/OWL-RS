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
| `commit` | the pre-commit hook | rustfmt; clippy (warnings are errors) on every target of changed crates; compile their dependents' libraries; run the changed crates' fast tests |
| `push` | the pre-push hook | changes since the upstream branch: rustfmt and clippy on changed crates; all their tests (slow suites too), dependents' fast tests, additional random seeds for changed crates, and the lock check |
| `all` | milestones, merges to `main`, CI | everything: both Cargo workspaces, doc tests, the lock check, `cargo-deny` (licences, advisories, bans, sources; `deny.toml`), benchmark-suite contract tests, the console's typecheck and tests |

- A crate declares its slow test binaries (conformance suites, fuzz campaigns: over ~15 s)
  in its manifest, `[package.metadata.nrese] slow-tests = [...]`.
- Tests run under `cargo-nextest` where it is installed (`cargo install --locked
  cargo-nextest`): every test of every binary in parallel. Without it, `cargo test` runs
  crate by crate.
- Enable the hooks with `git config core.hooksPath .githooks`.

## Building and measuring

Build through `scripts/cargo-guarded.sh` (a disk budget, a memory cap, at most
`NRESE_BUILD_SLOTS` builds at once per machine). Validate in batches: quick checks while
working, the push tier once per batch. Measure counts first; time only to answer a stated
question, inside `scripts/quiet-slot.sh`, on `release` (`--profile lab` to iterate). A slot
holds one short measurement, since builds wait for it; long comparison runs go to a machine of
their own. Every number names the commit its binary was built from and whether the tree was
clean; a binary kept for comparison is rebuilt after its source changes or is reverted.

CI (`.github/workflows/ci.yml`, the same steps on Linux) runs at milestones and on merges to
`main`: `gh workflow run ci.yml --ref <branch>`; the Jena oracle likewise (`oracle.yml`).
