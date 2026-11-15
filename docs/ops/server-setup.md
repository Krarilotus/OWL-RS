# Running a server

How to run `nrese-server` for other people: in a container or under systemd, behind a TLS proxy,
with your data loaded, and how to stop and upgrade it. Every setting is in the
[configuration reference](config-reference.md), every endpoint in the
[HTTP interface](http-api.md). Backups have [their own page](backups.md), and so do
[read replicas](replication.md). To build from source, see [getting started](../dev/getting-started.md).

## What runs

- One process serves one data directory. It holds the default repository, and each repository
  created over the API in its `repositories/<id>/` ([repositories](http-api.md#rdf4j-protocol)).
- The store is in memory unless `store.mode` is `on-disk` (`NRESE_STORE_MODE`, which the image
  sets). On disk, every commit is synced to the write-ahead log before it is acknowledged, and
  checkpoints run in the background and remove the log segments they cover.
- One process at a time opens a data directory. A second server or offline command on it fails
  at start ([data directory locked](#err-data-directory-locked)). Don't put a data directory on
  NFS, where Linux emulates the lock in a way any other close of the file releases.
- Opening a store creates the data directory and any missing directory above it, and syncs
  each one's name. If a holding directory can't be synced (one this process can't read, for
  example), the open is refused, so create the data directory beforehand.
- The same process serves the SPARQL endpoints, the console at `/console` (`/` redirects there)
  and the operator page at `/ops` (also at `/ui`).
- For more queries, [read replicas](replication.md) follow a primary's log. Nothing partitions
  the data across machines.

## In a container

The image is built from this repository (`docker build -t nrese .`, as in the
[quick start](../../README.md#quick-start)). It runs as the user `nrese` (uid 10001), who must
own a data directory mounted from the host, and sets
`NRESE_BIND_ADDR=0.0.0.0:8080`, `NRESE_STORE_MODE=on-disk` and
`NRESE_DATA_DIR=/var/lib/nrese/data`, the volume. Everything else keeps the server's defaults.

```bash
docker run -d --name nrese --read-only --tmpfs /tmp \
  -p 127.0.0.1:8080:8080 \
  -v /var/lib/nrese/data:/var/lib/nrese/data \
  --env-file /etc/nrese/nrese.env \
  nrese
```

`/etc/nrese/nrese.env` holds your settings as environment variables, the secrets among them, so
that none is in the image or the command line ([auth modes](config-reference.md#auth-modes)):

```bash
NRESE_DEPLOYMENT_POSTURE=internal-authenticated
NRESE_AUTH_MODE=bearer-jwt
NRESE_AUTH_JWT_SECRET=...
NRESE_AUTH_JWT_ISSUER=https://login.example.org/
```

The image's entrypoint is `nrese-server`, so a [command](#commands) runs as
`docker run --rm nrese check-config`. On Kubernetes, use `/readyz` as the readiness probe,
`/healthz` as the liveness probe and a persistent volume for the data.

## Under systemd

Put the binary in `/opt/nrese/bin/`, the settings in `/etc/nrese/` and the data in
`/var/lib/nrese/data`, owned by a non-login user `nrese`. The server logs to standard output, so
the journal has its log. `/etc/nrese/nrese.env` is as above, with `NRESE_STORE_MODE=on-disk` and
`NRESE_DATA_DIR=/var/lib/nrese/data`, readable by `nrese` alone.

```ini
# /etc/systemd/system/nrese.service
[Unit]
Description=NRESE
After=network-online.target
Wants=network-online.target

[Service]
User=nrese
Group=nrese
EnvironmentFile=/etc/nrese/nrese.env
ExecStart=/opt/nrese/bin/nrese-server --config /etc/nrese/config.toml
Restart=on-failure
RestartSec=3
LimitNOFILE=65536
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/nrese
CapabilityBoundingSet=

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable --now nrese
journalctl -u nrese -f
curl -fsS http://127.0.0.1:8080/readyz
```

## Settings to choose first

A setting comes from the command line (`--set KEY=VALUE`), the environment, the file or the
default, in that order ([precedence](config-reference.md#precedence)).
`nrese-server check-config` shows what a configuration comes to without starting a server.

- **What the server exposes:** [`server.deployment_posture`](config-reference.md#server.deployment_posture),
  one of `open-workbench` (the default, which exposes everything), `read-only-demo`,
  `internal-authenticated` and `replacement-grade`. The server refuses to start on a posture
  whose requirements the other settings don't meet, such as `internal-authenticated` without
  authentication.
- **Who may do what:** [`auth.mode`](config-reference.md#auth-modes) and, for access per graph,
  an [access policy](http-api.md#graph-level-access-control).
- **Where it listens:** `server.bind_address`, `127.0.0.1:8080` by default. For other machines,
  `0.0.0.0:8080` behind a TLS proxy.
- **How it reasons:** [`reasoner.mode`](config-reference.md#reasoner) and the
  [SHACL gate](config-reference.md#shacl).
- **An ontology at start:** `store.ontology_path` (`NRESE_ONTOLOGY_PATH`) loads a file at
  every start. A missing file stops the start, and with the setting unset nothing is loaded,
  whatever the working directory.

## Behind a TLS proxy

TLS ends at the proxy, and NRESE listens on localhost or a private network only. The proxy passes
on `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Request-Id`, and the server believes
`X-Forwarded-For` only from the proxies in
[`auth.trusted_proxies`](config-reference.md#auth.trusted_proxies). The console, the operator page
and the API go through the same host and the same access rules, unless you split user and
operator access on purpose.

```nginx
server {
    listen 443 ssl http2;
    server_name sparql.example.org;

    ssl_certificate     /etc/letsencrypt/live/sparql.example.org/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/sparql.example.org/privkey.pem;
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;

    client_max_body_size 16m;
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

With `auth.mode = "mtls"` the proxy also checks the client certificates and passes the subject
on in a header. NRESE doesn't check certificates itself, and takes that header only from
[`auth.mtls.trusted_proxies`](config-reference.md#auth.mtls.trusted_proxies).

## Before others use it

- The posture and the authentication are set, and each role was tried, and a request without
  credentials.
- The server listens only where the proxy reaches it, and the proxy's TLS passes a scanner.
- Backups are scheduled, and a [restore was drilled](backups.md#a-complete-drill).
- `/metrics` is scraped and the log collected, with alerts on `5xx` answers
  (`nrese_http_responses_total`) and on the data volume's free space.
- The ontology preload, if any, was tried on a copy.

## Loading data

Large files load offline, while no server has the data directory open:

```bash
NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=/var/lib/nrese/data \
  nrese-server load [--replace] [--graph IRI] [--strict | --quarantine] data.nt more.nq ...
```

- Each file's format comes from its extension. Triple formats load into the default graph, or
  into `--graph`, and quad formats keep their graphs. Blank nodes are fresh for each load.
- `--replace` replaces the dataset instead of adding to it.
- A statement that can't be read goes to the repository's [quarantine](../design/quarantine.md),
  and the load goes on (`store.invalid_statements = "quarantine"`, the default). There it is kept
  with its file, line and reason, out of queries, reasoning and exports, until it is fixed and
  admitted through the usual checks, or discarded. The load prints the count and the first few.
  With `--strict` the first one stops the load, and nothing of it is committed. Malformed XML or
  JSON, and files that can't be read, always stop it.
- A load isn't refused the way a write is: the SHACL gate doesn't run, and a load that leaves
  the data inconsistent under the ruleset exits `1` with the count of violations, the data stays
  loaded, and the server starts in quarantine.
- N-Triples and N-Quads are read on all cores and load fastest, about 2.7 million statements a
  second on our reference machine (100 million in 37 s, [baselines](../../benches/baselines/README.md)).
  `nrese-server convert data.ttl data.nt` turns other formats into N-Triples first.

### From another store

Export the dataset as N-Quads, named graphs kept, load it, and check these before you move the
clients:

- the statement count of each graph on both sides
  (`SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g`, and the default graph);
- the application's own queries and updates, on both sides;
- with reasoning on, that the old store's ruleset is the [mode](../spec/reasoning-semantics.md#modes)
  you configured, since another ruleset gives other answers.

Keep the old store until the comparison passes. Moving back is changing the clients' endpoint.

## Commands

`nrese-server [COMMAND] [OPTIONS]`. Every command takes `--config FILE` (also `-c`) and
`--set KEY=VALUE` (repeatable). `convert`, `config-schema` and `print-query --schema` read no
configuration. An unknown option is an error. Commands log to standard error, and the server to
standard output.

The exit status is `0` on success and `1` on any error, with `Error: <message>` and its causes on
standard error, and `2` from `print-query` for a query it can't write exactly. A binary built for
a CPU feature this machine lacks exits `1` before anything runs, naming the feature.

Commands marked *store* open the configured store and lock its data directory, so they fail
while another process has it open.

| Command | Arguments and options (defaults) | What it does | Fails when |
|---|---|---|---|
| (none) | | *store*. Opens the store, brings the inferred statements up to date, then serves HTTP on `server.bind_address` until SIGTERM or SIGINT ([stopping](#stopping)) | the configuration is invalid; the directory is locked; the address can't be bound ([`failed to bind`](#err-failed-to-bind)); reasoning at start fails |
| `load` | `FILE...` (format by extension); `--replace` (default: add); `--graph IRI` (triple formats; default: the default graph); `--strict` or `--quarantine` (older `--skip-errors`; default: `store.invalid_statements`) | *store*. Commits the files' statements, then derives the inferences again and prepares the planner's statistics ([loading data](#loading-data)) | no file; a file can't be read or parsed (with `--strict`: any bad statement); the data is inconsistent under the ruleset (exit `1`, the data stays loaded, and the server starts in quarantine) |
| `query` | `QUERY` or `--file FILE`; `--format` `json`, `xml`, `csv`, `tsv`, or for CONSTRUCT and DESCRIBE `nt`, `ttl`, `nq`, `trig`, `rdf`, `jsonld` (default JSON, N-Triples) | *store*. Answers one query over every graph, to standard output | both or neither of `QUERY` and `--file`; an unknown format; the query doesn't parse or fails |
| `print-query` | `QUERY` or `--file FILE`; `--form paths\|values` (default `paths`); `--schema FILE` (repeatable; default: the configured store) | *store* without `--schema`. Prints the query as plain SPARQL 1.1. Changes nothing | exit `2`: it can't be written exactly, with the reasons on standard error |
| `convert` | `INPUT OUTPUT` (formats by extension) | Writes `OUTPUT` from `INPUT`, without a store | not two files; an unknown extension; a named graph in a format without graphs |
| `check-config` | | Validates the configuration and prints the main settings in effect, a `cpu:` line and `configuration is valid` ([what it prints](config-reference.md#validation)) | the configuration is invalid: it names the setting and the reason |
| `config-schema` | | Prints the JSON Schema of the configuration file | |
| `backup` | `DIR` | *store*. Writes an [image](backups.md#image-backups) of the store into `DIR` | the directory is locked; writing fails |
| `restore` | `DIR`; `--wal LOGDIR` (repeatable; default: no replay); `--until-revision N` or `--until-time T` | Places the image in `DIR` in the configured data directory, then replays the log ([point-in-time restore](backups.md#point-in-time-restore)) | the data directory holds a store; the image fails its manifest check; `T` comes before the image's last commit, or the image has no commit times |
| `prune-archive` | `REVISION` | Removes the segments in `wal-archive/` whose records all come before `REVISION`. Safe while the server runs | no revision, or not a number |

`query` and `print-query` first bring the inferred statements up to date, as a start does, and
derive them again if the rules changed since. `query` writes them, so it may change the store's
files, not its data.

`print-query` writes a query that a store without reasoning, holding the same statements,
answers as NRESE does under `owl2-rl` with the QL rewriting on. `--form paths` reaches the class
and property hierarchy with property paths over the schema, and `values` lists it. Its first line,
`# NRESE completeness: …`, is the [status](../spec/reasoning-semantics.md) NRESE gives the answers.

`convert` goes statement by statement, so a file can be larger than memory. It keeps blank node
labels, and writes the output only when it is complete.

## Startup errors

A server that fails to start writes `Error: <message>` and its causes to standard error (the
journal, `docker logs`) and exits `1`. A configuration error names the setting, and
`nrese-server check-config` with the same configuration shows it without starting.

<a id="err-data-directory-locked"></a>
#### Data directory locked

```text
Error: storage engine error: data directory ./data is locked by another process

Caused by:
    data directory ./data is locked by another process
```

- **Cause:** another process has the store in `store.data_dir` (the path shown) open, a running
  server or an offline command (`load`, `query`, `print-query`, `backup`). The lock is that
  process's hold on the directory's `LOCK` file, released when it closes the store.
- **Fix:** stop the other process (`systemctl stop nrese`, `docker stop nrese`), wait for the
  offline command to end, or give this one another data directory. On Linux,
  `fuser DATA_DIR/LOCK` names the process. Don't delete `LOCK`: the file isn't the lock, the
  other process's hold on it is.
- **Check:** start the server again. Once it is ready, `curl -fsS http://127.0.0.1:8080/readyz`
  answers `{"status":"ready"}`.

<a id="err-failed-to-bind"></a>
#### Address can't be bound

```text
Error: failed to bind 0.0.0.0:8080

Caused by:
    Address already in use (os error 98)
```

- **Cause:** the last line says which. `Address already in use`: another process listens on
  that port. `Cannot assign requested address`: the address isn't one of this machine's.
  `Permission denied`: a port below 1024, without the right to bind it.
- **Fix:** stop the other listener (on Linux, `ss -ltnp 'sport = :8080'` names it), or choose
  another address or port in `server.bind_address` (`NRESE_BIND_ADDR`), such as
  `127.0.0.1:8081`. In Docker the server binds inside the container, and a busy port on the
  machine shows as `docker run`'s own error, `port is already allocated`: publish another one
  (`-p 127.0.0.1:8081:8080`).
- **Check:** start the server again, and `curl -fsS http://127.0.0.1:8080/readyz` (with your
  port) answers `{"status":"ready"}`.

## Stopping

SIGTERM (`systemctl stop`, `docker stop`, Kubernetes) or SIGINT (ctrl-c, also in the server's
console on Windows) ends the server cleanly, in this order:

1. It takes no new connections, closes idle ones and lets the requests in flight finish, for up
   to 5 s. A request still running then is cut: its client sees the connection drop, and
   doesn't know whether a commit it sent was made, as on any kill.
2. A replica stops following, and the store closes, after the maintenance in progress. The
   longest is a background checkpoint, which rewrites the whole store.
3. It logs `nrese-server stopped`, with the signal, how long the stop took and whether requests
   were cut, and exits `0`.

The stop has 8 s from the signal. A stop not done by then logs `nrese-server stop deadline
reached`, with what it still waited for, and 200 ms later ends the process with `0`, as a kill
would. The tests check that a server has ended within 9 s, so give the supervisor more than that
before it kills (`docker stop` waits 10 s unless `-t` says otherwise). Nothing acknowledged is
lost either way: every commit is in the log before it is acknowledged, and the next start
recovers from the checkpoint and the log ([durability](../spec/02-storage-and-transactions.md#durability-on-disk-mode)).

A signal during the start, while the store opens and reasons, acts as a kill. On Windows a
service stop or `taskkill /F` ends the process without letting requests finish.

## Memory over time

A server under steady load grows in memory for a while, then levels off. What the program
holds stops growing once its working set is in place, and what grows with the data does so by
design. Each part has a gauge in `/metrics`:

| What | Grows with | Stops at | Gauge |
|---|---|---|---|
| The data and its indexes | the statements | the data | `nrese_index_bytes`, `nrese_store_quads` |
| The dictionary | every distinct term ever written (a deleted term keeps its entry) | the distinct terms, so a store that keeps receiving new terms keeps growing | `nrese_dictionary_terms`, `nrese_dictionary_bytes` |
| The full-text index | the dictionary's literals, from the first text search on | as the dictionary | `nrese_text_index_terms` |
| The result cache | repeated queries on an unchanged store (a commit empties it) | `budgets.result_cache` | `nrese_query_cache_bytes` |
| The write-ahead log (on disk) | writes | `store.checkpoint_after_wal`, then a checkpoint | `nrese_wal_bytes_since_checkpoint` |
| Sessions, running queries | open client transactions, queries | their end (an idle session after ten minutes) | `nrese_sessions_open`, `nrese_queries_running` |
| The allocator | the most the program held at once, across its threads | what it reserved, within minutes | `nrese_allocator_bytes{kind="committed"}` |

The allocator is most of a serving process's memory. With
[`budgets.huge_pages`](config-reference.md#budgets.huge_pages) at its default, `on`, transparent
huge pages back it on Linux, where one touched byte makes 2 MiB resident. In a soak of 16 clients
(inserts, deletes, queries, transactions, Graph Store calls; 7 October 2026) the program held about
2 MiB once warm and never more than 15 MiB, while the process held 250 to 330 MiB, within the
about 600 MiB the allocator reserved in the first minutes. That is not a leak: the program's own
memory stays level, and `crates/nrese-store/tests/memory_churn.rs` checks it. With `off` the same
soak held 95 to 120 MiB, and materialising LUBM 100 took 26 % longer. `bulk` uses huge pages only
while a load or a materialisation runs.

`benches/probes/soak.py` reads every gauge above beside the resident memory and prints each
one's slope.

<a id="131-upgrading-a-store-from-main-to-v2"></a>
## Upgrading

A data directory written by an earlier NRESE, `main` (d9a4b24) included, opens as it is, with no
migration step. Checkpoints of formats 4 to 11 are read, and the log is replayed (also after a
crash or a kill). The repositories, their settings and rules, the namespaces, the access state
(users, local logins, roles, workspaces, saved queries) and the image backups carry over. Sessions
and the result cache start empty, as at every start.

1. Stop the server, and keep a copy of the data directory (or an image backup) and the old
   binary.
2. Start the new binary with the same configuration. The first start brings each repository's
   inferred statements in line with the new rules: they are kept where the rules didn't change
   (the fingerprint in `reasoning.state`), and derived again before the server reports ready
   where they did, which takes about as long as reasoning after a bulk load. From `main` to v2,
   `owl2-ql` and `owl-horst` are derived again, and `rdfs`, `owl2-rl` and repositories with their
   own rules are kept.
3. Check `/readyz`, read `revision` and `quad_count` from `GET /api/v1/repositories/{id}/info`,
   and compare a few known queries with the old server's answers.

What changes on purpose:

- Under `owl2-ql`, answers come through existentials. The QL rewriting is on by default there
  (`reasoner.ql_rewriting = "auto"`), so a query can find individuals that only an existential
  axiom makes match: with `Employee ⊑ ∃worksFor`, an employee with no stated employer now works
  for something. `reasoner.ql_rewriting = "off"`, or a repository's own `ql_rewriting`, keeps the
  earlier answers. Under `owl2-rl` nothing changes.
- The N-Quads export's checksum moves from a leading header to the end, since the export streams
  (in constant memory, at any store size). It closes the export as the comment
  `# nrese-sha256 <hex>` and follows it as the trailer `x-nrese-checksum-sha256` (HTTP/1.1
  clients ask for trailers with `TE: trailers`). A restore checks the comment where present. The
  Graph Store's `GET` and the export stream too, and their timeout
  (`budgets.graph_read_timeout`) bounds the first piece and each stall, not the whole transfer.
- A new repository's id can't start or end with `.` or be a Windows device name (`CON`, `NUL`,
  `COM1`, …). Existing ones keep working and are named in the log at start. To move the data
  directory to another system, first copy such a repository into one with another id.

Both versions lock a data directory the same way, so an old and a new process never have it
open at once.

To roll back: the earlier binary opens the directory again until the new one writes its first
checkpoint (a bulk load, a background or an explicit checkpoint). From then on checkpoints are
format 12, which earlier binaries refuse (`UnsupportedFormat`), so restore the image backup taken
before the upgrade, with the earlier binary. Before that point the earlier binary opens the
directory, deriving again where the rules differ. It doesn't open a repository switched to a mode
it lacks (`owl2-dl`), so switch that back first. It ignores settings it doesn't know
(`ql_rewriting`, `query_timeout_ms`), and reads `inferred = "supported"` as `hidden`.

The upgrade is tested: `crates/nrese-server/tests/it/upgrade_tests.rs` opens a data directory
written by `main`'s server (`crates/nrese-server/tests/fixtures/main-store`, with the script that
writes it) and finds the same data and answers in every repository (but the QL rewriting's one),
every setting, the local logins, current inferences, writes surviving a restart, and `main`'s
image backup restoring.
