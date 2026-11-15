# NRESE server setup and deployment guide

## 1. Purpose

This page says how to deploy and run `nrese-server` in production. It covers deployment as a
native binary or in a container, a reverse proxy with TLS and the checks before the server goes
into use.

## 1.1 Local test-server profile

For local checks we run `nrese-server` as a test server with the ontology preloaded. The test
profile checks the start-up and readiness behaviour, how the ontology load succeeds or fails,
and that the basic query path works before the server is hardened for production.

## 2. Deployment targets

- Linux `x86_64` and `aarch64` are the main production targets.
- The server runs as a native binary under `systemd`, or as an OCI container under an
  orchestrator or a standalone runtime.

## 3. Runtime topology

- One `nrese-server` instance manages one data directory, with the default repository in it and each repository created over the API in its `repositories/<id>/` ([HTTP interface](http-api.md#rdf4j-protocol)).
- To scale out, read replicas follow a primary's log ([replication](replication.md)), and nothing partitions the data across machines.
- Put a reverse proxy in front of NRESE for TLS termination and controls at the edge.

### 3.1 Operator UI endpoint

- `GET /ops` serves a small operator console from the same server process.
- `GET /ui` stays as an alias, for compatibility.
- `GET /console` serves the user's console from the same server process.
- `GET /` redirects to `/console`.
- `/console` calls endpoints of the same origin, such as `/dataset/query`, `/dataset/tell`, `/dataset/update`, `/dataset/data`, `/api/v1/ai/status` and `/api/v1/ai/query-suggestions`.
- `/ops` stays the operator's page for diagnostics and operational work.

## 4. Filesystem layout

This is the layout we recommend on the host.
- `/opt/nrese/bin/nrese-server`
- `/etc/nrese/nrese.env`
- `/etc/nrese/config.toml`
- `/var/lib/nrese/data`
- `/var/lib/nrese/backups`
- `/var/log/nrese/`

The service user must own the data and log directories, with the least privilege.

## 5. Configuration surface

Every setting is in [config-reference.md](./config-reference.md), and
[precedence](./config-reference.md#precedence) says which value wins when a setting is given in
several places (command line, environment, file, default).

This is how configuration behaves.

- `config.toml` is a full runtime input.
- Environment variables override file values.
- `--set KEY=VALUE` overrides one file key, over the file and the environment.
- Typed runtime defaults and validation stay in the owning crates.
- External parsing, file loading and precedence stay in `crates/nrese-server/src/config/`.
- `server.deployment_posture` / `NRESE_DEPLOYMENT_POSTURE` selects the deployment mode explicitly, one of `open-workbench`, `read-only-demo`, `internal-authenticated` and `replacement-grade`.
- Start-up validation refuses invalid `internal-authenticated` / `replacement-grade` combinations instead of serving them silently.

This is how durable storage behaves.

- `NRESE_STORE_MODE=on-disk` stores a write-ahead log and checkpoints under `NRESE_DATA_DIR`.
  It needs no build feature and no native toolchain, and every commit is synced before it is
  acknowledged.
- The data directory is locked, so only one process at a time can use it. A second server or offline command on the same directory fails at start-up ([what to do](#err-data-directory-locked)).
- Checkpoints run in the background and delete the WAL segments they cover.
- There are three ways to back up and restore, and you can also copy the directory.
  - For an image backup (physical), `POST /api/v1/repositories/{id}/image` (for administrators, and the earlier `POST /ops/api/admin/dataset/image`, with `?repository=ID` for another repository than the default, still answers, into `backups/<ID>-<seconds>/`) writes an image of the current snapshot, the store's checkpoint format with the inferred statements, into `backups/<seconds since 1970>/` of the data directory, with `manifest.json` (revision, counts, size, SHA-256), while writes go on. Offline, `nrese-server backup DIR` does the same. To restore, stop the server and run `nrese-server restore DIR` with the configuration of the target. It checks the image against its manifest and places it in the data directory, which must hold no store, and the server then opens at the backup's revision. The image needs an NRESE that reads its checkpoint format.
  - The N-Quads export, `GET /api/v1/repositories/{id}/backup`, and its `POST …/restore` (earlier `/ops/api/admin/dataset/backup` and `/restore`) carry the asserted statements only. The export is portable to other stores, and the inferences are derived again after a restore.
  - For a point-in-time restore, with `store.wal_archive` on, checkpoints move the WAL segments they cover into `wal-archive/` instead of deleting them. `nrese-server restore DIR --wal DATA/wal-archive --wal DATA/wal --until-revision N` restores the image and replays the log up to revision `N` (every commit is one revision: `/api/v1/repositories/{id}/info` gives it to whole-dataset readers or administrators, and the logs record it), or with `--until-time 2026-10-02T14:05:00Z` (RFC 3339 with a zone, read by the `xsd:dateTime` calendar: no leap second `:60`, offsets within ±14:00, `24:00:00` the next day's midnight, digits past the microsecond cut off) the commits made up to that time (logged since WAL version 6), cutting it there, and without `--until-revision` as far as the log goes. A restore by time refuses an image whose last commit was made after the time (take an older image), or one with no commit time, so restore that one by revision instead. These images have no commit time: those written before 7 Nov 2026 (checkpoint format 11), and every image, a bulk replacement's included, of a store that was upgraded from such a checkpoint, of a store created before 2 Oct 2026 (it keeps integer literals in its dictionary, and its log keeps no times), or of a replica of one of these. To get restore by time for such a store, export it as N-Quads and `nrese-server load` the export into a new, empty data directory, and that store's images have commit times. After a refusal the target directory holds the placed image, so clear it before trying again. The target data directory must hold no store. `nrese-server prune-archive R` removes the archived segments whose records all come before revision `R` (after an image backup at `R - 1` or later they are not needed), and it is safe while the server runs.
  - Or copy the directory while the server is stopped.

To make the server reachable from other machines:

- Keep the default bind address (`127.0.0.1`) for local work and development.
- For a deployment reachable from outside, bind `NRESE_BIND_ADDR=0.0.0.0:8080` and put a TLS reverse proxy with access controls in front.
- In `mtls` mode, NRESE trusts a client certificate's identity only through the documented header contract of a trusted reverse proxy. It doesn't terminate client TLS certificates itself.

### 5.2 Reliability and storage variables

No setting schedules backups, which are taken through the admin API or offline with `nrese-server backup` (above, and [backup-restore-drills.md](backup-restore-drills.md)). Earlier versions of this page listed `NRESE_SNAPSHOT_RETENTION`, `NRESE_BACKUP_*` and `NRESE_RECOVERY_MODE`, but none of them was ever implemented. The checkpoint and log settings (`store.checkpoint_after_wal`, `store.wal_archive`, `store.wal_damage`, `store.verify_on_open`, `store.map_checkpoints`) are in [config-reference.md](config-reference.md), the only place settings are listed.

### 5.3 Ontology preload

- Set `NRESE_ONTOLOGY_PATH` (or `store.ontology_path`) to preload an ontology at startup.
- If it is set and the file is missing, startup fails with a clear error.
- If it is unset, nothing is preloaded. There are no implicit discovery or fallback paths, so the server behaves the same regardless of the working directory.

### 5.4 Bulk loading

Initial loads and full restores of large files use the offline bulk loader instead of HTTP.

```powershell
$env:NRESE_STORE_MODE = "on-disk"; $env:NRESE_DATA_DIR = ".\data"
nrese-server load [--config .\config.toml] [--replace] [--graph <IRI>] [--strict | --quarantine] data.nt more.nq ...
```

This is how a load behaves.

- The format comes from each file's extension.
- `--replace` swaps the dataset instead of adding to it.
- `--graph` sets the target graph for triple formats, and quad formats keep their own graphs.
- Blank nodes are fresh per load.
- A statement with a syntax error goes into the repository's quarantine and the load goes on (`store.invalid_statements = "quarantine"`, the default, `--quarantine` or the older `--skip-errors`). Such a statement is a line of N-Triples or N-Quads, a Turtle or TriG statement up to its `.`, an RDF/XML node element or a JSON-LD top-level object. It is kept with its file, line and reason, and its text verbatim except for RDF/XML and JSON-LD, invisible to queries, reasoning, exports and statistics until it is re-admitted (fixed, or repaired on request, through the commit gates) or discarded ([quarantine.md](../design/quarantine.md)). The load prints the count and the first examples. With `--strict` (or `store.invalid_statements = "strict"`) the first stops the load and nothing of it is committed. Malformed XML or JSON and read errors always stop.
- Bulk loading is offline only. The server must not be running on the same data directory, and the directory lock enforces this. Validation gates don't run during a bulk load.
- N-Triples and N-Quads are parsed on all cores and are the fastest input, about 2.7 M triples/s on the reference machine, with 100 M triples in 37 s (`benches/baselines/README.md`). Convert other formats to N-Triples for the largest loads (`nrese-server convert`, below).

### 5.4.1 One-off queries and conversions

```powershell
nrese-server query [--config .\config.toml] [--format json|xml|csv|tsv|nt|ttl|nq|trig|rdf|jsonld] "SELECT ..."
nrese-server query [--config .\config.toml] --file query.rq
nrese-server convert data.ttl data.nt
nrese-server print-query [--config .\config.toml] [--form paths|values] [--schema ontology.ttl]... "SELECT ..."
```

- `query` answers one query from the configured store and writes the results to standard output, SELECT and ASK in the results format (default JSON) and CONSTRUCT and DESCRIBE in the RDF format (default N-Triples). First it brings the inferred stack up to date, as the server's start does, and it rematerialises if the rules changed since the last load or start. It runs offline, like `load`.
- `print-query` prints the query as standard SPARQL 1.1 for a store without reasoning that holds the same statements, and that query answers as NRESE does under `owl2-rl` with the QL rewriting on (`docs/design/ql-rewriting.md` §8). `--form paths` (the default) reaches the hierarchy with property paths over the schema, and `values` enumerates it. It works against the configured store, or with `--schema` against those files alone. The first line, `# NRESE completeness: …`, is NRESE's status for the answers. A query that can't be written exactly (equality, recursive rules, …) gets its reasons on standard error and exit status 2.
- `convert` reads one RDF file and writes it in another format, both taken from the extensions, statement by statement and without a store, so the file can be larger than memory. Blank node labels are kept, and a named graph is an error for a format without graphs. The output appears only when complete.

### 5.4.2 Moving data in from another store

To move from Fuseki, GraphDB, RDF4J or any other SPARQL store, export the dataset as N-Quads
(named graphs kept), load it with `nrese-server load` and check these before you switch the
clients over.

- Compare the statement count per graph on both sides
  (`SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g`, and the default graph).
- Run the application's own queries and updates on both sides and compare the results.
- With reasoning on, check that the source store's ruleset matches the configured mode
  ([reasoning semantics](../spec/reasoning-semantics.md)), because a different ruleset changes
  the answers, not only their speed.

Keep the old store until the comparison passes, and switching back is changing the client's
endpoint.

### Commands

The form is `nrese-server [COMMAND] [OPTIONS]`. Every command takes `--config FILE` (also `-c FILE`, `--config=FILE`) and `--set KEY=VALUE` (repeatable, see [precedence](config-reference.md#precedence)), and `convert`, `config-schema` and `print-query --schema` read no configuration. An unknown option is an error. Commands log to standard error, and the server logs to standard output.

The exit status is `0` on success, `1` on any error, with `Error: <message>` and its causes on standard error, and `2` from `print-query` when the query can't be written exactly. A binary built for a CPU feature this machine lacks exits `1` before anything runs, naming the feature.

Commands marked *store* open the configured store and lock its data directory, so they fail while another process has it open ([locked](#err-data-directory-locked)).

| Command | Arguments and options (defaults) | What it does and changes | Fails when |
|---|---|---|---|
| (none) | | *store*. Serves HTTP on `server.bind_address`: opens the store, brings the inferred stack up to date, then serves until SIGTERM or SIGINT ([stopping](#64-stopping), exit `0`) | the configuration is invalid; the directory is locked; the address can't be bound ([`failed to bind ADDR`](#err-failed-to-bind)); startup reasoning fails |
| `load` | `FILE...` (at least one; format by extension); `--replace` (default: add); `--graph IRI` (default: the default graph; triple formats only); `--strict` or `--quarantine` (older `--skip-errors`; default `store.invalid_statements`) | *store*. Commits the files' statements, then rematerialises the inferences and prepares the planner's statistics ([bulk loading](#54-bulk-loading)) | no file; a file can't be read or parsed (under `--strict`: any bad statement); the loaded data is inconsistent under the ruleset: exit `1` with the violation count, the data stays loaded and the server starts in quarantine |
| `query` | `QUERY` or `--file FILE` (one of them); `--format F` (`json`, `xml`, `csv`, `tsv`, or for CONSTRUCT and DESCRIBE `nt`, `ttl`, `nq`, `trig`, `rdf`, `jsonld`; default JSON, N-Triples) | *store*. Brings the inferred stack up to date (it may rematerialise and write it), then writes the answers to standard output, reading every graph ([one-off queries](#541-one-off-queries-and-conversions)) | both or neither of `QUERY` and `--file`; an unknown format; `parsing the query failed`; `the query failed` |
| `print-query` | `QUERY` or `--file FILE`; `--form paths\|values` (default `paths`); `--schema FILE` (repeatable; default: the configured store) | *store* without `--schema`. Prints the query as standard SPARQL 1.1 after a `# NRESE completeness:` line; changes nothing | exit `2`: not expressible, the reasons on standard error; exit `1`: other errors |
| `convert` | `INPUT OUTPUT` (formats by extension) | Writes `OUTPUT` from `INPUT`, statement by statement, without a store; the output appears only when complete | not exactly two files; an unknown extension; a named graph in a format without graphs |
| `check-config` | | Loads and validates the configuration, prints the main effective settings, a `cpu:` line and `configuration is valid` ([what it prints](config-reference.md#validation)); changes nothing | the configuration is invalid: exit `1` with the setting and the reason |
| `config-schema` | | Prints the JSON Schema of the configuration file; changes nothing | |
| `backup` | `DIR` | *store*. Writes an image backup of the store into `DIR` (for a running server: `POST /api/v1/repositories/{id}/image`) | no `DIR`; the directory is locked; writing fails |
| `restore` | `DIR`; `--wal LOGDIR` (repeatable; default: no replay); `--until-revision N`, `--until-time T` (RFC 3339; default: as far as the log goes) | Places the image in `DIR` into the configured data directory, then replays the log ([point-in-time restore](#5-configuration-surface)) | no `DIR`; the data directory holds a store; the image fails its manifest check; `--until-time` before the image's last commit, or an image without commit times |
| `prune-archive` | `REVISION` (required) | Removes the segments in `wal-archive/` of the data directory whose records all come before `REVISION`; safe while the server runs | no or a non-numeric revision |

### Startup errors

A server that fails to start writes `Error: <message>` and its causes to standard error (the journal, `docker logs`) and exits `1`. Configuration errors name the setting, and to see them without starting, run `nrese-server check-config` with the same configuration.

<a id="err-data-directory-locked"></a>
#### Data directory locked

```text
Error: storage engine error: data directory ./data is locked by another process

Caused by:
    data directory ./data is locked by another process
```

- The cause is that another process has the store in `store.data_dir` (the path shown) open, a running server or an offline command (`load`, `query`, `print-query`, `backup`) on the same directory. The lock is that process's hold on the directory's `LOCK` file, released when it closes the store.
- To fix it, stop the other process (`systemctl stop nrese`, `docker stop nrese`), wait for the offline command to end or give this one another data directory. On Linux, `fuser DATA_DIR/LOCK` names the process. Don't delete `LOCK`, because the file isn't the lock, the other process's hold on it is.
- To check the fix, start the server again. Once it reports ready, `curl -fsS http://127.0.0.1:8080/readyz` answers `{"status":"ready"}`.

<a id="err-failed-to-bind"></a>
#### Address can't be bound

```text
Error: failed to bind 0.0.0.0:8080

Caused by:
    Address already in use (os error 98)
```

- The cause line says which cause it is. `Address already in use` means another process listens on that port, `Cannot assign requested address` means the address isn't one of this machine's, and `Permission denied` means a port below 1024 without the right to bind it.
- To fix it, stop the other listener (on Linux, `ss -ltnp 'sport = :8080'` names it), or set another address or port in `server.bind_address` (`NRESE_BIND_ADDR`), for example `127.0.0.1:8081`. In Docker the server binds inside the container, and a busy port on the machine shows as `docker run`'s own error, `port is already allocated`. Then publish another one (`-p 127.0.0.1:8081:8080`).
- To check the fix, start the server again, and `curl -fsS http://127.0.0.1:8080/readyz` (with the port you chose) answers `{"status":"ready"}`.

## 5.5 Local test-server startup example (PowerShell)

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:8080"
$env:NRESE_DEPLOYMENT_POSTURE = "open-workbench"
$env:NRESE_DATA_DIR = ".\data"
$env:RUST_LOG = "info"
$env:NRESE_ONTOLOGY_PATH = ".\ontology.ttl"
cargo run -p nrese-server
```

To start with an explicit config file:

```powershell
cargo run -p nrese-server -- --config .\config.toml
```

Build the frontend before serving `/console`.

```powershell
Set-Location .\apps\nrese-console
npm install
npm run build
Set-Location ..\..
```

## 6. Native binary deployment (`systemd`)

### 6.1 Create a service user
Create a dedicated non-login user `nrese`, and give it ownership of `/var/lib/nrese` and `/var/log/nrese`.

### 6.2 Example unit file
Create `/etc/systemd/system/nrese.service`.

```ini
[Unit]
Description=NRESE Semantic Engine
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nrese
Group=nrese
WorkingDirectory=/opt/nrese
EnvironmentFile=/etc/nrese/nrese.env
ExecStart=/opt/nrese/bin/nrese-server --config /etc/nrese/config.toml
Restart=on-failure
RestartSec=3
LimitNOFILE=65536
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/nrese /var/log/nrese
AmbientCapabilities=
CapabilityBoundingSet=

[Install]
WantedBy=multi-user.target
```

### 6.3 Start and verify
These commands start the service and check it.

- `systemctl daemon-reload`
- `systemctl enable --now nrese.service`
- `systemctl status nrese.service`
- `journalctl -u nrese.service -f`
- `curl -fsS http://127.0.0.1:8080/healthz`
- `curl -fsSI http://127.0.0.1:8080/ops`
- `curl -fsSI http://127.0.0.1:8080/console`

### 6.4 Stopping
A stop signal, SIGTERM (`systemctl stop`, `docker stop`, Kubernetes) or SIGINT (ctrl-c, and on Windows ctrl-c in the server's console), ends the server cleanly, in this order.

1. It accepts no new connections, closes idle ones and lets the requests in flight finish, for up to 5 s. A request still running then is cut, and its client sees the connection drop and doesn't know whether a commit it sent was made, as on any kill.
2. A replica's follower stops, and the store closes. It waits for the maintenance in progress first, the longest of which is a background checkpoint (it rewrites the whole store).
3. It logs `nrese-server stopped` (the signal, how long the stop took, whether requests were cut) and exits with code 0.

One deadline bounds the stop, 8 s from the signal. A stop not done by then logs `nrese-server stop deadline reached`, naming what it still waited for, and 200 ms later ends the process with code 0, as a kill would. The tests check that a stopped server has ended within 9 s of the signal, so give the supervisor a stop allowance above that before it kills (`docker stop` waits 10 s unless `-t` says otherwise). Nothing acknowledged is lost either way, because every commit is synced to the log before it is acknowledged, and the next start recovers from the checkpoint and the log, as after a kill ([durability](../spec/02-storage-and-transactions.md#durability-on-disk-mode)).

Two cases aren't handled. A signal during start-up (while the store opens and start-up reasoning runs) acts as a kill (in the image, Docker kills at the end of its grace period), and on Windows a service stop or `taskkill /F` ends the process without the drain.

## 7. Container deployment

### 7.1 Image requirements
An image should have a minimal base image, a non-root runtime user, a read-only root filesystem where possible and a writable volume mounted only for `/var/lib/nrese`.

### 7.2 Example `docker run`
```bash
docker run -d --name nrese \
  --read-only \
  --tmpfs /tmp \
  -p 127.0.0.1:8080:8080 \
  -v /var/lib/nrese/data:/var/lib/nrese/data \
  -v /etc/nrese:/etc/nrese:ro \
  -e NRESE_BIND_ADDR=0.0.0.0:8080 \
  -e NRESE_DATA_DIR=/var/lib/nrese/data \
  -e NRESE_AUTH_MODE=bearer-jwt \
  nrese
```

`nrese` is the image built from this repository (`docker build -t nrese .`, as in the [README](../../README.md#quick-start)). The image sets `NRESE_BIND_ADDR=0.0.0.0:8080`, `NRESE_STORE_MODE=on-disk` and `NRESE_DATA_DIR=/var/lib/nrese/data`, and keeps the server's defaults for everything else ([configuration](config-reference.md)), and the example names two of them only to show them. The image's entrypoint is `nrese-server`, so a [command](#commands) runs as `docker run --rm nrese config-schema`, and from source as `cargo run --release -p nrese-server -- COMMAND`.

### 7.3 Kubernetes notes
On Kubernetes, use `/readyz` as the readiness probe and `/healthz` as the liveness probe, a `PodDisruptionBudget` for availability and a persistent volume for the data. Keep secrets in the cluster's secret manager, and restrict access to trusted namespaces and ingress controllers with a `NetworkPolicy`.

## 8. Reverse proxy and TLS

### 8.1 Recommended edge pattern
TLS terminates at the reverse proxy, and NRESE binds to localhost or a private network only. The proxy forwards `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Request-Id`. Expose `/console`, `/ops` and the API routes through the same trusted host, and apply the same auth and rate-limit policy to them unless you split user and operator access on purpose.

### 8.2 Nginx example
```nginx
server {
    listen 443 ssl http2;
    server_name semantic.example.com;

    ssl_certificate     /etc/letsencrypt/live/semantic.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/semantic.example.com/privkey.pem;
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;

    client_max_body_size 16m;
    proxy_connect_timeout 3s;
    proxy_read_timeout 120s;
    proxy_send_timeout 120s;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Request-Id $request_id;
    }
}
```

## 9. Auth and secret management
Never put credentials or key material into images. Give JWT signing secrets, keys and OIDC secrets to the server through file mounts or a secret store, rotate keys and certificates on a schedule and enforce the checks of each token's issuer, audience and expiry.

## 10. Backup and recovery setup
Schedule backups yourself, since no setting does ([5.2](#52-reliability-and-storage-variables)), and keep copies on local or remote storage. Check the backups with restore drills from time to time.

To recover from an image backup (`backups/<seconds>/` of the data directory, or a copy of it elsewhere), follow these steps.

1. Stop writer traffic.
2. Restore the newest image into an empty staging directory, with `NRESE_DATA_DIR=/srv/nrese-staging nrese-server restore /srv/nrese-data/backups/<seconds>` (and `--wal` to replay the log after it, see [point-in-time restore](#5-configuration-surface)). Exit `0` means the image passed its manifest check and is in place.
3. Start a server on it with writes disabled, on another port, with `NRESE_DATA_DIR=/srv/nrese-staging NRESE_DEPLOYMENT_POSTURE=read-only-demo NRESE_BIND_ADDR=127.0.0.1:8081 nrese-server`.
4. Check that `/readyz` answers `200`, that `GET /api/v1/repositories/{id}/info` (as a whole-dataset reader or administrator) gives the `revision` and `quad_count` you expect and that a few known queries answer as before. For a full comparison of two servers, run the [drill](backup-restore-drills.md#a-complete-drill)'s count step against both.
5. To promote it, stop the staging server, start the production server with `NRESE_DATA_DIR` pointing at the restored directory, check `/readyz` again and then switch traffic.

## 11. Production Readiness Checklist
- W3C protocol endpoints reachable and authenticated as expected.
- Operator UI endpoint (`/ops`) reachable and usable from approved external networks.
- User console endpoint (`/console`) reachable and usable from approved external networks.
- TLS policy validated by security scanner.
- AuthN/AuthZ tested for reader/writer/admin roles.
- Metrics and logs collected centrally.
- Ontology preload path validated in pre-prod (there is no fallback: [5.3](#53-ontology-preload)).
- Alerts configured for:
- High 5xx rate.
- Update queue saturation.
- Query timeout spikes.
- Disk usage threshold on data volume.
- Backup job success and restore drill executed.
- Load test confirms SLO under mixed read/write profile.

## 12. Operational SLO Baseline
- Availability target: `>=99.9%` monthly.
- P95 query latency target for representative `SELECT`: `<300 ms` under baseline load.
- P99 update commit latency target: `<2 s` for moderate update transactions.
- Error budget policy SHOULD define rollback gates for releases.

### 12.1 Memory over time: what grows, and where it stops

A server under steady load grows in memory for a while and then levels off. What the
program holds stops growing once its working set is in place; what can keep growing
grows with the data by design. Each part has a gauge in `/metrics`:

| What | Grows with | Stops at | Gauge |
|---|---|---|---|
| The data and its indexes | the statements | the data | `nrese_index_bytes`, `nrese_store_quads` |
| The dictionary | every distinct term ever written (append-only: a term deleted from the data keeps its entry) | the distinct terms; a store that keeps receiving new terms grows without bound, by design | `nrese_dictionary_terms`, `nrese_dictionary_bytes` |
| The full-text index | the dictionary's literals, from the first text search on | as the dictionary | `nrese_text_index_terms` |
| The result cache | repeated queries on an unchanged store (a commit makes it start over) | `budgets.result_cache` | `nrese_query_cache_bytes` |
| The write-ahead log (on disk, not in memory) | writes | `store.checkpoint_after_wal`, then a checkpoint | `nrese_wal_bytes_since_checkpoint` |
| Sessions, running queries | open client transactions, queries | their end (an idle session after ten minutes) | `nrese_sessions_open`, `nrese_queries_running` |
| The allocator's memory | the most the program held at once, spread over its threads | what it reserved: it stops growing within minutes | `nrese_allocator_bytes{kind="committed"}` |

The last line is most of a serving process's memory. In a soak of 16 clients (inserts,
deletes, queries, transactions, the Graph Store; 7 October 2026) the program held about
2 MiB once warm, and never more than 15 MiB at once, while the process held 250 to 330
MiB: mimalloc backs its memory with transparent huge pages on Linux, where one touched
byte makes 2 MiB resident and a page in use keeps the rest of its 2 MiB. That level moves
within what mimalloc reserved (about 600 MiB there, reached in the first minutes) and does
not leak: the program's own memory stays level (a test checks it,
`crates/nrese-store/tests/memory_churn.rs`). With `budgets.huge_pages = "off"` the same
soak held 95 to 120 MiB (the system allocator: 75 to 85 MiB), and materialising LUBM 100
took 26 % longer. Choose `off` where serving memory matters more than reasoning speed.
`budgets.huge_pages = "bulk"` takes both: huge pages back the process only while a bulk load
or a materialisation runs (a load, a rematerialisation at start or after a change of rules, a
commit's fallback to one), and are off otherwise. On LUBM 100's materialised closure (22 M
statements; 11 November 2026, a builder host) the allocator held about 13 MiB per pool thread
after the query mix with huge pages and about 6 MiB without: at 32 threads the process's peak
was 917 MiB with them and 636 MiB without, the queries' times within their spread.

`benches/probes/soak.py` reads every gauge above beside resident memory and prints each
one's slope; on Linux it also splits resident memory by mapping (the allocator, stacks,
the store's mapped files, the binary).

## 13. Upgrade strategy

### 13.1 Upgrading a store (from `main` to v2)

A data directory written by an earlier NRESE opens as it is, with no migration step. Checkpoints of formats 4 to 11 are read, the write-ahead log is replayed (also after a crash or a kill), and the repositories, their settings and rules, the namespaces, the access state (users, local logins, roles, workspaces, saved queries) and the image backups carry over. Sessions and the result cache live in memory and start empty, as at every start.

1. Stop the server, and keep a copy of the data directory (or an image backup) and the old binary.
2. Install the new binary and start it with the same configuration. The first start brings each inferred stack in line with the new rules. A stack whose rules didn't change (the reasoning marker's fingerprint, `reasoning.state`) is kept, and one whose rules did is rebuilt before the server reports ready, which takes about as long as reasoning after a bulk load. From `main` (d9a4b24) to v2, `owl2-ql` and `owl-horst` stacks are rebuilt, and `rdfs`, `owl2-rl` and repositories with their own rules are kept.
3. Check `/readyz` for readiness, then read `revision` and `quad_count` from `/api/v1/repositories/{id}/info` as a whole-dataset reader or administrator, and compare a few known queries against the old server's answers.

These change on purpose.

- Queries under `owl2-ql` get answers through existentials. The QL rewriting is on by default there (`reasoner.ql_rewriting = "auto"`), so queries can find individuals that only an existential axiom makes match (with `Employee ⊑ ∃worksFor`, an employee without a stated employer now works for something). `reasoner.ql_rewriting = "off"`, or a repository's own `ql_rewriting`, keeps the earlier answers. Under `owl2-rl` nothing changes.
- The N-Quads export's checksum moves from a leading header to the end, because the export streams (constant memory, at any store size), so its SHA-256 is known only after the body. It closes the body as the comment `# nrese-sha256 <hex>` (over every byte before it, and N-Quads readers skip it) and follows it as the trailer `x-nrese-checksum-sha256`, announced in `Trailer` (HTTP/1.1 clients ask for trailers with `TE: trailers`). A restore checks the comment where present and refuses a mismatch (400). Graph Store `GET` and the export stream too, and their timeout (`budgets.graph_read_timeout`) bounds the first chunk and each stall, not the whole transfer.
- New repositories can't have an id starting or ending with `.` or a Windows device name (`CON`, `NUL`, `COM1`, ...). Existing ones keep working and are logged at start, and to move the data directory between systems, copy one into a repository with another id.

A data directory is open in one process at a time, and a second process opening a data directory one already has open is refused (`Locked`), whichever version each runs, because earlier versions and this one lock the directory's `LOCK` file the same way. The lock is the open file's (`flock` on Unix, `LockFileEx` on Windows), so reading or copying `LOCK` doesn't release it, and it is released when the store is closed, even while a child process the server started is still starting. Don't put a data directory on NFS, because Linux emulates that lock there in a way any other close of the file releases.

Opening a store or restoring an image creates the data directory and any missing directory above it, and makes each one's name durable by syncing the directory that holds it. A directory directly under a drive's root (`D:\nrese-data`) works. If the holding directory can't be synced (for example one this process can't read), the open or restore is refused, so create the data directory beforehand.

On a rollback, the earlier binary opens the directory again only until this version writes its first checkpoint (a bulk load, a background or explicit checkpoint), and from then on checkpoints are format 12, which earlier binaries refuse (`UnsupportedFormat`). To roll back after that, restore the image backup taken before the upgrade, with the earlier binary. Before that point it opens the directory (checkpoint format 11, unchanged log), rebuilding the stacks whose rules differ. A repository switched to a mode only the new version has (`owl2-dl`) isn't opened by the earlier one, so switch it back first. Settings only the new version knows (`ql_rewriting`, `query_timeout_ms`, access `inferred = "supported"`) are ignored, `supported` reading as `hidden`.

The upgrade is tested (`crates/nrese-server/tests/it/upgrade_tests.rs`). A data directory written by `main`'s server (`crates/nrese-server/tests/fixtures/main-store`, with the script that writes it) is opened by this build's binary, with the same data and answers in every repository (the QL rewriting's one answer apart), every setting, local logins, current stacks, writes surviving a restart, and `main`'s image backup restoring.

### 13.2 Rollout
- Prefer blue/green or canary rollout.
- Before upgrade:
- Verify backward-compatible API behavior.
- Execute migration dry-run on snapshot copy.
- During upgrade:
- Keep one previous version ready for rollback.
- After upgrade:
- Compare key metrics and protocol conformance smoke tests.
