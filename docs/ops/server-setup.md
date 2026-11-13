# NRESE Server Setup and Deployment Guide

## 1. Purpose
This document defines how to deploy and operate `nrese-server` in production.
It covers native binary deployment, container deployment, TLS/reverse proxy setup, and production readiness checks.

## 1.1 Local Test-Server Profile

For local validation we run `nrese-server` as a test server with ontology preloading enabled.
The test profile is intended to validate:

- startup and readiness behavior
- ontology load success and failure semantics
- basic query path availability before full production hardening

## 2. Deployment Targets
- Linux `x86_64` and `aarch64` are primary production targets.
- Deployment modes:
- `systemd` managed native binary.
- OCI container with orchestrator or standalone runtime.

## 3. Runtime Topology
- One `nrese-server` instance manages one data directory: the default repository in it, and each repository created over the API in its `repositories/<id>/` ([HTTP interface](http-api.md#rdf4j-protocol)).
- Scale-out: read replicas follow a primary's log ([replication](replication.md)); nothing partitions the data across machines.
- Place reverse proxy in front of NRESE for TLS termination and edge controls.

### 3.1 Operator UI Endpoint
- `GET /ops` serves a lightweight operator console from the same server process.
- `GET /ui` remains as a compatibility alias.
- `GET /console` serves the user-facing console from the same server process.
- `GET /` redirects to `/console` for ergonomic access.
- `/console` calls same-origin endpoints such as `/dataset/query`, `/dataset/tell`, `/dataset/update`, `/dataset/data`, `/api/v1/ai/status`, and `/api/v1/ai/query-suggestions`.
- `/ops` remains the operator-facing surface for diagnostics and operational workflows.

## 4. Filesystem Layout
Recommended host layout:
- `/opt/nrese/bin/nrese-server`
- `/etc/nrese/nrese.env`
- `/etc/nrese/config.toml`
- `/var/lib/nrese/data`
- `/var/lib/nrese/backups`
- `/var/log/nrese/`

The service user MUST own data and log directories with least privilege.

## 5. Configuration Surface
The canonical runtime knob reference is [config-reference.md](./config-reference.md).

Configuration precedence ([config-reference.md](./config-reference.md#precedence)): a value is
taken from the first of
1. the command line: `--set KEY=VALUE` with a file key, repeatable, for every command
   (secrets are refused there)
2. the environment variable
3. the configuration file: the path from `--config` or `-c`, else `NRESE_CONFIG_PATH`
4. the built-in default

Current behavior:

- `config.toml` is supported as a first-class runtime input.
- Environment variables override file values.
- `--set KEY=VALUE` overrides one file key, over the file and the environment.
- Typed runtime defaults and validation stay in the owning crates.
- External parsing, file loading, and precedence stay in `crates/nrese-server/src/config/`.
- `server.deployment_posture` / `NRESE_DEPLOYMENT_POSTURE` is now the explicit deployment-mode selector for `open-workbench`, `read-only-demo`, `internal-authenticated`, and `replacement-grade`.
- Startup validation now rejects invalid `internal-authenticated` / `replacement-grade` combinations instead of silently serving them.

Durable storage note:

- `NRESE_STORE_MODE=on-disk` stores a write-ahead log and checkpoints under `NRESE_DATA_DIR`.
  - It needs no build feature and no native toolchain.
  - Every commit is synced before it is acknowledged.
- The data directory is locked: one process at a time. A second server or offline command on the same directory fails at startup ([what to do](#err-data-directory-locked)).
- Checkpoints run in the background and delete the WAL segments they cover.
- Backups:
  - **Image backups** (physical): `POST /api/v1/repositories/{id}/image` (admins; the earlier `POST /ops/api/admin/dataset/image`, with `?repository=ID` for another repository than the default, still answers, into `backups/<ID>-<seconds>/`) writes an image of the current snapshot, the store's checkpoint format with the inferred statements, into `backups/<seconds since 1970>/` of the data directory, with `manifest.json` (revision, counts, size, SHA-256), while writes go on. Offline: `nrese-server backup DIR`. To restore, stop the server and run `nrese-server restore DIR` with the configuration of the target: it checks the image against its manifest and places it in the data directory, which must hold no store; the server then opens at the backup's revision. The image needs an NRESE that reads its checkpoint format.
  - **N-Quads export**: `GET /api/v1/repositories/{id}/backup` and `POST …/restore` (earlier: `/ops/api/admin/dataset/backup`, `/restore`): asserted statements only, portable to other stores, inferences derived again after a restore.
  - **Point-in-time restore**: with `store.wal_archive` on, checkpoints move the WAL segments they cover into `wal-archive/` instead of deleting them. `nrese-server restore DIR --wal DATA/wal-archive --wal DATA/wal --until-revision N` restores the image and replays the log up to revision `N` (every commit is one revision: `/api/v1/repositories/{id}/info` gives it to whole-dataset readers or administrators, and the logs record it), or with `--until-time 2026-10-02T14:05:00Z` (RFC 3339 with a zone, read by the `xsd:dateTime` calendar: no leap second `:60`, offsets within ±14:00, `24:00:00` the next day's midnight, digits past the microsecond cut off) the commits made up to that time (logged since WAL version 6), cutting it there; without `--until-revision` as far as the log goes. A restore by time refuses an image whose last commit was made after the time (take an older image), or one with no commit time; restore by revision instead. Images without a commit time: those written before 7 Nov 2026 (checkpoint format 11), and every image, a bulk replacement's included, of a store that was upgraded from such a checkpoint, of a store created before 2 Oct 2026 (it keeps integer literals in its dictionary, and its log keeps no times), or of a replica of one of these. To get restore by time for such a store, export it as N-Quads and `nrese-server load` the export into a new, empty data directory; that store's images have commit times. After a refusal the target directory holds the placed image, so clear it before trying again. The target data directory must hold no store. `nrese-server prune-archive R` removes the archived segments whose records all come before revision `R` (after an image backup at `R - 1` or later they are not needed; safe while the server runs).
  - Or copy the directory while the server is stopped.

External exposure note:

- Keep default bind (`127.0.0.1`) for local/dev.
- For externally reachable deployment, bind `NRESE_BIND_ADDR=0.0.0.0:8080` and front with TLS reverse proxy plus access controls.
- In `mtls` mode, NRESE trusts authenticated client-certificate identity only through the documented trusted reverse-proxy header contract. It does not terminate client TLS certificates directly in-process in the current implementation.

### 5.2 Reliability and Storage Variables

No setting schedules backups: they are taken through the admin API or offline with `nrese-server backup` (above, and [backup-restore-drills.md](backup-restore-drills.md)). Earlier versions of this document listed `NRESE_SNAPSHOT_RETENTION`, `NRESE_BACKUP_*` and `NRESE_RECOVERY_MODE`; none of them were ever implemented. The checkpoint and log settings (`store.checkpoint_after_wal`, `store.wal_archive`, `store.wal_damage`, `store.verify_on_open`, `store.map_checkpoints`) are in [config-reference.md](config-reference.md), the only place settings are listed.

### 5.3 Ontology Preload

- Set `NRESE_ONTOLOGY_PATH` (or `store.ontology_path`) to preload an ontology at startup.
- If it is set and the file is missing, startup fails with a clear error.
- If it is unset, nothing is preloaded. There are no implicit discovery or fallback paths, so the server behaves the same regardless of the working directory.

### 5.4 Bulk Loading

Initial loads and full restores of large files use the offline bulk loader instead of HTTP:

```powershell
$env:NRESE_STORE_MODE = "on-disk"; $env:NRESE_DATA_DIR = ".\data"
nrese-server load [--config .\config.toml] [--replace] [--graph <IRI>] [--strict | --quarantine] data.nt more.nq ...
```

- **Behaviour:**
  - The format comes from each file's extension.
  - `--replace` swaps the dataset instead of adding to it.
  - `--graph` sets the target graph for triple formats; quad formats keep their own graphs.
  - Blank nodes are fresh per load.
  - A statement with a syntax error goes into the repository's quarantine and the load goes on (`store.invalid_statements = "quarantine"`, the default; `--quarantine`, or the older `--skip-errors`): a line of N-Triples or N-Quads, a Turtle or TriG statement up to its `.`, an RDF/XML node element, a JSON-LD top-level object. It is kept verbatim with its file, line and reason, invisible to queries, reasoning, exports and statistics until it is re-admitted (fixed, or repaired on request, through the commit gates) or discarded ([quarantine.md](../design/quarantine.md)). The load prints the count and the first examples. With `--strict` (or `store.invalid_statements = "strict"`) the first stops the load and nothing of it is committed. Malformed XML or JSON and read errors always stop.
- **Offline only.** The server must not be running on the same data directory; the directory lock enforces this. Validation gates don't run during a bulk load.
- **Speed.** N-Triples and N-Quads are parsed on all cores and are the fastest input: about 2.7 M triples/s on the reference machine, with 100 M triples in 37 s (`benches/baselines/README.md`). Convert other formats to N-Triples for the largest loads (`nrese-server convert`, below).

### 5.4.1 One-off Queries and Conversions

```powershell
nrese-server query [--config .\config.toml] [--format json|xml|csv|tsv|nt|ttl|nq|trig|rdf|jsonld] "SELECT ..."
nrese-server query [--config .\config.toml] --file query.rq
nrese-server convert data.ttl data.nt
nrese-server print-query [--config .\config.toml] [--form paths|values] [--schema ontology.ttl]... "SELECT ..."
```

- `query` answers one query from the configured store and writes the results to standard output. First it brings the inferred stack up to date, as the server's start does (it rematerialises if the rules changed since the last load or start): SELECT and ASK in the results format (default JSON), CONSTRUCT and DESCRIBE in the RDF format (default N-Triples). Offline, like `load`.
- `print-query` prints the query as standard SPARQL 1.1 for a store without reasoning that holds the same statements: it answers as NRESE does under `owl2-rl` with the QL rewriting on (`docs/design/ql-rewriting.md` §8). `--form paths` (the default) reaches the hierarchy with property paths over the schema, `values` enumerates it. Against the configured store, or with `--schema` against those files alone. The first line, `# NRESE completeness: …`, is NRESE's status for the answers; a query that can't be written exactly (equality, recursive rules, …) gets its reasons on standard error and exit status 2.
- `convert` reads one RDF file and writes it in another format, both taken from the extensions, statement by statement and without a store, so the file can be larger than memory. Blank node labels are kept; a named graph is an error for a format without graphs. The output appears only when complete.

### 5.4.2 Moving Data In from Another Store

From Fuseki, GraphDB, RDF4J or any SPARQL store: export the dataset as N-Quads (named
graphs kept), load it with `nrese-server load`, then check before switching clients over:
- the statement count per graph (`SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g`, and the default graph) on both sides;
- the application's own queries and updates on both sides, compared by result;
- with reasoning on, that the source store's ruleset matches the configured mode
  ([reasoning semantics](../spec/reasoning-semantics.md)): a different ruleset changes
  the answers, not only their speed.

Keep the old store until the comparison passes; switching back is changing the client's
endpoint.

### Commands

`nrese-server [COMMAND] [OPTIONS]`. Every command takes `--config FILE` (also `-c FILE`, `--config=FILE`) and `--set KEY=VALUE` (repeatable; [precedence](config-reference.md#precedence)); `convert`, `config-schema` and `print-query --schema` read no configuration. An unknown option is an error. Commands log to standard error; the server logs to standard output.

Exit status: `0` on success; `1` on any error, with `Error: <message>` and its causes on standard error; `2` from `print-query` when the query can't be written exactly. A binary built for a CPU feature this machine lacks exits `1` before anything runs, naming the feature.

Commands marked *store* open the configured store and lock its data directory: they fail while another process has it open ([locked](#err-data-directory-locked)).

| Command | Arguments and options (defaults) | What it does and changes | Fails when |
|---|---|---|---|
| (none) | | *store*. Serves HTTP on `server.bind_address`: opens the store, brings the inferred stack up to date, then serves until SIGTERM or SIGINT ([stopping](#64-stopping), exit `0`) | the configuration is invalid; the directory is locked; the address can't be bound (`failed to bind ADDR`); startup reasoning fails |
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

A server that fails to start writes `Error: <message>` and its causes to standard error (the journal, `docker logs`) and exits `1`. Configuration errors name the setting; run `nrese-server check-config` with the same configuration to see them without starting.

<a id="err-data-directory-locked"></a>
**Data directory locked.**

```text
Error: storage engine error: data directory ./data is locked by another process

Caused by:
    data directory ./data is locked by another process
```

- **Cause:** another process has the store in `store.data_dir` (the path shown) open: a running server, or an offline command (`load`, `query`, `print-query`, `backup`) on the same directory. The lock is that process's hold on the directory's `LOCK` file, released when it closes the store.
- **Fix:** stop the other process (`systemctl stop nrese`, `docker stop nrese`), or wait for the offline command to end; or give this one another data directory. On Linux, `fuser DATA_DIR/LOCK` names the process. Don't delete `LOCK`: the file isn't the lock, the other process's hold on it is.
- **Check:** start the server again; once it reports ready, `curl -fsS http://127.0.0.1:8080/readyz` answers `{"status":"ready"}`.

## 5.5 Local Test-Server Startup Example (PowerShell)

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:8080"
$env:NRESE_DEPLOYMENT_POSTURE = "open-workbench"
$env:NRESE_DATA_DIR = ".\data"
$env:RUST_LOG = "info"
$env:NRESE_ONTOLOGY_PATH = ".\ontology.ttl"
cargo run -p nrese-server
```

Explicit config file startup:

```powershell
cargo run -p nrese-server -- --config .\config.toml
```

Build the frontend before serving `/console`:

```powershell
Set-Location .\apps\nrese-console
npm install
npm run build
Set-Location ..\..
```

## 6. Native Binary Deployment (`systemd`)

### 6.1 Create Service User
- Create dedicated non-login user `nrese`.
- Grant ownership of `/var/lib/nrese` and `/var/log/nrese`.

### 6.2 Example Unit File
Create `/etc/systemd/system/nrese.service`:

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

### 6.3 Start and Verify
- `systemctl daemon-reload`
- `systemctl enable --now nrese.service`
- `systemctl status nrese.service`
- `journalctl -u nrese.service -f`
- `curl -fsS http://127.0.0.1:8080/healthz`
- `curl -fsSI http://127.0.0.1:8080/ops`
- `curl -fsSI http://127.0.0.1:8080/console`

### 6.4 Stopping
A stop signal ends the server cleanly: SIGTERM (`systemctl stop`, `docker stop`, Kubernetes) or SIGINT (ctrl-c; on Windows, ctrl-c in the server's console). In this order:
1. It accepts no new connections, closes idle ones, and lets the requests in flight finish, for up to 5 s. A request still running then is cut: its client sees the connection drop, and doesn't know whether a commit it sent was made, as on any kill.
2. A replica's follower stops, and the store closes; it waits for the maintenance in progress first, the longest of which is a background checkpoint (it rewrites the whole store).
3. It logs `nrese-server stopped` (the signal, how long the stop took, whether requests were cut) and exits with code 0.

One deadline bounds the stop: 8 s from the signal. A stop not done by then logs `nrese-server stop deadline reached`, naming what it still waited for, and 200 ms later ends the process with code 0, as a kill would. The tests check that a stopped server has ended within 9 s of the signal: give the supervisor a stop allowance above that before it kills (`docker stop` waits 10 s unless `-t` says otherwise). Nothing acknowledged is lost either way: every commit is synced to the log before it is acknowledged, and the next start recovers from the checkpoint and the log, as after a kill ([durability](../spec/02-storage-and-transactions.md#durability-on-disk-mode)).

Not handled: a signal during start-up (while the store opens and start-up reasoning runs) acts as a kill (in the image, Docker kills at the end of its grace period); on Windows, a service stop or `taskkill /F` ends the process without the drain.

## 7. Container Deployment

### 7.1 Image Requirements
- Minimal base image.
- Non-root runtime user.
- Read-only root filesystem where possible.
- Writable volume mounted only for `/var/lib/nrese`.

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

`nrese` is the image built from this repository (`docker build -t nrese .`, as in the [README](../../README.md#quick-start)).

### 7.3 Kubernetes Notes
- Use readiness probe `/readyz` and liveness probe `/healthz`.
- Use `PodDisruptionBudget` for availability.
- Mount persistent volume for data.
- Store secrets in cluster secret manager.
- Use `NetworkPolicy` to restrict access to trusted namespaces and ingress controllers.

## 8. Reverse Proxy and TLS

### 8.1 Recommended Edge Pattern
- TLS terminates at reverse proxy.
- NRESE binds to localhost/private network only.
- Proxy forwards:
- `X-Forwarded-For`
- `X-Forwarded-Proto`
- `X-Request-Id`
- Expose `/console`, `/ops`, and API routes through the same trusted host, and apply identical auth/rate-limit policy unless you intentionally split user and operator access.

### 8.2 Nginx Example
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

## 9. Auth and Secret Management
- Never hardcode credentials or key material in images.
- Use file mounts or secret stores for JWT signing secrets, keys, and OIDC secrets.
- Rotate keys and certificates on defined schedule.
- Enforce token issuer, audience, and expiration checks.

## 10. Backup and Recovery Setup
- Schedule periodic snapshot backups to local or remote object storage.
- Validate backups with periodic restore drills.
- Recovery procedure:
1. Stop writer traffic.
2. Restore latest consistent snapshot to staging path.
3. Start the server on it with writes disabled (`server.deployment_posture = "read-only-demo"`).
4. Run integrity checks and smoke queries.
5. Promote restored dataset and switch traffic.

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

## 13. Upgrade Strategy

### 13.1 Upgrading a Store (from `main` to v2)

A data directory written by an earlier NRESE opens as it is: no migration step. Checkpoints of formats 4 to 11 are read, the write-ahead log is replayed (also after a crash or a kill), and the repositories, their settings and rules, the namespaces, the access state (users, local logins, roles, workspaces, saved queries) and the image backups carry over. Sessions and the result cache live in memory and start empty, as at every start.

1. Stop the server, and keep a copy of the data directory (or an image backup) and the old binary.
2. Install the new binary and start it with the same configuration. The first start brings each inferred stack in line with the new rules: a stack whose rules didn't change (the reasoning marker's fingerprint, `reasoning.state`) is kept; one whose rules did is rebuilt before the server reports ready, which takes about as long as reasoning after a bulk load. From `main` (d9a4b24) to v2, `owl2-ql` and `owl-horst` stacks are rebuilt; `rdfs`, `owl2-rl` and repositories with their own rules are kept.
3. Check `/readyz` for readiness, then read `revision` and `quad_count` from `/api/v1/repositories/{id}/info` as a whole-dataset reader or administrator, and compare a few known queries against the old server's answers.

What changes on purpose:
- **Answers through existentials under `owl2-ql`:** the QL rewriting is on by default there (`reasoner.ql_rewriting = "auto"`), so queries can find individuals that only an existential axiom makes match (`Employee ⊑ ∃worksFor`: an employee without a stated employer now works for something). `reasoner.ql_rewriting = "off"`, or a repository's own `ql_rewriting`, keeps the earlier answers. Under `owl2-rl` nothing changes.
- **The N-Quads export's checksum** moves from a leading header to the end: the export streams (constant memory, at any store size), so its SHA-256 is known only after the body. It closes the body as the comment `# nrese-sha256 <hex>` (over every byte before it; N-Quads readers skip it) and follows it as the trailer `x-nrese-checksum-sha256`, announced in `Trailer` (HTTP/1.1 clients ask for trailers with `TE: trailers`). A restore checks the comment where present and refuses a mismatch (400). Graph Store `GET` and the export stream too, and their timeout (`budgets.graph_read_timeout`) bounds the first chunk and each stall, not the whole transfer.
- **Repository ids:** new repositories can't have an id starting or ending with `.` or a Windows device name (`CON`, `NUL`, `COM1`, ...). Existing ones keep working and are logged at start; copy one into a repository with another id to move the data directory between systems.

**One process per data directory:** a second process opening a data directory one already has open is refused (`Locked`), whichever version each runs: earlier and this version lock the directory's `LOCK` file the same way. The lock is the open file's (`flock` on Unix, `LockFileEx` on Windows): reading or copying `LOCK` doesn't release it, and it is released when the store is closed, even while a child process the server started is still starting. Don't put a data directory on NFS: Linux emulates that lock there in a way any other close of the file releases.

**New data directories:** opening a store or restoring an image creates the data directory and any missing directory above it, and makes each one's name durable by syncing the directory that holds it. A directory directly under a drive's root (`D:\nrese-data`) works. If the holding directory can't be synced (for example one this process can't read), the open or restore is refused: create the data directory beforehand.

**Rolling back:** the earlier binary opens the directory again only until this version writes its first checkpoint (a bulk load, a background or explicit checkpoint): from then on checkpoints are format 12, which earlier binaries refuse (`UnsupportedFormat`). To roll back after that, restore the image backup taken before the upgrade, with the earlier binary. Before that point it opens the directory (checkpoint format 11, unchanged log), rebuilding the stacks whose rules differ. A repository switched to a mode only the new version has (`owl2-dl`) isn't opened by the earlier one: switch it back first. Settings only the new version knows (`ql_rewriting`, `query_timeout_ms`, access `inferred = "supported"`) are ignored, `supported` reading as `hidden`.

**Tested** (`crates/nrese-server/tests/it/upgrade_tests.rs`): a data directory written by `main`'s server (`crates/nrese-server/tests/fixtures/main-store`, with the script that writes it) opened by this build's binary: the same data and answers in every repository (the QL rewriting's one answer apart), every setting, local logins, current stacks, writes surviving a restart, and `main`'s image backup restoring.

### 13.2 Rollout
- Prefer blue/green or canary rollout.
- Before upgrade:
- Verify backward-compatible API behavior.
- Execute migration dry-run on snapshot copy.
- During upgrade:
- Keep one previous version ready for rollback.
- After upgrade:
- Compare key metrics and protocol conformance smoke tests.
