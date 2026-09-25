# Pilot Migration Report Template

## Document Control

- Pilot ID:
- Date:
- Environment:
- NRESE Build:
- Fuseki Baseline Version:
- Owners:

## Objective

Define what this pilot must prove before production promotion.

Required objective categories:

- semantic compatibility
- protocol compatibility
- performance envelope
- operational stability

## Traffic Profile

- Workload source:
- Query mix summary:
- Update mix summary:
- Duration:
- Peak concurrency:

## Validation Gates

### Gate 1: Semantic Compatibility

- Result parity achieved for critical query set:
- Known intentional differences:
- Decision:

### Gate 2: Protocol Behavior

- Query/update/graph store endpoints validated:
- Error semantics, including covered Graph Store failure-parity cases, validated:
- Content negotiation validated:
- Decision:

### Gate 3: Performance

- p95 query latency:
- p99 update latency:
- CPU profile summary:
- Memory profile summary:
- Decision:

### Gate 4: Operability

- Alerting and dashboards verified:
- Backup/restore drill status:
- Incident response dry-run status:
- Decision:

## Incidents During Pilot

- Incident ID:
- Summary:
- Impact:
- Resolution:
- Preventive action:

## Promotion Decision

- Final decision: `promote` / `rollback` / `repeat pilot`
- Approvers:
- Follow-up actions:
