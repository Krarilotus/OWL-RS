# Storage and Transaction Model

## Objective

Provide a high-throughput, low-latency RDF storage layer with explicit transaction semantics and reasoner-friendly snapshot boundaries.

## Target Storage Capabilities

- Typed query and update execution APIs
- Default graph + named graph lifecycle operations
- Snapshot-aware read consistency
- Atomic commit boundaries for mutation batches
- Reasoner overlay compatibility
- Operational controls for backup, restore, and retention

## Storage Capability Tiers

### Tier S1: Baseline

- In-memory and local durable mode abstraction
- SPARQL query and update execution
- Ontology preload and data import primitives

### Tier S2: Protocol-Complete Dataset Behavior

- Graph Store operations (GET/PUT/POST/DELETE/HEAD)
- Dataset-level metadata and graph enumeration
- Media-type aware RDF parse/serialize flows

### Tier S3: Transaction and Snapshot Hardening

- Explicit revision IDs
- Atomic publish after validation gates
- Read isolation for concurrent query traffic
- Observable commit latency + queue pressure

### Tier S4: Enterprise Durability

- Backup/restore workflows bound to committed revisions
- Store-owned export/import primitives with a stable dataset artifact format
- Recovery validation and corruption detection flows
- Compaction/vacuum hooks and storage health metrics

### Tier S5: Scale and Distribution Readiness

- Abstractions for remote/object-backed snapshots
- Replication and partition strategy integration points
- Bounded consistency contracts for multi-node modes

## Transaction Lifecycle (Target)

1. Parse and validate mutation payload.
2. Normalize the request into one canonical mutation command.
3. Build a candidate revision through a side-effect-free preview path.
4. Trigger reasoner planning/execution on the preview snapshot if enabled.
5. Validate post-reasoning constraints.
6. Apply the canonical mutation to the live store and publish the new revision atomically.
7. Emit metrics + audit event.

## Query Visibility Model

- Queries read the latest committed revision.
- Inference visibility is policy-controlled:
- `explicit-only`
- `materialized-union`
- `profile-dependent`

## Operational Requirements

- Backups must represent committed canonical state.
- Restore validation must complete before the live dataset is replaced.
- Recovery must reject partial revisions.
- Metrics must include:
- Query latency
- Update latency
- Commit duration
- Active revision age
- Reasoning overlay freshness

## Conformance and Reliability Requirements

- SPARQL behavior aligned to supported W3C semantics
- Deterministic error classes for parser/evaluation/storage failures
- Load/regression test suite for mixed read/write pressure

## Acceptance Criteria

- Failed mutation or reasoning stage cannot leak partial state.
- Graph-level operations preserve default vs named graph semantics.
- Restore path yields a consistent queryable dataset and shares the same atomic publish gate as the other write paths.
- Backup artifacts and restore reports expose revision and checksum metadata.
- Revision publication and rollback paths are test-covered.
