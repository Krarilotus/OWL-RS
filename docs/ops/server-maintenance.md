# Server Maintenance

## Routine Maintenance

- Monitor query latency, commit latency, and reasoning duration
- Verify storage capacity and backup freshness
- Review logs for repeated protocol or reasoning failures
- Verify `/ops` operator console and `/console` user console are reachable through the intended external ingress path
- Track compatibility changes before upgrading workspace dependencies
- Verify ontology preload source file presence and readability
- Confirm active ontology path matches expected environment configuration
- Keep migration and rollback documents current:
- `migration-fuseki-to-nrese.md`
- `rollback-runbook.md`
- `backup-restore-drills.md`
- `operational-readiness-handbook.md`

## Preventive Controls

- Run scheduled restore drills against recent backups.
- Review update queue growth and revise limits before saturation becomes operationally visible.
- Track dependency advisories for Rust crates and TLS infrastructure.
- Revalidate auth and reverse-proxy configuration after infrastructure changes.
- Re-run ontology preload smoke tests after path, filesystem, or deployment layout changes.

## Backup and Restore

- Backup only committed durable store state as the canonical source
- Treat inferred state as rebuildable unless future persistence rules specify otherwise
- Validate restore by starting the server and re-running smoke checks

## Observability Review

- Confirm metrics export still includes query, update, commit, and reasoning signals.
- Audit structured logs for request correlation and error classification.
- Confirm `/ops` and `/console` interactions are visible in request logs and follow expected auth policy.
- Keep alert thresholds aligned with current SLOs and traffic shape.
- Ensure startup logs include ontology preload result and resolved source path.

## Upgrade Discipline

- Run `cargo check` and repository tests before deployment
- Roll out configuration changes with explicit change logs
- Prefer reversible deployment steps and data-compatible releases
- Validate fallback path discovery behavior in staging before production rollout

## Incident Response Pattern

1. Stabilize the node and stop unsafe writes if data integrity is uncertain.
2. Confirm whether the fault is in storage, orchestration, or reasoning.
3. Restore service with the last known good committed state.
4. Rebuild inferred state if required.
5. Add regression coverage before the incident is closed.

## Incident Priorities

1. Protect committed data integrity.
2. Restore query availability.
3. Rebuild or refresh inference state.
4. Investigate root cause and add regression coverage.
