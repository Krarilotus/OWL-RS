# Read replicas

One server takes the writes and ships its write-ahead log. Other servers apply it and answer
queries. This gives more query throughput and availability. The
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
  - Every exchange with the primary has
    [deadlines](config-reference.md#replication). A log answer must say how many records it carries
    and up to which revision; one that doesn't, or whose records don't reach that revision,
    is an error. Errors are retried with a growing pause and shown in the status.
- **Both** need an on-disk store.

## Configuration

```toml
# The primary
[replication]
mode = "primary"

[store]
wal_archive = true          # keep the log for replicas that fall behind (see below)

# A replica
[replication]
mode = "replica"
primary = "https://primary.example:8080"
token = "..."               # an administrator's bearer token on the primary
poll = "500ms"
batch_bytes = "8MiB"
```

`primary` is the primary's own base URL: a replica follows no redirect from it, so its token goes only to that
address (a redirect is reported as the primary's error).

## Falling behind

The primary's log holds the records since its last checkpoint, unless `store.wal_archive`
keeps them. Bulk loads and rematerialisations write a checkpoint, not records.

A replica that asks for records the log no longer holds gets 410 Gone. It keeps serving
what it has and reports the error in its status. To catch up, it starts again from an
empty data directory: it takes a fresh image. With `store.wal_archive = true` on the
primary, only bulk loads and rematerialisations send replicas back to an image.
`nrese-server prune-archive REVISION` removes archived segments that every replica has.

## Watching

- `GET /api/v1/replication/status`: the mode, this server's revision, and on a replica the
  primary's revision when last asked, the lag in revisions, the records applied, the last
  contact and the last error.
- `/metrics` on a replica: `nrese_replication_lag_revisions`,
  `nrese_replication_records_applied_total`, `nrese_replication_healthy`.

## Limits of this first step

- A replica serves its primary's statements without its primary's reasoning certificate:
  its reasoned reads say `not-certified`, under `dl.answers = "exact"` they are refused,
  and its consistency reads `unknown`. This holds from its start on, also after a rebuild of
  its inferences or a change of its settings.
- A primary under `reasoner.equality = "compact"`, with a ruleset that reasons with
  `owl:sameAs`, serves no replica in this release. It answers the log and the image with
  `409` and a sentence that says so, also where the log no longer holds the records. A
  replica shows the refusal as `last_error` in its status and asks again, about once a
  minute; one with an empty data directory doesn't start. A primary for replicas keeps the
  default, `representatives`.
- A replica started over a data directory restored from such a primary's backup takes no
  image: its reasoned reads say `not-certified` and may miss rows, and the primary's refusal
  shows only in its status. Don't start a replica over such a backup.
- On a replica, a reader of only some graphs who sees inferred statements
  (`inferred = "visible"` or `"supported"`) is refused every read with `503` and a sentence
  that says the replica doesn't serve it, also a read of the stored statements alone, though
  that one could be answered. Such a reader reads from the primary.
- Only the default repository is replicated. Other repositories on a replica are served
  read-only as its data directory holds them, reasoned at its start, and their reasoned
  reads say `not-certified`.
- A replica isn't promoted automatically. To promote one, stop it, set
  `replication.mode = "primary"` (or `"off"`), and point the writers and the other replicas
  at it.
- For `inferred = "supported"`, a replica computes support graph sets afresh after
  the records it applies, not incrementally.
- A replica doesn't yet take a new image by itself when the log no longer
  serves it.
- A replica computes no inferences, so it keeps a closure an older build wrote: after an
  upgrade that changes the reasoning semantics (`reasoning_semantics` on `/version`), start
  it again from an empty data directory.
