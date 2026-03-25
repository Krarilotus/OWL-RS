# NRESE Runtime Configuration Reference

## Purpose

This document is the operator-facing source of truth for `nrese-server` runtime configuration.

It owns:

- supported file keys
- supported environment overrides
- precedence rules
- examples for `config.toml`

Implementation ownership remains in `crates/nrese-server/src/config/`.

## Precedence

Configuration is resolved in this order:

1. CLI config path via `--config` or `-c`
2. `NRESE_CONFIG_PATH` for selecting the config file path when no CLI path is given
3. environment variable overrides for runtime values
4. `config.toml`
5. built-in defaults

Notes:

- CLI currently selects the config file path; individual runtime knobs are still configured through file or env.
- Environment variables always override values loaded from `config.toml`.
- If no config file path is provided by CLI or `NRESE_CONFIG_PATH`, the server runs from env/defaults only.

## Minimal Example

```toml
[server]
bind_address = "127.0.0.1:8080"

[store]
mode = "in-memory"
data_dir = "./data"
ontology_path = "C:/data/rg_ontology.ttl"

[reasoner]
mode = "rules-mvp"

[reasoner.rules_mvp]
preset = "bounded-owl"
features = [
  "rdfs-subclass-closure",
  "rdfs-subproperty-closure",
  "rdfs-type-propagation",
  "rdfs-domain-range-typing",
  "owl-property-assertion-closure",
  "owl-equality-reasoning",
  "owl-consistency-check",
  "unsupported-diagnostics",
]

[policy.limits]
max_query_bytes = 1048576
max_update_bytes = 1048576
max_rdf_upload_bytes = 10485760

[policy.rate_limits]
window_secs = 60
read_requests_per_window = 0
write_requests_per_window = 0
admin_requests_per_window = 0

[policy.timeouts]
query_ms = 30000
update_ms = 60000
graph_read_ms = 30000
graph_write_ms = 60000

[policy.exposure]
operator_ui = true
metrics = true

[auth]
mode = "bearer-jwt"

[auth.bearer_jwt]
shared_secret = "replace-me"
issuer = "nrese"
audience = "nrese-api"
read_role = "nrese.read"
admin_role = "nrese.admin"
leeway_seconds = 30

[ai]
enabled = true
provider = "gemini"
model = "gemini-2.5-flash"
timeout_ms = 15000
max_suggestions = 3
system_prompt = "Generate practical SPARQL suggestions."

[ai.gemini]
api_key = "replace-me"
```

## Server

- file key: `server.bind_address`
- env override: `NRESE_BIND_ADDR`
- default: `127.0.0.1:8080`

## Store

- file key: `store.mode`
- env override: `NRESE_STORE_MODE`
- values: `in-memory`, `on-disk`

- file key: `store.data_dir`
- env override: `NRESE_DATA_DIR`
- default: `./data`

- file key: `store.ontology_path`
- env override: `NRESE_ONTOLOGY_PATH`
- optional explicit preload path

## Reasoner

- file key: `reasoner.mode`
- env override: `NRESE_REASONING_MODE`
- values: `disabled`, `rules-mvp`, `owl-dl-target`

- file key: `reasoner.rules_mvp.features`
- env override: `NRESE_REASONER_RULES_MVP_FEATURES`
- accepted values:
  - `default`
  - `all`
  - `none`
  - `rdfs-subclass-closure`
  - `rdfs-subproperty-closure`
  - `rdfs-type-propagation`
  - `rdfs-domain-range-typing`
  - `owl-property-assertion-closure`
  - `owl-equality-reasoning`
  - `owl-consistency-check`
  - `unsupported-diagnostics`
- file format:
  - string: `features = "default"`
  - list: `features = ["rdfs-subclass-closure", "owl-consistency-check"]`

- file key: `reasoner.rules_mvp.preset`
- env override: `NRESE_REASONER_RULES_MVP_PRESET`
- values:
  - `rdfs-core`
  - `bounded-owl`
- precedence:
  - explicit `features` override `preset`
  - `preset` overrides the built-in default

## Policy Limits

- `policy.limits.max_query_bytes` -> `NRESE_MAX_QUERY_BYTES`
- `policy.limits.max_update_bytes` -> `NRESE_MAX_UPDATE_BYTES`
- `policy.limits.max_rdf_upload_bytes` -> `NRESE_MAX_RDF_UPLOAD_BYTES`

## Policy Rate Limits

- `policy.rate_limits.window_secs` -> `NRESE_RATE_LIMIT_WINDOW_SECS`
- `policy.rate_limits.read_requests_per_window` -> `NRESE_READ_REQUESTS_PER_WINDOW`
- `policy.rate_limits.write_requests_per_window` -> `NRESE_WRITE_REQUESTS_PER_WINDOW`
- `policy.rate_limits.admin_requests_per_window` -> `NRESE_ADMIN_REQUESTS_PER_WINDOW`

