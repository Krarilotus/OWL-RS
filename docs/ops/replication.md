# Read replicas

One server takes the writes and ships its write-ahead log. Other servers apply it and answer
queries (hardware plan H6, step 1). This gives more query throughput and availability. The
data must still fit one machine; partitioning across machines is a later step.

## How it works

- **The primary** (`replication.mode = "primary"`) serves, to administrators:
  - `GET /api/v1/replication/log?after=N` (the committed records after revision `N`, as they
    stand in its log);
  - `GET /api/v1/replication/image` (an image of its latest revision, streamed, with its
    manifest).
- **A replica** (`replication.mode = "replica"`) starts from the primary's image when its
  data directory holds no store. It then follows the log: it asks for the records after its
  revision, applies them, and asks again at once while records come, and after
  `replication.poll` once it has caught up.
  - Each record becomes a commit with the primary's revision and term ids, in the replica's
    own log too, so a replica restarts where it stopped.
  - A replica takes no writes (its write surfaces are off, as in the read-only posture) and
    doesn't reason: the records carry the inferences.
- **Both** need an on-disk store.

## Configuration

```toml
# The primary
[replication]
mode = "primary"

[storage]
wal_archive = true          # keep the log for replicas that fall behind (see below)

# A replica
[replication]
mode = "replica"
primary = "https://primary.example:8080"
token = "..."               # an administrator's bearer token on the primary
poll = "500ms"
batch_bytes = "8MiB"
```

## Falling behind

The primary's log holds the records since its last checkpoint, unless `storage.wal_archive`
keeps them. Bulk loads and rematerialisations write a checkpoint, not records.

A replica that asks for records the log no longer holds gets 410 Gone. It keeps serving
what it has and reports the error in its status. To catch up, it starts again from an
empty data directory: it takes a fresh image. With `storage.wal_archive = true` on the
primary, only bulk loads and rematerialisations send replicas back to an image.
`nrese-server prune-archive REVISION` removes archived segments that every replica has.

## Watching

- `GET /api/v1/replication/status`: the mode, this server's revision, and on a replica the
  primary's revision when last asked, the lag in revisions, the records applied, the last
  contact and the last error.
- `/metrics` on a replica: `nrese_replication_lag_revisions`,
  `nrese_replication_records_applied_total`, `nrese_replication_healthy`.

## Limits of this first step

- **Failover:** a replica isn't promoted automatically. To promote one, stop it, set
  `replication.mode = "primary"` (or `"off"`), and point the writers and the other replicas
  at it.
- **Support graph sets:** for `inferred = "supported"`, a replica computes them afresh after
  the records it applies, not incrementally.
- **Re-bootstrap:** a replica doesn't yet take a new image by itself when the log no longer
  serves it.
