# Code rules

The principles (ownership, functional core and imperative shell, DRY, one error enum per
crate) are in [ARCHITECTURE.md §6](../ARCHITECTURE.md#6-design-principles), and the layer
that owns each kind of fix in [§3](../ARCHITECTURE.md#3-where-to-fix-what). These are the
working rules on top of them.

## Modules

- A file over about 600 lines, or with a second responsibility, is split before the next
  feature goes in. A concern that has a public façade plus internal helpers, or several
  files sharing its types, gets a folder whose `mod.rs` re-exports only the boundary.
- Same mapping, parsing or projection needed twice: extract it, don't copy it. When a new
  path makes an old one redundant, the old one goes in the same change.
- Performance-sensitive code states its complexity in a doc comment, and the win it
  carries has a guard ([performance.md](../design/performance.md) §0).

## Configuration

- Typed in the crate that owns the behaviour; parsed (environment, file, CLI) only in
  `crates/nrese-server/src/config/`. Environment names and defaults are defined once.
- Cargo features switch build-time capabilities; runtime settings switch behaviour. A
  setting that changes cached results is part of the cache's key.
- A setting is documented in [config-reference.md](../ops/config-reference.md) in the
  change that adds it, and only once it works.

## Tests

- Unit tests next to the code they test; crate contracts in the crate's one integration
  binary (`tests/it`). A test lives in the crate that owns the behaviour it checks.
  A cross-component invariant belongs at the layer that composes those components
  (for example, the store's atomic mutation pipeline); it may legitimately fail after
  a dependency changes. Keep lower-level regression tests at the cause as well.
- Every optimisation has a differential test against an oracle; every fix a test that
  fails without it.
- Small RDF fixtures inline; larger ones under the crate's `tests/` and fetched corpora
  never committed.
- The console is tested through its public components and client, not its styling.

## Finishing a change

The gate is [`scripts/check.sh`](../../scripts/check.sh); its tiers are described in
[CONTRIBUTING.md](../../CONTRIBUTING.md):

Use that definition rather than maintaining a second tier list here. Benchmarks never
run in a gate.

Test placement follows the invariant's owner, not which tier happens to run it. A narrow
commit gate does not establish that cross-component invariants passed; the push and full
tiers provide broader coverage. Slow binaries are declared in crate metadata.
Before closing a work package: dead code and stale helpers removed, duplicated builders
collapsed, large files split, the docs it changes updated.