## Policy Timeouts

- `policy.timeouts.query_ms` -> `NRESE_QUERY_TIMEOUT_MS`
- `policy.timeouts.update_ms` -> `NRESE_UPDATE_TIMEOUT_MS`
- `policy.timeouts.graph_read_ms` -> `NRESE_GRAPH_READ_TIMEOUT_MS`
- `policy.timeouts.graph_write_ms` -> `NRESE_GRAPH_WRITE_TIMEOUT_MS`

## Endpoint Exposure

- `policy.exposure.operator_ui` -> `NRESE_ENABLE_OPERATOR_UI`
- `policy.exposure.metrics` -> `NRESE_ENABLE_METRICS`

## Auth Modes

- file key: `auth.mode`
- env override: `NRESE_AUTH_MODE`
- values: `none`, `bearer-static`, `bearer-jwt`, `mtls`, `oidc-introspection`

### Static Bearer

- `auth.bearer_static.read_token` -> `NRESE_AUTH_READ_TOKEN`
- `auth.bearer_static.admin_token` -> `NRESE_AUTH_ADMIN_TOKEN`

### JWT Bearer

- `auth.bearer_jwt.shared_secret` -> `NRESE_AUTH_JWT_SECRET`
- `auth.bearer_jwt.issuer` -> `NRESE_AUTH_JWT_ISSUER`
- `auth.bearer_jwt.audience` -> `NRESE_AUTH_JWT_AUDIENCE`
- `auth.bearer_jwt.read_role` -> `NRESE_AUTH_JWT_READ_ROLE`
- `auth.bearer_jwt.admin_role` -> `NRESE_AUTH_JWT_ADMIN_ROLE`
- `auth.bearer_jwt.leeway_seconds` -> `NRESE_AUTH_JWT_LEEWAY_SECS`

### Proxy-Terminated mTLS

- `auth.mtls.subject_header` -> `NRESE_AUTH_MTLS_SUBJECT_HEADER`
- `auth.mtls.read_subjects` -> `NRESE_AUTH_MTLS_READ_SUBJECTS`
- `auth.mtls.admin_subjects` -> `NRESE_AUTH_MTLS_ADMIN_SUBJECTS`
- file format:
  - string: `admin_subjects = "CN=admin,O=Example"`
  - list: `admin_subjects = ["CN=admin-1,O=Example", "CN=admin-2,O=Example"]`

### OIDC Introspection

- `auth.oidc_introspection.introspection_url` -> `NRESE_AUTH_OIDC_INTROSPECTION_URL`
- `auth.oidc_introspection.client_id` -> `NRESE_AUTH_OIDC_CLIENT_ID`
- `auth.oidc_introspection.client_secret` -> `NRESE_AUTH_OIDC_CLIENT_SECRET`
- `auth.oidc_introspection.read_role` -> `NRESE_AUTH_OIDC_READ_ROLE`
- `auth.oidc_introspection.admin_role` -> `NRESE_AUTH_OIDC_ADMIN_ROLE`
- `auth.oidc_introspection.timeout_ms` -> `NRESE_AUTH_OIDC_TIMEOUT_MS`

## AI Query Suggestions

- file key: `ai.enabled`
- env override: `NRESE_AI_ENABLED`
- default: `false`

- file key: `ai.provider`
- env override: `NRESE_AI_PROVIDER`
- values: `disabled`, `gemini`, `openrouter`

- file key: `ai.model`
- env override: `NRESE_AI_MODEL`

- file key: `ai.timeout_ms`
- env override: `NRESE_AI_TIMEOUT_MS`

- file key: `ai.max_suggestions`
- env override: `NRESE_AI_MAX_SUGGESTIONS`

- file key: `ai.system_prompt`
- env override: `NRESE_AI_SYSTEM_PROMPT`

### Gemini

- `ai.gemini.api_key` -> `NRESE_AI_GOOGLE_API_KEY`
- `ai.gemini.api_base` -> `NRESE_AI_GOOGLE_API_BASE`
- `GOOGLE_API_KEY` is accepted as a fallback when `NRESE_AI_GOOGLE_API_KEY` is not set

### OpenRouter

- `ai.openrouter.api_key` -> `NRESE_AI_OPENROUTER_API_KEY`
- `ai.openrouter.api_base` -> `NRESE_AI_OPENROUTER_API_BASE`
- `ai.openrouter.site_url` -> `NRESE_AI_OPENROUTER_SITE_URL`
- `ai.openrouter.app_name` -> `NRESE_AI_OPENROUTER_APP_NAME`

## Ownership Rules

- typed runtime defaults and validation live in owning crates
- external config parsing and precedence live in `crates/nrese-server/src/config/`
- env variable names are centralized in `crates/nrese-server/src/config/env_names.rs`
- do not document runtime knobs in multiple operator docs
