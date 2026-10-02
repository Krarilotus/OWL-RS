# Rollback Runbook

## Purpose

Provide a deterministic rollback path from NRESE back to Fuseki when migration or post-cutover stability gates fail.

## Preconditions

- Last known-good Fuseki deployment package is available.
- Last known-good Fuseki dataset snapshot is verified.
- DNS/reverse proxy switching controls are available.
- Incident commander and rollback approver are assigned.

## Rollback Decision Inputs

- active incident severity
- data integrity confidence
- current error rate trend
- expected time-to-recovery in NRESE versus rollback time

## Rollback Procedure

1. Declare rollback event in incident channel.
2. Stop write traffic to NRESE.
3. Capture NRESE state snapshot for forensic analysis.
4. Switch traffic routing back to Fuseki.
5. Validate Fuseki health and critical workflows.
6. Announce rollback completion.

## Validation Checklist

- `/healthz` equivalent returns healthy on Fuseki
- critical query pack returns expected results
- update operations complete without errors
- dashboards and alerting point to active Fuseki target

## Data Integrity Handling

- Do not discard NRESE snapshots collected during incident.
- Preserve migration logs and request-correlation identifiers.
- Record any writes accepted by NRESE after cutover and before rollback.

## Post-Rollback Actions

1. Freeze further migration attempts until root cause is analyzed.
2. Produce rollback incident report within 24 hours.
3. Add regression checks to migration harness before next pilot.
