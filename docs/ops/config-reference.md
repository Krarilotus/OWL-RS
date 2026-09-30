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
- External reasoner input may still be expressed as `mode`, `preset`, and optional feature overrides, but the server resolves those inputs into one runtime profile/tier contract before diagnostics, capabilities, and frontend surfaces see them.

## Validation

- Unknown keys in the TOML file are a startup error that names the key (for example a misspelt `mdoe`). Known aliases such as `server.bind_addr` still work.
- `nrese-server check-config [--config FILE]` loads and validates the configuration (file, then environment overrides), prints the effective settings and exits. Credentials are never printed: authentication and AI show their mode and provider only. `reasoner.semantics` shows the ruleset and its semantic fingerprint (the stored inferences are rebuilt when it changes).

## Minimal Example

```toml
[server]
bind_address = "127.0.0.1:8080"
deployment_posture = "internal-authenticated"

[store]
mode = "in-memory"
data_dir = "./data"
ontology_path = "C:/data/rg_ontology.ttl"

[reasoner]
mode = "owl2-rl"

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

[policy]
sparql_parse_error_profile = "problem-json"

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

- file key: `server.deployment_posture`
- env override: `NRESE_DEPLOYMENT_POSTURE`
- values:
  - `open-workbench`
  - `read-only-demo`
  - `internal-authenticated`
  - `replacement-grade`
- default: `open-workbench`
- posture effects:
  - `read-only-demo` disables SPARQL Update, `TELL`, Graph Store writes, and admin mutation surfaces
  - `internal-authenticated` requires auth mode other than `none`
  - `replacement-grade` additionally requires on-disk storage and `problem-json` SPARQL parse errors

## Store

- file key: `store.mode`
- env override: `NRESE_STORE_MODE`
- values: `in-memory` (aliases `inmemory`, `memory`), `on-disk` (aliases `ondisk`, `disk`, `durable`)
- unknown values are a startup error

- file key: `store.data_dir`
- env override: `NRESE_DATA_DIR`
- default: `./data`

- file key: `store.default_graph`
- env override: `NRESE_DEFAULT_GRAPH`
- values: `default` (the default), `union`
- what a query, or an update's `WHERE`, reads when it names no dataset:
  - `default`: only the default graph, as the SPARQL specification describes a plain dataset;
  - `union`: the merge of all graphs, as GraphDB, RDF4J stores and Blazegraph do. A statement counts once, however many graphs hold it.
- use `union` for clients that write into named graphs and query without naming them (ResearchSpace, the Datamodel Workflow's exports)
- `GRAPH` patterns, `FROM` clauses and the protocol's dataset parameters mean the same in both modes
- an update without `GRAPH` still writes to, and deletes from, the default graph only
- cost: `union` runs on the native executor like `default`, but reads graph-last index orders and drops repeated statements, and the shortcuts that answer from index counts alone (`COUNT(*)` of one pattern, range filters, the worst-case-optimal join) don't apply. How much slower that is hasn't been measured yet
- `/version` reports the mode as `default_graph`

- file key: `shacl.shapes_graph`
- env override: `NRESE_SHACL_SHAPES_GRAPH`
- default: `http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` (the graph RDF4J and GraphDB use)
- the graph whose shapes `GET /dataset/shacl` validates against ([http-api.md](http-api.md)); it must be an IRI

- file key: `store.query_cache_bytes`
- env override: `NRESE_QUERY_CACHE_BYTES`
- default: `67108864` (64 MiB); `0` disables the cache
- serialised results of repeated queries on an unchanged store are answered from memory. Every commit starts a new revision, so a cached result is never stale; queries using `NOW()`, `RAND()`, `UUID()`, `STRUUID()` or `BNODE()` are not cached, and no single result takes more than an eighth of the budget

- file key: `store.ontology_path`
- env override: `NRESE_ONTOLOGY_PATH`
- optional; when set, the file is loaded at startup and a missing file is a startup error
- when unset, nothing is preloaded (there are no implicit fallback locations)

## Reasoner

- file key: `reasoner.mode`
- env override: `NRESE_REASONING_MODE`
- values:
  - `disabled` (aliases `none`, `off`): no reasoning; reads see asserted statements only
  - `rdfs`: the RDFS closure is materialised into the inferred stack
  - `owl2-rl`: the OWL 2 RL/RDF closure is materialised, and commits that violate a consistency rule are rejected with the rule and the facts it matched
- unknown values are a startup error (a typo must not silently disable consistency checking); `rules-mvp`, the removed v1 reasoner, is an error that names its replacement
- with `rdfs` or `owl2-rl`:
  - `nrese-server load` and startup materialise the closure (startup skips it when `reasoning.state` in the data directory records that the inferred stack is current for this build's semantics; `/version` reports them as `reasoning_semantics`, e.g. `owl2-rl v2 <fingerprint>`)
  - what each mode derives and omits: [reasoning-semantics.md](../spec/reasoning-semantics.md)
  - every commit maintains it incrementally, inside the same transaction
  - queries see asserted and inferred statements by default; `infer=false` or `FROM <http://www.ontotext.com/explicit>` reads asserted statements only, `FROM <http://www.ontotext.com/implicit>` inferred ones
- switching reasoning off clears the inferred stack at the next startup

## Policy Limits

- `policy.limits.max_query_bytes` -> `NRESE_MAX_QUERY_BYTES`
- `policy.limits.max_query_memory_bytes` -> `NRESE_MAX_QUERY_MEMORY_BYTES`: bytes of intermediate results one query may hold (default 4 GiB, `0` = unlimited). A query that needs more is rejected with `413` instead of growing the server's memory.
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

A write that times out before its commit starts is never committed, and the request gets 408. The deadline reaches every phase of the write:
- the `WHERE` evaluation of an update;
- commit-path reasoning, polled between rounds and per work unit.

A cancelled reasoning run discards the asserted and the inferred changes and frees the writer at once. Two steps don't poll the deadline:
- the one-off full materialisation that runs when no current reasoning state is recorded;
- closing a newly declared transitive property.

## SPARQL Parse Error Profile

- `policy.sparql_parse_error_profile` -> `NRESE_SPARQL_PARSE_ERROR_PROFILE`
- values:
  - `problem-json`
  - `plain-text`
  - `fuseki-plain-text`
- default: `problem-json`

Use `fuseki-plain-text` when you want local or live parity runs to match Fuseki-style plain-text parse errors for invalid SPARQL syntax while keeping the rest of the API on the normal problem+json path.

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
