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

Durable storage build note:

- `on-disk` mode requires building with `--features durable-storage`.
- On Windows, the current RocksDB dependency chain may require a working `libclang`/LLVM installation for `bindgen`.
- If the durable feature is not compiled in, `NRESE_STORE_MODE=on-disk` will fail fast with a typed startup error instead of silently downgrading.

External exposure note:

- Keep default bind (`127.0.0.1`) for local/dev.
- For externally reachable deployment, bind `NRESE_BIND_ADDR=0.0.0.0:8080` and front with TLS reverse proxy plus access controls.
- In `mtls` mode, NRESE trusts authenticated client-certificate identity only through the documented trusted reverse-proxy header contract. It does not terminate client TLS certificates directly in-process in the current implementation.

### 5.2 Reliability and Storage Variables
- `NRESE_SNAPSHOT_RETENTION` number of retained local snapshots.
- `NRESE_BACKUP_DIR` path for scheduled backups.
- `NRESE_BACKUP_INTERVAL` cron-like interval or duration.
- `NRESE_BACKUP_COMPRESSION` `none|zstd|gzip`.
- `NRESE_RECOVERY_MODE` `normal|replay|read-only`.

### 5.3 Ontology Preload and Path Discovery

`nrese-server` should resolve the ontology preload file in this order:

1. `NRESE_ONTOLOGY_PATH` (explicit override, highest priority)
2. `../Ontology-Development/files/processed/rg_ontology.ttl` resolved from the `OWL-RS` working directory
3. `../MEPHISTO/Ontology-Development/files/processed/rg_ontology.ttl` resolved from the `OWL-RS` working directory
4. `../Ontology-Development/files/raw/rg_ontology.ttl` resolved from the `OWL-RS` working directory
5. `../MEPHISTO/Ontology-Development/files/raw/rg_ontology.ttl` resolved from the `OWL-RS` working directory

Current known local canonical processed path:

- `C:\Users\Johannes\Documents\MEPHISTO\Ontology-Development\files\processed\rg_ontology.ttl`

Startup behavior requirements:

- If an explicit `NRESE_ONTOLOGY_PATH` is provided and missing, startup fails fast with a clear error.
- If discovery is enabled and no fallback path exists, startup continues only if ontology preload is optional in the selected profile.
- Readiness should remain `not ready` until required ontology preload has completed.

## 5.4 Local Test-Server Startup Example (PowerShell)

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:8080"
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
