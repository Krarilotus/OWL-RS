# Backup and Restore Drills

## Purpose

Define repeatable backup/restore drills that prove recovery readiness under realistic failure scenarios.

Status tracking for this concern lives in [docs/spec/06-fuseki-replacement-gap-matrix.md](../spec/06-fuseki-replacement-gap-matrix.md).

This document is the operational source of truth for backup/restore procedure and evidence.

## Backup Artifact Contract

Every drill must use an artifact with these recorded fields:

- artifact id
- format
- creation time
- source revision
- quad count
- checksum

## Restore Verification Contract

Every restore drill must record these checks:

- readiness reaches `ready`
- restored revision is recorded
- critical parity query pack pass/fail is recorded

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
2. Restore into clean staging data directory.
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
