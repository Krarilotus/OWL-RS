# Operational Readiness Handbook

## Purpose

Define the minimum operational maturity required before NRESE is treated as a production platform component.

## Ownership Model

- Service owner: accountable for uptime, release quality, and incident coordination
- Data owner: accountable for semantic/data integrity and migration sign-off
- Security owner: accountable for auth/policy hardening and vulnerability response
- On-call owner: accountable for first-response incident execution

## Required Operational Capabilities

- health/readiness and metrics are monitored centrally
- request correlation IDs are available in logs and incident context
- backup and restore drills run on schedule
- rollback runbook is tested and current
- release gates include tests, smoke checks, and security review

## Release Readiness Checklist

- protocol smoke tests pass
- compatibility test set passes against target dataset
- benchmark baseline does not regress past accepted thresholds
- security configuration review completed
- rollback package and procedure verified

## Incident Readiness Checklist

- incident severity matrix published
- escalation path documented and reachable
- communication templates prepared
- recovery playbooks linked from on-call dashboard

## Change Management

- all production changes require change record
- high-risk changes require rollback rehearsal evidence
- post-change validation must include health/readiness + critical workload checks

## Review Cadence

- monthly operations review
- quarterly disaster-recovery review
- quarterly migration-readiness review while Fuseki coexistence remains active
