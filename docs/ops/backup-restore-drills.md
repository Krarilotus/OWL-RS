# Backup and Restore Drills

## Purpose

Define repeatable backup/restore drills that prove recovery readiness under realistic failure scenarios.

Status tracking for this concern lives in [docs/spec/06-target-capability-matrix.md](../spec/06-target-capability-matrix.md). The engine v2 durability design (WAL + checkpoints) is in [ADR-0002](../adr/0002-engine-storage-lsm-permutations.md); drills are revised with roadmap WP E4.

This document is the operational source of truth for backup/restore procedure and evidence.

## Backup Artifact Contract

Every drill must use an artifact with these recorded fields:

- artifact id
- format
- creation time
- source revision
- quad count
- checksum

Where they come from: an image backup's `manifest.json` (revision, counts, size, SHA-256). For the N-Quads export (`GET /ops/api/admin/dataset/backup`), the headers `x-nrese-source-revision` and `x-nrese-quad-count`, and the SHA-256 at the end: the file's last line `# nrese-sha256 <hex>` (over every byte before it, so it stays with the file) and the trailer `x-nrese-checksum-sha256` (with `curl`, send `TE: trailers`). The HTTP restore checks the last line and refuses a mismatch; record its `checksum_verified`. The offline `nrese-server load --replace` doesn't: check the file first with `head -n -1 export.nq | sha256sum` against that line.

## Restore Verification Contract

Every restore drill must record these checks:

- readiness reaches `ready`
- restored revision is recorded
- critical parity query pack pass/fail is recorded
- semantic restore outcome is recorded explicitly:
  - accepted and published
  - or rejected by the shared mutation gate without partial publication

## Evidence Schema

Each drill run must produce:

- `drill-report.json`
- `smoke-results.json`
- `query-diff.json`
- `checksums.txt`

## Drill Cadence

- Weekly: restore validation in staging from most recent backup
- Monthly: full recovery simulation including service restart and smoke test pack
- Quarterly: failure injection drill (interrupted update / abrupt process termination)

## Drill Scenarios

## Scenario A: Standard Restore

1. Take latest backup artifact.
2. Restore into clean staging data directory. For a store-sized backup, restore offline:
   - an image backup: `nrese-server restore DIR` (checked against its manifest);
   - an N-Quads export: `nrese-server load --replace export.nq` (with the target's configuration, `NRESE_STORE_MODE=on-disk`, `NRESE_DATA_DIR`).

   The HTTP restore (`POST /ops/api/admin/dataset/restore`) takes at most the server's body limit, about 3 × `budgets.upload_size` + 64 KiB (about 384 MiB by default), held whole while it is parsed: for small datasets only.
3. Start NRESE with restored dataset.
4. Run smoke tests and critical query pack.

Success criteria:

- service starts cleanly
- no data checksum mismatch
- critical queries return expected outputs

## Scenario B: Interrupted Update Recovery

1. Trigger controlled interruption during update workload.
2. Restart service in recovery-safe mode.
3. Validate dataset integrity and revision continuity.

Success criteria:

- no partial publication is observed
- integrity checks pass
- service returns to ready state

## Scenario C: Rollback Restore

1. Restore backup intended for rollback cutover.
2. Re-run migration compatibility pack against restored state.

Success criteria:

- restored state is operationally valid for fallback use
- rollback time objective is met

## Evidence to Capture

- backup artifact ID + checksum
- restore start/end timestamps
- smoke test report
- query diff report
- incident notes (if drill fails)

## Pass/Fail Rule

A drill passes only if:

- the backup artifact metadata is complete
- restore verification checks all pass
- no checksum or query parity failure is observed

Any failure triggers remediation and a repeat drill before the issue is considered closed.

## Failure Handling

- classify drill failure severity
- create remediation task with owner and due date
- rerun failed scenario after fix before closing task
