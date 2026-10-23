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
| `push` | the pre-push hook | the committed delta from `scripts/lib/push-base.txt`: complete Linux and native Windows Fleet parts, with source/tool identity and required test inventory checked by the producer |
| `all` | milestones, merges to `main`, CI | everything: both Cargo workspaces, doc tests, the lock check, `cargo-deny` (licences, advisories, bans, sources; `deny.toml`), benchmark-suite contract tests, the console's typecheck and tests |

- A crate declares its slow test binaries (conformance suites, fuzz campaigns: over ~15 s)
  in its manifest, `[package.metadata.nrese] slow-tests = [...]`.
- Tests run under `cargo-nextest` where it is installed (`cargo install --locked
  cargo-nextest`): every test of every binary in parallel. Without it, `cargo test` runs
  crate by crate.
- Enable the hooks with `git config core.hooksPath .githooks`.

## Building and measuring

Fleet is the execution entrypoint for builds, tests and measurements on fleet hosts.
The [tooling operations guide](docs/dev/fleet-operations.md) covers preparation,
submission, collection and recovery. Its Fleet job guide owns current host rules and
notices; `benches/hosts.toml` and old SSH/queue recipes do not override them.

Run `bash scripts/check.sh push` from the intended committed checkout. It submits
both platform parts and returns 75 while they are pending or lack a valid verdict.
After completion, invoke the same command to collect and validate the saved request;
it does not submit a duplicate merely because the operator checks again. It returns
0 only for the matching complete evidence, or 1 for a validated check failure.
There is no direct-host or local fallback for this ordinary push path. Keep the
normal commit hooks and their existing local admission contract.

Build through `scripts/cargo-guarded.sh` inside the admitted execution path. Validate
coherent changes together; use narrow checks to choose and verify a repair. Measure
work counts first, then use [ABBA](benches/abba/README.md) for targeted timing
certification through Fleet. A diagnostic run is not release qualification. Every
number names the binary's source, compiler, settings, inputs and actual host. Retain
a compatible existing binary while its planned comparisons still need it.

Keep a separate Cargo target directory for each worktree; share downloaded registry
dependencies through `CARGO_HOME`, never build artifacts of different revisions.

CI (`.github/workflows/ci.yml`, the same steps on Linux) runs at milestones and on merges to
`main`: `gh workflow run ci.yml --ref <branch>`; the Jena oracle likewise (`oracle.yml`).
