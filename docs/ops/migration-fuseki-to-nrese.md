# Fuseki to NRESE Migration Runbook

## Purpose

This runbook defines a controlled migration path from Apache Fuseki to NRESE with explicit rollback safety.
It is designed for production teams that require semantic compatibility, data integrity, and predictable cutover behavior.

## Scope

In scope:

- single-dataset migration from Fuseki to one NRESE node
- compatibility validation on real ontology + real query/update workload
- controlled cutover and rollback

Out of scope:

- multi-node HA migration orchestration
- cross-region failover automation

## Preconditions

- Current Fuseki instance is healthy and reachable.
- NRESE target build is pinned to a release artifact.
- Ontology + dataset export rights are available.
- Reverse proxy and DNS/traffic switching mechanism is prepared.
- Rollback owner and approval chain are explicitly assigned.

## Required Artifacts

- Fuseki dataset export snapshot (N-Quads preferred)
- NRESE config bundle (`nrese.env`, config overrides, proxy config)
- Query/update compatibility test set
- Pilot migration report template (see `pilot-migration-report-template.md`)
- Rollback runbook (see `rollback-runbook.md`)

## Migration Phases

## 1. Baseline Capture

1. Freeze a baseline observation window on Fuseki.
2. Record p95 query latency, p99 update latency, error rates, and representative query outputs.
3. Export the production dataset snapshot.
4. Store immutable checksum metadata for exported artifacts.

Acceptance:

- baseline metrics and exported data are versioned and reproducible

## 2. Staging Rehearsal

1. Restore exported Fuseki data into NRESE staging.
2. Run compatibility test pack against Fuseki and NRESE side-by-side.
3. Compare result sets, documented Graph Store failure semantics, and error/problem-detail behavior for the covered parity fixture set.
4. Document intentional differences (if any) with owner sign-off.

Acceptance:

- no unexplained semantic, protocol, or covered Graph Store failure-parity regressions remain

## 3. Pilot Migration

1. Execute pilot using the template from `pilot-migration-report-template.md`.
2. Route a controlled subset of client traffic to NRESE.
3. Observe metrics, logs, and operator feedback for defined soak duration.
4. Decide `promote` or `rollback` using agreed gates.

Acceptance:

- pilot gates are passed and signed off by platform + application owners

## 4. Production Cutover

1. Enable temporary write freeze on Fuseki (or single-writer guard).
2. Perform final incremental export and import into NRESE.
3. Execute mandatory smoke checks (`docs/ops/test-server-smoke-tests.md`).
4. Switch traffic at reverse proxy / gateway.
5. Monitor high-frequency for the first 60 minutes.

Acceptance:

- NRESE serves production traffic within agreed SLO envelope

## 5. Post-Cutover Verification

1. Re-run business-critical query/update workflows.
2. Validate backup job success on NRESE.
3. Confirm incident runbooks and alerts reference NRESE endpoints.
4. Mark migration complete only after observation window closes cleanly.

Acceptance:

- no open Sev-1/Sev-2 migration defects remain

## Rollback Trigger Conditions

Immediate rollback is required when one or more conditions hold:

- persistent semantic mismatch on critical queries
- sustained error-rate breach above accepted threshold
- data integrity uncertainty
- mutation pipeline instability with integrity risk
- inability to meet minimum service availability objective

Rollback execution:

- follow `docs/ops/rollback-runbook.md`

## Evidence Checklist

- dataset export checksums
- compatibility diff reports
- pilot report
- cutover timeline
- post-cutover validation report
- rollback readiness confirmation
