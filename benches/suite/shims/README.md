# Per-revision shims

A timing harness file is byte-identical on every side of a comparison. Where an older revision
lacks an API the harness calls, the one call that differs goes into a small file of its own
beside the harness, and this directory holds that file's version for the revision:
`shims/<revision>/<harness>/<file>` is copied over the harness's own before the side is built.
Record each side's shim hash in the run's manifest.

| Revision | Harness | File | Why |
|---|---|---|---|
| f9b515e8 (main) | http_lab | load.rs | `BulkLoadRequest` has `skip_errors`, not `invalid` |
