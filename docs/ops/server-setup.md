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
- One `nrese-server` instance manages one active dataset storage directory.
- Scale-out is read-heavy first, then sharded or partitioned by dataset at architecture level.
- Place reverse proxy in front of NRESE for TLS termination and edge controls.

### 3.1 Operator UI Endpoint
- `GET /ops` serves a lightweight operator console from the same server process.
- `GET /ui` remains as a compatibility alias.
- `GET /console` serves the user-facing console from the same server process.
- `GET /` redirects to `/console` for ergonomic access.
- `/console` calls same-origin endpoints such as `/dataset/query`, `/dataset/tell`, `/dataset/update`, `/dataset/data`, `/api/ai/status`, and `/api/ai/query-suggestions`.
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

Configuration precedence:
1. CLI config path via `--config` or `-c`
2. `NRESE_CONFIG_PATH` for selecting the config file path when no CLI path is given
3. environment variables
4. `config.toml`
5. built-in defaults

Current behavior:

- `config.toml` is supported as a first-class runtime input.
- Environment variables override file values.
- CLI currently selects the config file path; per-setting overrides remain file/env based.
- Typed runtime defaults and validation stay in the owning crates.
- External parsing, file loading, and precedence stay in `crates/nrese-server/src/config/`.
- `server.deployment_posture` / `NRESE_DEPLOYMENT_POSTURE` is now the explicit deployment-mode selector for `open-workbench`, `read-only-demo`, `internal-authenticated`, and `replacement-grade`.
- Startup validation now rejects invalid `internal-authenticated` / `replacement-grade` combinations instead of silently serving them.

Durable storage note:

- `NRESE_STORE_MODE=on-disk` stores a write-ahead log and checkpoints under `NRESE_DATA_DIR`.
  - It needs no build feature and no native toolchain.
  - Every commit is synced before it is acknowledged.
- The data directory is locked. A second server on the same directory fails at startup with a clear error.
- Checkpoints run in the background and delete the WAL segments they cover.
- Backups:
  - **Image backups** (physical): `POST /ops/api/admin/dataset/image` (admins; `?repository=ID` for another repository than the default, into `backups/<ID>-<seconds>/`) writes an image of the current snapshot, the store's checkpoint format with the inferred statements, into `backups/<seconds since 1970>/` of the data directory, with `manifest.json` (revision, counts, size, SHA-256), while writes go on. Offline: `nrese-server backup DIR`. To restore, stop the server and run `nrese-server restore DIR` with the configuration of the target: it checks the image against its manifest and places it in the data directory, which must hold no store; the server then opens at the backup's revision. The image needs an NRESE that reads its checkpoint format.
  - **N-Quads export**: `GET /ops/api/admin/dataset/backup` and `POST /ops/api/admin/dataset/restore`: asserted statements only, portable to other stores, inferences derived again after a restore.
  - **Point-in-time restore**: with `store.wal_archive` on, checkpoints move the WAL segments they cover into `wal-archive/` instead of deleting them. `nrese-server restore DIR --wal DATA/wal-archive --wal DATA/wal --until-revision N` restores the image and replays the log up to revision `N` (every commit is one revision: `/readyz` and the logs give them), or with `--until-time 2026-10-02T14:05:00Z` (RFC 3339) the commits made up to that time (logged since WAL version 6), cutting it there; without `--until-revision` as far as the log goes. The target data directory must hold no store. `nrese-server prune-archive R` removes the archived segments whose records all come before revision `R` (after an image backup at `R - 1` or later they are not needed; safe while the server runs).
  - Or copy the directory while the server is stopped.

External exposure note:

- Keep default bind (`127.0.0.1`) for local/dev.
- For externally reachable deployment, bind `NRESE_BIND_ADDR=0.0.0.0:8080` and front with TLS reverse proxy plus access controls.
- In `mtls` mode, NRESE trusts authenticated client-certificate identity only through the documented trusted reverse-proxy header contract. It does not terminate client TLS certificates directly in-process in the current implementation.

### 5.2 Reliability and Storage Variables

