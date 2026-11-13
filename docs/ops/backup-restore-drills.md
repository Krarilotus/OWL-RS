# Backup and restore drills

A drill proves that a backup restores. This page gives one complete drill, small enough to
run against the [quick start](../../README.md#quick-start)'s server, then the scenarios to
repeat on a schedule. The endpoints: [http-api.md](http-api.md#engine-api-apiv1). Backups
of large stores, offline: [server setup §10](server-setup.md#10-backup-and-recovery-setup).
The storage design behind them: [ADR-0002](../adr/0002-engine-storage-lsm-permutations.md).

## A complete drill

The quick start's server answers on port 8080. The drill restores its backup into a second,
empty server on port 8081 and compares the two. The commands are for Linux (GNU `head`).

```bash
# 1. Back up: an N-Quads export of every asserted statement, closed by its checksum.
curl -sS -D backup-headers.txt -o export.nq http://localhost:8080/api/v1/repositories/nrese/backup
grep -i '^x-nrese' backup-headers.txt   # its source revision and quad count
tail -n 1 export.nq                     # "# nrese-sha256 <hex>"
head -n -1 export.nq | sha256sum        # the same <hex>

# 2. Restore it into a fresh server.
docker run -d --name nrese-drill -p 127.0.0.1:8081:8080 -v nrese-drill:/var/lib/nrese/data \
  -e NRESE_REASONING_MODE=owl2-rl nrese
curl -sSf --max-time 5 --retry 60 --retry-delay 1 --retry-max-time 60 --retry-all-errors http://localhost:8081/readyz
curl -sS -X POST -H 'Content-Type: application/n-quads' --data-binary @export.nq \
  http://localhost:8081/api/v1/repositories/nrese/restore

# 3. Count the asserted statements of every graph on both.
q='query=SELECT (COUNT(*) AS ?n) WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }'
curl -sS --data-urlencode infer=false --data-urlencode "$q" http://localhost:8080/dataset/query
curl -sS --data-urlencode infer=false --data-urlencode "$q" http://localhost:8081/dataset/query

# 4. Remove the drill's server.
docker rm -f nrese-drill && docker volume rm nrese-drill
```

The drill passes when:

- the two checksums of step 1 are the same;
- the restore answers `200` with `"checksum_verified":true` and a `quad_count` equal to
  the backup's `x-nrese-quad-count`;
- both counts of step 3 are that number too.

A restore whose checksum doesn't match answers `400` and changes nothing. Over HTTP a
restore takes at most about 384 MiB by default (3 × `budgets.upload_size` + 64 KiB, held
whole while it is parsed). Larger exports restore offline with
`nrese-server load --replace export.nq`, which doesn't check the checksum: check it first
as in step 1.

## Scenarios to repeat

| Scenario | Steps | Passes when |
|---|---|---|
| Standard restore | The drill above, from the latest backup, into a clean data directory. For a store-sized backup, restore offline: an image with `nrese-server restore DIR` (checked against its `manifest.json`), an export with `nrese-server load --replace` | Step 3's counts match the backup's quad count, and your own check queries return their recorded answers |
| Interrupted write | Stop the server hard (`docker kill nrese`) while updates run, then start it on the same data directory | It becomes ready; every acknowledged commit is there. Opening replays the log's intact records and cuts a torn last one; other damage stops the start unless `store.wal_damage` says otherwise ([durability](../spec/02-storage-and-transactions.md#durability-on-disk-mode)) |
| Rollback | Restore the backup taken before a change, then run the queries your clients depend on | They return the answers recorded before the change |

What to keep from each run: the backup's source revision, quad count and checksum; the
restore's answer; the start and end times; the counts and query answers compared.
