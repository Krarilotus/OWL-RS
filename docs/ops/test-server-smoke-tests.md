# NRESE Test Server Smoke Tests

## Purpose

This document provides a minimal, repeatable smoke-test flow for local and staging validation of `nrese-server` with ontology preloading.

## Preconditions

- Rust toolchain is installed
- Repository root is `OWL-RS`
- Ontology file exists at one of the supported discovery paths, or `NRESE_ONTOLOGY_PATH` is set

Known local processed ontology path:

- `C:\Users\Johannes\Documents\MEPHISTO\Ontology-Development\files\processed\rg_ontology.ttl`

## 1. Start Server (PowerShell)

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:8080"
$env:NRESE_DATA_DIR = ".\data"
$env:RUST_LOG = "info"
$env:NRESE_ONTOLOGY_PATH = "C:\Users\Johannes\Documents\MEPHISTO\Ontology-Development\files\processed\rg_ontology.ttl"
cargo run -p nrese-server
```

Expected behavior:

- Process starts without panic
- Startup logs show ontology preload source path and success
- `readyz` only turns ready after required preload completes

## 2. Health and Readiness

```powershell
curl.exe -sS -i http://127.0.0.1:8080/healthz
curl.exe -sS -i http://127.0.0.1:8080/readyz
```

Expected:

- `/healthz` returns HTTP `200`
- `/readyz` returns HTTP `200` when store and required preload are complete

## 3. Operator UI Reachability

```powershell
curl.exe -sS -i http://127.0.0.1:8080/ops
curl.exe -sS -i http://127.0.0.1:8080/console
curl.exe -sS -i http://127.0.0.1:8080/
```

Expected:

- `/ops` returns HTTP `200` and `Content-Type: text/html`
- `/console` returns HTTP `200` and `Content-Type: text/html`
- `/ui` remains available as a compatibility alias
- `/` returns a redirect to `/console`

## 4. SPARQL Query Smoke Test

```powershell
$query = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 5"
curl.exe -sS -i -X POST `
  -H "Content-Type: application/sparql-query" `
  --data "$query" `
  http://127.0.0.1:8080/dataset/query
```

Expected:

- Endpoint returns HTTP `200`
- Response body contains a valid SPARQL result payload

## 5. SPARQL Update Smoke Test

```powershell
$update = "INSERT DATA { <http://example.com/runtime> <http://example.com/p> <http://example.com/o> }"
curl.exe -sS -i -X POST `
  -H "Content-Type: application/sparql-update" `
  --data "$update" `
  http://127.0.0.1:8080/dataset/update

$ask = "ASK WHERE { <http://example.com/runtime> <http://example.com/p> <http://example.com/o> }"
curl.exe -sS -G `
  -H "Accept: application/sparql-results+json" `
  --data-urlencode "query=$ask" `
  http://127.0.0.1:8080/dataset/query
```

Expected:

- Update endpoint returns HTTP `204`
- The follow-up `ASK` query returns `true`
- `/ops/api/health/extended` shows an incremented `revision`

## 6. Metrics Smoke Test

```powershell
curl.exe -sS http://127.0.0.1:8080/metrics
```

Expected:

- Output contains `nrese_ready`
- Output contains `nrese_dataset_revision`
- Output contains `nrese_store_quads`

## 7. Graph Store HEAD Smoke Test

```powershell
curl.exe -sS -I -H "Accept: text/turtle" "http://127.0.0.1:8080/dataset/data?default"
```

Expected:

- Endpoint returns HTTP `200`
- `Content-Type` is `text/turtle`
- Response body is empty

## 8. Failure-Mode Check (Path Validation)

Start with an invalid explicit path:

```powershell
$env:NRESE_ONTOLOGY_PATH = "C:\invalid\missing\rg_ontology.ttl"
cargo run -p nrese-server
```

Expected:

- Startup fails fast with a clear path resolution error when explicit path is required

## 9. Fallback Discovery Check

Unset explicit path:

```powershell
Remove-Item Env:NRESE_ONTOLOGY_PATH -ErrorAction SilentlyContinue
cargo run -p nrese-server
```

Expected:

- Server uses fallback discovery order documented in `docs/ops/server-setup.md`
- Resolved path appears in startup logs