There are no scheduled-backup or recovery-mode settings yet. Earlier versions of this document listed `NRESE_SNAPSHOT_RETENTION`, `NRESE_BACKUP_*` and `NRESE_RECOVERY_MODE`; none of them were ever implemented. Backups are taken through the admin API (see [backup-restore-drills.md](backup-restore-drills.md)). Checkpoint and WAL settings arrive with roadmap WP E4 and will be documented in [config-reference.md](config-reference.md), the only place config knobs are listed.

### 5.3 Ontology Preload

- Set `NRESE_ONTOLOGY_PATH` (or `store.ontology_path`) to preload an ontology at startup.
- If it is set and the file is missing, startup fails with a clear error.
- If it is unset, nothing is preloaded. There are no implicit discovery or fallback paths, so the server behaves the same regardless of the working directory.

### 5.4 Bulk Loading

Initial loads and full restores of large files use the offline bulk loader instead of HTTP:

```powershell
$env:NRESE_STORE_MODE = "on-disk"; $env:NRESE_DATA_DIR = ".\data"
nrese-server load [--config .\config.toml] [--replace] [--graph <IRI>] [--skip-errors] data.nt more.nq ...
```

- **Behaviour:**
  - The format comes from each file's extension.
  - `--replace` swaps the dataset instead of adding to it.
  - `--graph` sets the target graph for triple formats; quad formats keep their own graphs.
  - Blank nodes are fresh per load.
  - A syntax error stops the load and changes nothing. With `--skip-errors` the bad statement is skipped instead (a line of N-Triples or N-Quads, a Turtle or TriG statement up to its `.` or the `}` of its graph block), the first 20 are logged, and the load reports how many it skipped. RDF/XML and JSON-LD still stop at the first error; a read error always stops.
- **Offline only.** The server must not be running on the same data directory; the directory lock enforces this. Validation gates don't run during a bulk load.
- **Speed.** N-Triples and N-Quads are parsed on all cores and are the fastest input: about 2.7 M triples/s on the reference machine, with 100 M triples in 37 s (`benches/baselines/README.md`). Convert other formats to N-Triples for the largest loads (`nrese-server convert`, below).

### 5.4.1 One-off Queries and Conversions

```powershell
nrese-server query [--config .\config.toml] [--format json|xml|csv|tsv|nt|ttl|nq|trig|rdf|jsonld] "SELECT ..."
nrese-server query [--config .\config.toml] --file query.rq
nrese-server convert data.ttl data.nt
```

- `query` answers one query from the configured store as it is (no reasoning first; the inferred stack is what the last load or start materialised) and writes the results to standard output: SELECT and ASK in the results format (default JSON), CONSTRUCT and DESCRIBE in the RDF format (default N-Triples). Offline, like `load`.
- `convert` reads one RDF file and writes it in another format, both taken from the extensions, statement by statement and without a store, so the file can be larger than memory. Blank node labels are kept; a named graph is an error for a format without graphs. The output appears only when complete.

## 5.5 Local Test-Server Startup Example (PowerShell)

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:8080"
$env:NRESE_DEPLOYMENT_POSTURE = "open-workbench"
$env:NRESE_DATA_DIR = ".\data"
$env:RUST_LOG = "info"
$env:NRESE_ONTOLOGY_PATH = "C:\Users\Johannes\Documents\MEPHISTO\Ontology-Development\files\processed\rg_ontology.ttl"
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
  ghcr.io/example/nrese-server:latest
```

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
3. Start server in `read-only` validation mode.
4. Run integrity checks and smoke queries.
5. Promote restored dataset and switch traffic.

## 11. Production Readiness Checklist
- W3C protocol endpoints reachable and authenticated as expected.
- Operator UI endpoint (`/ops`) reachable and usable from approved external networks.
- User console endpoint (`/console`) reachable and usable from approved external networks.
- TLS policy validated by security scanner.
- AuthN/AuthZ tested for reader/writer/admin roles.
- Metrics and logs collected centrally.
- Ontology preload path and fallback behavior validated in pre-prod.
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

## 13. Upgrade Strategy
- Prefer blue/green or canary rollout.
- Before upgrade:
- Verify backward-compatible API behavior.
- Execute migration dry-run on snapshot copy.
- During upgrade:
- Keep one previous version ready for rollback.
- After upgrade:
- Compare key metrics and protocol conformance smoke tests.
