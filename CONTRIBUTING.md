# Contributing

NRESE is licensed under the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence ([LICENSE-MIT](LICENSE-MIT)), at your option.

## Contributions

- Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in NRESE by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions. No separate contributor agreement is needed.
- Don't paste code from other projects into an issue or pull request unless its licence is MIT, Apache-2.0, BSD or similar, and say where it comes from.

## Dependencies

- A new dependency must be permissively licensed, so that NRESE stays usable under MIT or Apache-2.0 alone: MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, and similar. MPL-2.0 only unmodified, and only after asking.
- GPL, LGPL (statically linked), AGPL and source-available dependencies aren't accepted: they would impose their terms on everyone who uses NRESE.
- Prefer none at all: see the rules in [docs/plan/2026-09-30-graphdb-parity-plan.md](docs/plan/2026-09-30-graphdb-parity-plan.md) §6.

## Working rules

Layer ownership, where to fix what, and the evidence a change needs are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/dev/code-structure-guidelines.md](docs/dev/code-structure-guidelines.md).

## Checks

- `scripts/check.sh` is the gate every change passes before it is pushed: rustfmt, clippy on all targets with warnings as errors, the tests of both Cargo workspaces, the lock check, `cargo-deny` (licences, advisories, bans, sources; `deny.toml`) and the console's typecheck and tests. `scripts/check.sh --changed` runs fmt, clippy and the tests of the crates you changed.
- Git hooks run them for you: `git config core.hooksPath .githooks` (`pre-commit`: the changed crates; `pre-push`: the whole gate).
- CI (`.github/workflows/ci.yml`, the same steps on Linux) runs at milestones and on merges to `main`: `gh workflow run ci.yml --ref <branch>`; the Jena oracle likewise (`oracle.yml`).
