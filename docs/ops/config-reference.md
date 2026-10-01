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

[budgets]
query_memory = "4GiB"
total_query_memory = "50%"
query_timeout = "30s"
update_timeout = "60s"
upload_size = "128MiB"

[policy.rate_limits]
window_secs = 60
read_requests_per_window = 0
write_requests_per_window = 0
admin_requests_per_window = 0

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

- file key: `store.verify_on_open`
- env override: `NRESE_VERIFY_ON_OPEN`
- default: `false`
- on disk, the newest checkpoint is used in place from a memory map: opening reads only its structure, so a restart takes milliseconds and memory grows with what queries touch. `true` checks the whole checkpoint when opening (its CRC, every index block, every dictionary key): opening then reads the whole file once, and a damaged checkpoint is refused at once instead of failing when a query reaches the damage

- file key: `store.ontology_path`
- env override: `NRESE_ONTOLOGY_PATH`
- optional; when set, the file is loaded at startup and a missing file is a startup error
- when unset, nothing is preloaded (there are no implicit fallback locations)

## Reasoner

- file key: `reasoner.mode`
- env override: `NRESE_REASONING_MODE`
- values:
  - `disabled` (aliases `none`, `off`): no reasoning; reads see asserted statements only
  - `rdfs`: the RDFS closure is materialised into the inferred stack (the six rules queries over data use)
  - `rdfs-full`: every RDFS entailment rule and the axiomatic triples
  - `rdfs-plus`: RDFS with equality, inverse, symmetric, transitive, functional and inverse functional properties, equivalent classes and properties
  - `owl-horst`: RDFS-Plus with `hasValue`, `someValuesFrom` and `allValuesFrom` (ter Horst's pD*)
  - `owl2-ql`: OWL 2 QL materialised; commits that violate its disjointness axioms are rejected
  - `owl2-rl`: the OWL 2 RL/RDF closure is materialised, and commits that violate a consistency rule are rejected with the rule and the facts it matched
  - `custom`: the user's rules only (`reasoner.rules`, below)
- file key `reasoner.rules`, env `NRESE_REASONING_RULES`: a Notation3 file (`.n3`) of user rules. With `custom` they are the whole program; with any other reasoning mode they are added to its ruleset. They are compiled at startup, and a rule the reasoner can't run stops the startup with the rule and the reason. What compiles:
  - `{ premises } => { conclusions } .` (and `<=`), with quick variables `?x`; blank nodes in the premises are variables
  - `{ premises } => false .`: a consistency rule; commits that make it hold are rejected, naming it (`rules.n3#4`)
  - `log:notEqualTo` and `log:equalTo` in the premises
  - plain triples outside formulas: facts that hold whatever the data
  - not (yet): other builtins (`math:`, `string:`, `list:`, `time:`), blank nodes in conclusions (existentials), formulas as terms
  - a different rules file (one character is enough) makes the recorded closure stale, so the next startup rematerialises
- unknown values are a startup error (a typo must not silently disable consistency checking); `rules-mvp`, the removed v1 reasoner, is an error that names its replacement
- file key `reasoner.unnamed_classes`, env `NRESE_REASONING_UNNAMED_CLASSES`: `derive` (the default, OWL 2 RL as written) or `skip`: memberships in unnamed union classes that nothing consumes (an anonymous union used only as a domain or range, as the GND ontology does) are not derived. Everything else stays; members of such a class declared `owl:Class` are still `owl:Thing`s. With the GND ontology on the integration workload's example tier: 219,840 inferred statements become 99,213, with the same answers to its questions. A commit that makes such a class used (a subclass axiom on it) triggers a rematerialisation
- with any mode but `disabled`:
  - `nrese-server load` and startup materialise the closure (startup skips it when `reasoning.state` in the data directory records that the inferred stack is current for this build's semantics; `/version` reports them as `reasoning_semantics`, e.g. `owl2-rl v2 <fingerprint>`)
  - what each mode derives and omits: [reasoning-semantics.md](../spec/reasoning-semantics.md)
  - every commit maintains it incrementally, inside the same transaction
  - queries see asserted and inferred statements by default; `infer=false` or `FROM <http://www.ontotext.com/explicit>` reads asserted statements only, `FROM <http://www.ontotext.com/implicit>` inferred ones
- switching reasoning off clears the inferred stack at the next startup

## Federation (`SERVICE`)

- file table `[federation]`
- `allow` (env `NRESE_FEDERATION_ALLOW`, comma-separated): the endpoints `SERVICE` may call, as full IRIs or prefixes (`https://query.wikidata.org/`), or `*` for any. Empty (the default): `SERVICE` is off, an error, and `SERVICE SILENT` one solution without bindings. Allowing `*` lets anyone who may query make the server fetch any URL
- `timeout` (env `NRESE_FEDERATION_TIMEOUT_MS`, units as in [Budgets](#budgets)): per request to an endpoint; default `30s`
- `max_rows` (env `NRESE_FEDERATION_MAX_ROWS`): rows one request may return; default 1,000,000
- a `SERVICE` joined to a pattern sends the pattern's distinct values as `VALUES`, 200 rows per request (up to 20,000 values; beyond, the block goes once, unbound); redirects are not followed; queries with `SERVICE` are never answered from the result cache

## Budgets

Every limit on memory, time and request size is in one table, `[budgets]`. Values are plain numbers (bytes, milliseconds) or numbers with a unit: `"4GiB"`, `"512MiB"`, `"2GB"`, `"30s"`, `"2min"`, and for memory a share of the machine, `"50%"`. `nrese-server check-config` prints the values in effect, and `/version` reports them under `budgets`.

| Key | Environment | Default | What it bounds |
|---|---|---|---|
| `budgets.query_memory` | `NRESE_MAX_QUERY_MEMORY_BYTES` | 4 GiB | Intermediate results of one query. A query that needs more is answered `413`. `0` = unlimited |
| `budgets.total_query_memory` | `NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES` | 50 % of the machine's memory | Intermediate results of all running queries together. A query that asks for more than is left is answered `503` and may succeed later. `0` = unlimited |
| `budgets.query_timeout` | `NRESE_QUERY_TIMEOUT_MS` | 30 s | A query, until its last result is sent (`408`) |
| `budgets.update_timeout` | `NRESE_UPDATE_TIMEOUT_MS` | 60 s | A SPARQL update, reasoning included |
| `budgets.graph_read_timeout` | `NRESE_GRAPH_READ_TIMEOUT_MS` | 30 s | A Graph Store read |
| `budgets.graph_write_timeout` | `NRESE_GRAPH_WRITE_TIMEOUT_MS` | 60 s | A Graph Store write |
| `budgets.query_text` | `NRESE_MAX_QUERY_BYTES` | 1 MiB | The text of a query |
| `budgets.update_size` | `NRESE_MAX_UPDATE_BYTES` | 16 MiB | A SPARQL update request |
| `budgets.upload_size` | `NRESE_MAX_RDF_UPLOAD_BYTES` | 128 MiB | An RDF payload (Graph Store, TELL, SHACL shapes); larger data goes through `nrese-server load` |
| `budgets.result_cache` | `NRESE_QUERY_CACHE_BYTES` | 64 MiB | Serialised results kept for repeated queries. `0` switches the cache off |

How the two memory budgets work:
- They count what queries hold between their operators: the tables of joins, groups and sorts, and the hash tables of joins. A table that is being built may take half of what is left, because growing it, and merging the parts of a parallel join, holds its rows twice for a moment.
- The machine's memory is the container's limit where there is one (cgroups), else the machine's. It is known on Linux; elsewhere a share such as `50%` means "no limit", so set a size.
- The store's own memory (the data and its indexes) is not part of either budget.
- A request over a size limit is answered `413`. Requests are held in memory while they are handled, so a size limit is also memory a request may take.

The same settings have older names, which still work: `policy.limits.max_query_bytes`, `max_query_memory_bytes`, `max_update_bytes`, `max_rdf_upload_bytes`; `policy.timeouts.query_ms`, `update_ms`, `graph_read_ms`, `graph_write_ms`; `store.query_cache_bytes`. A setting under both names is a startup error.

## Policy Rate Limits

- `policy.rate_limits.window_secs` -> `NRESE_RATE_LIMIT_WINDOW_SECS`
- `policy.rate_limits.read_requests_per_window` -> `NRESE_READ_REQUESTS_PER_WINDOW`
- `policy.rate_limits.write_requests_per_window` -> `NRESE_WRITE_REQUESTS_PER_WINDOW`
- `policy.rate_limits.admin_requests_per_window` -> `NRESE_ADMIN_REQUESTS_PER_WINDOW`

## Timeouts and writes

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
