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

Every setting has a key in the configuration file and an environment variable (the lists below; the server's registry of them is `crates/nrese-server/src/config/settings.rs`). A value is taken from the first of:

1. the command line: `--set KEY=VALUE` with a file key (`--set budgets.query_timeout=2min`), repeatable, for every command; secrets (tokens, keys, passwords) are refused there, since a command line shows in the process list
2. the environment variable
3. the configuration file: the path from `--config` or `-c`, else `NRESE_CONFIG_PATH`
4. the built-in default

Notes:

- Environment variables always override values loaded from `config.toml`.
- If no config file path is provided by CLI or `NRESE_CONFIG_PATH`, the server runs from env/defaults only.
- External reasoner input may still be expressed as `mode`, `preset`, and optional feature overrides, but the server resolves those inputs into one runtime profile/tier contract before diagnostics, capabilities, and frontend surfaces see them.

## Validation

- Unknown keys in the TOML file are a startup error that names the key (for example a misspelt `mdoe`). Known aliases such as `server.bind_addr` still work.
- `nrese-server config-schema` prints the JSON Schema of the configuration file (2020-12): every key with its type, allowed values, description, environment variable (`x-env`), older keys (`x-older-keys`), and `writeOnly` for credentials. Editors and the console can validate and complete `config.toml` with it.
- A value of the wrong kind (text for a flag, a negative number) is a startup error that names the key.
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

- file key: `store.geosparql_relations`
- env override: `NRESE_GEOSPARQL_RELATIONS`
- values: `computed` (the default), `stated`
- what a GeoSPARQL relation in a triple pattern (`?building geo:sfWithin ex:Berlin`) matches:
  - `computed`: the statements that assert it and the pairs whose geometries stand in it (GeoSPARQL's query-rewrite extension);
  - `stated`: the asserted statements only (the topology vocabulary without the rewrite), for data whose relations are curated, or where computing them from geometries is unwanted
- the filter functions (`geof:sfWithin(?a, ?b)`) compute from geometries in both modes

- file key: `shacl.shapes_graph`
- env override: `NRESE_SHACL_SHAPES_GRAPH`
- default: `http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` (the graph RDF4J and GraphDB use)
- the graph whose shapes `GET /dataset/shacl` validates against ([http-api.md](http-api.md)); it must be an IRI

- file key: `store.query_cache_bytes`
- env override: `NRESE_QUERY_CACHE_BYTES`
- default: 2% of the memory the server may use (the container's limit, else the machine's), at least 64 MiB and at most 8 GiB; a size, or a share such as `5%`; `0` disables the cache
- serialised results of repeated queries on an unchanged store are answered from memory. Every commit starts a new revision, so a cached result is never stale; queries using `NOW()`, `RAND()`, `UUID()`, `STRUUID()` or `BNODE()` are not cached, and no single result takes more than a quarter of the budget

- file keys: `shacl.gate`, `shacl.gate_severity`
- env overrides: `NRESE_SHACL_GATE`, `NRESE_SHACL_GATE_SEVERITY`
- default: `off`; severity `violation`
- SHACL as a commit gate: `report` validates every commit against the shapes graph (`shacl.shapes_graph`) and logs what it introduces; `enforce` also rejects a commit that introduces a result at or above `gate_severity` (`violation`, `warning` or `info`), or a validation failure, and the commit changes nothing. Only what a commit introduces counts: data that was invalid before doesn't block writes (validation at `/dataset/shacl` reports it). The check validates the focus nodes the commit can affect, after reasoning; a commit that changes the shapes graph is validated in full

- file key: `store.import_directory`
- env override: `NRESE_IMPORT_DIR`
- default: none
- the directory administrators import files from by name (`POST /api/v1/repositories/{id}/import/files`): large loads without uploading them, as jobs. Paths can't leave it. Without it, server-side imports don't exist

- file key: `store.verify_on_open`
- env override: `NRESE_VERIFY_ON_OPEN`
- default: `false`
- on disk, the newest checkpoint is used in place from a memory map: opening reads only its structure, so a restart takes milliseconds and memory grows with what queries touch. `true` checks the whole checkpoint when opening (its CRC, every index block, every dictionary key): opening then reads the whole file once, and a damaged checkpoint is refused at once instead of failing when a query reaches the damage

- file key: `store.map_checkpoints`
- env override: `NRESE_MAP_CHECKPOINTS`
- default: `true`
- on disk, once a checkpoint is written (after a bulk load, and by background or explicit checkpoints) the data it holds is served from the file, mapped, as after a restart, and its copies in memory are freed: memory then grows with what queries touch, and the OS can page out the rest. `false` keeps everything in memory as well (the file is still written)

- file key: `store.wal_archive`
- env override: `NRESE_WAL_ARCHIVE`
- default: `false`
- on disk, keep the WAL segments that checkpoints cover in `wal-archive/` of the data directory instead of deleting them. With an image backup they restore the store to any later revision (`nrese-server restore DIR --wal ARCHIVE --wal DATA/wal --until-revision N`). The archive grows until pruned (`nrese-server prune-archive R`, with `R` the oldest image backup's revision plus one). Sync it to another machine for recovery from a lost disk

- file key: `store.index_encoding`
- env override: `NRESE_INDEX_ENCODING`
- default: `fast`
- how index blocks are encoded when they are built (loads, compactions, checkpoints): `fast` (frame of reference only, the fastest scans) or `compact` (a block position with few, far-apart values is stored as a palette of them; on the office PC DBpedia core's store is 3.3 % smaller with queries 7 % slower, Wikidata lexemes' 8.6 % smaller with queries even). Both are read either way, so switching needs no reload: blocks built later take the new encoding. Applies to every repository of the server. Checkpoints are format 11 (block offsets for the dictionary), which binaries before 2 October 2026 don't read; they read formats 8 to 10

- file key: `store.vocabulary`
- env override: `NRESE_VOCABULARY`
- default: `plain`
- how checkpoints store the dictionary's keys: `plain`, or `fsst` (compressed with two FSST symbol tables, one for IRIs and one for the other terms, trained on a sample when the checkpoint is written). On the office PC's stores the keys shrink to 39 % (Wikidata lexemes, 576 → 224 MiB), 45 % (DBpedia core) and 53 % (YAGO); a key read from a compressed checkpoint costs a decode (about 17 ns in order, more at random), so result-heavy and string-scanning queries pay for it. Terms interned since the last checkpoint stay plain until the next. Both forms are read whatever the setting: switching takes effect at the next checkpoint

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
  - `owl2-dl`: OWL 2 DL ([design](../design/owl2-dl.md) §8). The OWL 2 RL closure is materialised as the lower bound, as with `owl2-rl`, and the DL engines are added: commits checked for consistency under OWL 2 DL, the upper bound, and a completeness status on every answer (the `dl.*` settings below)
  - `custom`: the user's rules only (`reasoner.rules`, below)
- file key `reasoner.rules`, env `NRESE_REASONING_RULES`: a Notation3 file (`.n3`) or a GraphDB ruleset (`.pie`) of user rules. With `custom` they are the whole program; with any other reasoning mode they are added to its ruleset. They are compiled at startup, and a rule the reasoner can't run stops the startup with the rule and the reason. What compiles:
  - `{ premises } => { conclusions } .` (and `<=`), with quick variables `?x`; blank nodes in the premises are variables
  - `{ premises } => false .`: a consistency rule; commits that make it hold are rejected, naming it (`rules.n3#4`)
  - `log:notEqualTo` and `log:equalTo` in the premises
  - plain triples outside formulas: facts that hold whatever the data
  - not (yet): other builtins (`math:`, `string:`, `list:`, `time:`), blank nodes in conclusions (existentials), formulas as terms
  - a GraphDB ruleset (`.pie`) is read too: prefixes, axioms, `Id:` rules with `[Constraint a != b]` (and `!= blank_node`), `Consistency:` checks, `[Context <g>]` on statements with a constant predicate (their statements get a predicate of their own, `urn:nrese:pie-context:<g>#<p>`, and are in the inferred stack, where GraphDB hides them); `[Cut]` is ignored. GraphDB's RDFS, RDFS-Plus, OWL-Horst and Publishing rulesets (and their optimised variants) compile; its OWL 2 RL and QL ones don't (a context on a variable predicate, existential conclusions), and NRESE's own `owl2-rl` and `owl2-ql` modes cover those profiles
  - a different rules file (one character is enough) makes the recorded closure stale, so the next startup rematerialises
- unknown values are a startup error (a typo must not silently disable consistency checking); `rules-mvp`, the removed v1 reasoner, is an error that names its replacement
- file key `reasoner.equality`, env `NRESE_REASONING_EQUALITY`: `representatives` (the default), `compact` or `replicate`. With a ruleset that reasons with equality (`owl:sameAs`: `owl-horst`, `owl2-rl`), a full materialisation (load, rematerialisation) computes the closure over one representative per `owl:sameAs` class and expands it to every identity, instead of the replacement rules copying every fact to every identity while the closure is computed. The stored closure is the same either way (W4 stage A; a property test checks it on random ontologies); on the integration workload's cohort tier the closure took 0.21 s instead of 3.09 s. Commits maintain it incrementally as before
- file key `reasoner.equality_answers`, env `NRESE_REASONING_EQUALITY_ANSWERS`: `strict` (the default) or `canonical`. With `reasoner.equality = "compact"`: `strict` answers with every identity of each `owl:sameAs` class, as if the closure were replicated; `canonical` with one identity per class, its representative (the smallest id), for analytics where the identities are one thing. Constants of a query keep their own names either way (work package W4 stage C)
- file key `reasoner.equality_expansion`, env `NRESE_REASONING_EQUALITY_EXPANSION`: `late` (the default) or `early`. With `reasoner.equality = "compact"` and strict answers: `late` evaluates the parts of a query whose answer can't tell identities apart (basic graph patterns, joins, OPTIONAL without a condition, UNION) over representatives and expands each solution once, instead of every read expanding every statement to every identity before the joins (`early`, stage B). The answers are the same (a differential test against `early` and against replication checks it); FILTER, BIND, VALUES, MINUS, property paths, named graphs and the operators that count or order stay expanded early. It applies where the materialised model is read and no named graph holds statements; EXPLAIN shows it as `late equality expansion`
  - `compact` (W4 stage B) also keeps the closure over representatives in the store: one copy of each fact per class instead of one per combination of identities, and each identity stored as `identity owl:sameAs representative`. Reads of the default graph expand it to every identity, so queries answer as with the other modes (a differential test compares all three after inserts, deletions, merges and splits of classes, and named graphs). The trade-off: storage and commits shrink by the replication factor; scans of facts about identities pay the expansion, and the engine's columnar, range and group-count fast paths step aside while any class exists. A commit that merges or splits classes recomputes the closure after it. Changing the mode recomputes the closure at the next start.
- file key `reasoner.unnamed_classes`, env `NRESE_REASONING_UNNAMED_CLASSES`: `derive` (the default, OWL 2 RL as written) or `skip`: memberships in unnamed union classes that nothing consumes (an anonymous union used only as a domain or range, as the GND ontology does) are not derived. Everything else stays; members of such a class declared `owl:Class` are still `owl:Thing`s. With the GND ontology on the integration workload's example tier: 219,840 inferred statements become 99,213, with the same answers to its questions. A commit that makes such a class used (a subclass axiom on it) triggers a rematerialisation
- file key `reasoner.ql_rewriting`, env `NRESE_REASONING_QL_REWRITING`: `auto` (the default), `on` or `off`; a repository's settings may choose their own (`ql_rewriting`), changed at once. `auto` rewrites while the inferred stack is current under `owl2-ql`; `on` under `owl2-rl` too (not the default there: `owl2-rl` keeps its standard semantics, and answer counts equal other systems'); `off` never; never under `owl2-dl`, whose bounds give those answers with their status. Where it applies, queries also get the OWL 2 QL answers that go through individuals an existential axiom implies (`Employee ⊑ ∃worksFor.Organisation`: `SELECT ?x { ?x :worksFor [] }` finds every employee), by tree-witness rewriting of their basic graph patterns over the closure ([design](../design/ql-rewriting.md)). It applies to queries over the default graph that read the inferred statements, without a dataset of their own; under graph access, a reader who sees only supported inferences gets the existentials of its readable graphs, one who sees every inference all of them, one who sees none no rewriting. A variable counts as existential when the query doesn't read it outside its pattern. The rewriting of a pattern is bounded in size and in work (50,000 steps); past a bound the pattern runs as written and the answer says `sound-only`. Bag queries keep their materialised rows and get each further answer once. EXPLAIN lists `ql-tree-witness` (or `ql-limit` when a pattern was too large to rewrite and ran as written) and reports the rewriting under `ql`. Incompleteness is never silent: where an axiom outside OWL 2 QL (transitivity, chains, functional properties, keys, cardinalities, `allValuesFrom`, `hasValue`, RL constructs on the left) meets the anonymous individuals, or a pattern reached a bound or runs without the rewriting, the answer says `sound-only` with the reasons (the `NRESE-Completeness` header, EXPLAIN's `ql.completeness`); otherwise `complete`. A schema without existentials on the right changes no query
- file key `reasoner.support_sets`, env `NRESE_REASONING_SUPPORT_SETS` (default 16, at least 1): the support graph sets kept per inferred statement for `inferred = "supported"` in the access policy, the smallest first (graphs in IRI order). A set left out can only hide a statement from a user who could have seen it, never show one; the log warns when statements reach the cap
- with `reasoner.mode = "owl2-dl"` (the [`owl2-dl` mode](../spec/reasoning-semantics.md)):
  - file key `dl.answers`, env `NRESE_DL_ANSWERS`: `certain-where-complete` (the default: the certain answers where the store can prove them complete, else the sound ones, the status saying which), `sound` (the lower bound alone, never checked against the upper bound) or `exact` (certain answers, or the query fails as `incomplete`)
  - file key `dl.consistency`, env `NRESE_DL_CONSISTENCY`: `inline` (the default: a commit that makes the data inconsistent under OWL 2 DL is rejected, as the RL gate rejects) or `off` (not checked; every answer's status then says the consistency is unknown)
  - file key `dl.timeout`, env `NRESE_DL_TIMEOUT_MS` (default 30min): the time one DL task may take (a commit's consistency check, a query's exact services, a classification); what isn't decided by then is reported, never guessed
  - file key `dl.memory`, env `NRESE_DL_MEMORY` (default 4GiB): the memory one DL task may hold
  - file key `dl.max_candidates`, env `NRESE_DL_MAX_CANDIDATES` (default 1000): the candidate answers (in the upper bound, not the lower) a query checks with the exact services at most; the rest are reported unresolved
  - file key `dl.threads`, env `NRESE_DL_THREADS` (default 0, every core): the workers of a classification
  - file key `dl.max_nodes`, env `NRESE_DL_MAX_NODES` (default 2000000), and `dl.max_branch_points`, env `NRESE_DL_MAX_BRANCH_POINTS` (default 1000000): the deterministic budgets of one hypertableau run (a consistency check, an entailment test, a class test), beside `dl.timeout`. A run past a budget decides nothing: its candidate stays unresolved, its class untested, its commit's status `unknown`, never a wrong answer
- with any mode but `disabled`:
  - `nrese-server load` and startup materialise the closure (startup skips it when `reasoning.state` in the data directory records that the inferred stack is current for this build's semantics; `/version` reports them as `reasoning_semantics`, e.g. `owl2-rl v2 <fingerprint>`)
  - what each mode derives and omits: [reasoning-semantics.md](../spec/reasoning-semantics.md)
  - every commit maintains it incrementally, inside the same transaction
  - queries see asserted and inferred statements by default; `infer=false` or `FROM <http://www.ontotext.com/explicit>` reads asserted statements only, `FROM <http://www.ontotext.com/implicit>` inferred ones
- switching reasoning off clears the inferred stack at the next startup

## Replication

Read replicas ([replication.md](replication.md)); both sides need an on-disk store.

- file key `replication.mode`, env `NRESE_REPLICATION_MODE`: `off` (the default), `primary` (serve the log and an image to administrators) or `replica` (start from the primary's image when the data directory holds no store, follow its log, take no writes, don't reason)
- file key `replication.primary`, env `NRESE_REPLICATION_PRIMARY`: a replica's primary, its base URL
- file key `replication.token`, env `NRESE_REPLICATION_TOKEN`: the bearer token a replica presents to its primary (an administrator's); a secret, never printed
- file key `replication.poll`, env `NRESE_REPLICATION_POLL_MS`: how long a replica waits before asking again once caught up (default `500ms`)
- file key `replication.batch_bytes`, env `NRESE_REPLICATION_BATCH_BYTES`: about how much log a replica asks for at once (default `8MiB`)

## Federation (`SERVICE`)

- file table `[federation]`
- `federation.allow` (env `NRESE_FEDERATION_ALLOW`, comma-separated): the endpoints `SERVICE` may call, as full IRIs or prefixes (`https://query.wikidata.org/`), or `*` for any. Empty (the default): `SERVICE` is off, an error, and `SERVICE SILENT` one solution without bindings. Allowing `*` lets anyone who may query make the server fetch any URL
- `federation.timeout` (env `NRESE_FEDERATION_TIMEOUT_MS`, units as in [Budgets](#budgets)): per request to an endpoint; default `30s`
- `federation.max_rows` (env `NRESE_FEDERATION_MAX_ROWS`): rows one request may return; default 1,000,000
- a `SERVICE` joined to a pattern sends the pattern's distinct values as `VALUES`, 200 rows per request (up to 20,000 values; beyond, the block goes once, unbound); redirects are not followed; queries with `SERVICE` are never answered from the result cache

## Budgets

Every limit on memory, time and request size is in one table, `[budgets]`. Values are plain numbers (bytes, milliseconds) or numbers with a unit: `"4GiB"`, `"512MiB"`, `"2GB"`, `"30s"`, `"2min"`, and for memory a share of the machine, `"50%"`. `nrese-server check-config` prints the values in effect, and `/version` reports them under `budgets`.

| Key | Environment | Default | What it bounds |
|---|---|---|---|
| `budgets.query_memory` | `NRESE_MAX_QUERY_MEMORY_BYTES` | 4 GiB | Intermediate results of one query. A query that needs more is answered `413`. `0` = unlimited |
| `budgets.total_query_memory` | `NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES` | 50 % of the machine's memory | Intermediate results of all running queries together. A query that asks for more than is left is answered `503` and may succeed later. `0` = unlimited |
| `budgets.process_memory` | `NRESE_PROCESS_MEMORY_BYTES` | 75 % of the machine's memory (the container's limit where there is one) | The private memory (mapped store files excluded) the server may hold before a long operation stops instead of taking the machine: a materialisation (load, startup, change of rules) or the reasoning of a commit ends with an error and applies nothing. A size (`48 GiB`) or a share (`60%`); `0` = unlimited |
| `budgets.bulk_load_memory` | `NRESE_BULK_LOAD_MEMORY` | 25 % of the machine's memory | The quads of a bulk load (`nrese-server load`, or a load into an empty store). Past it they are sorted in chunks of a third of it (one filling, one being sorted, its sorted copy), spilled to `bulk-spill/` in the data directory and merged into each index permutation while the checkpoint is written: one more pass over the disk, but the load's quads take this much plus one packed permutation whatever the data's size. The dictionary is apart (it grows with the distinct terms). Needs `store.map_checkpoints`. `0` = unlimited |
| `budgets.query_timeout` | `NRESE_QUERY_TIMEOUT_MS` | 30 s | A query, until its last result is sent (`408`) |
| `budgets.update_timeout` | `NRESE_UPDATE_TIMEOUT_MS` | 60 s | A SPARQL update, reasoning included |
| `budgets.graph_read_timeout` | `NRESE_GRAPH_READ_TIMEOUT_MS` | 30 s | A Graph Store read |
| `budgets.graph_write_timeout` | `NRESE_GRAPH_WRITE_TIMEOUT_MS` | 60 s | A Graph Store write |
| `budgets.query_text` | `NRESE_MAX_QUERY_BYTES` | 1 MiB | The text of a query |
| `budgets.update_size` | `NRESE_MAX_UPDATE_BYTES` | 16 MiB | A SPARQL update request |
| `budgets.upload_size` | `NRESE_MAX_RDF_UPLOAD_BYTES` | 128 MiB | An RDF payload (Graph Store, TELL, SHACL shapes); larger data goes through `nrese-server load` |
| `budgets.result_cache` | `NRESE_QUERY_CACHE_BYTES` | 2% of memory (64 MiB to 8 GiB) | Serialised results kept for repeated queries; a size or a share (`5%`). `0` switches the cache off |

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
- `auth.mtls.trusted_proxies` -> `NRESE_AUTH_MTLS_TRUSTED_PROXIES` (addresses or ranges such as `10.0.0.0/8`, comma-separated; default loopback): the peers that terminate TLS and may pass the subject header on. From any other peer the header is dropped before authentication, so a client that reaches the server's port directly can't claim a subject. With the proxy in another container or host, list its address or network here
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

### Graph-Level Access Control

The rules are the server's access state (users, workspaces, personal spaces, role rules; [ADR-0008](../adr/0008-users-workspaces-policies.md)), kept in `system/` of the data directory and changed through `/api/v1/access` ([HTTP API](http-api.md#users-workspaces-and-policies-apiv1access)). Until access is enforced, every user reads and writes what its authentication grants allow.

- `auth.access_policy` -> `NRESE_ACCESS_POLICY`: the path of an access policy file, imported into the access state at the first start (it turns enforcement on). Later changes of the file are reported at start and apply once imported (`POST /api/v1/access/import`); `GET /api/v1/access/export` writes the state's rules in this form.
- `auth.local_logins` -> `NRESE_AUTH_LOCAL_LOGINS` (default `true`): whether users of the access state with a password log in (`Basic` credentials, or a session from `POST /api/v1/access/login`) besides the authentication mode; for standalone and desktop installations without an identity provider.
- `auth.trusted_proxies` -> `NRESE_AUTH_TRUSTED_PROXIES` (addresses or ranges, comma-separated; default loopback): the reverse proxies whose `X-Forwarded-For` names the client. Failed local logins are counted per user name and client address (past 10, each further attempt waits, from one second doubling up to five minutes) and per client address over all names (past 100); behind a proxy that isn't listed here, every client has the proxy's address, so list it
- `auth.workspace_base` -> `NRESE_WORKSPACE_BASE`: what workspace graph prefixes start with (default `urn:nrese:`): a personal space is `{base}space/{user}/`, a workspace `{base}workspace/{name}/`.
- the file (TOML), per role:

```toml
default = "deny"        # roles no rule names: nothing ("deny") or everything ("allow")
inferred = "supported"  # inferred statements for users who may not read every graph: "hidden" (default), "visible" or "supported"

[[role]]
name = "analyst"
read = ["https://kg.example/graphs/public/*", "https://kg.example/graphs/sales"]
write = ["https://kg.example/graphs/analyst/*"]
deny = ["https://kg.example/graphs/public/hr/*"]
default_graph = "read"  # the store's default graph: "none" (default), "read", "write" or "deny"
service = true          # may call other endpoints with SERVICE (default false)
```

- inferred statements live in the default graph, so only users who may read it see any. `hidden` shows them none, `visible` all of them (derived from any graph), `supported` those one of whose derivations uses graphs the user may read alone: what the user could derive itself (support graph sets; reasoner v2 rulesets). The sets are computed on the first read that needs them (about four times the materialisation time: LUBM(10) over 8 graphs 2.4 s) and then updated with each commit for what it can affect; each user's view of a revision is built once, from the user's previous one (LUBM(10): a restricted read after a one-statement commit 2 ms, after a thousand 53 ms). Bulk loads, rematerialisations and schema changes compute them afresh. They are computed in the background at start and when the policy turns to `supported`, and after a rematerialisation while users read with them, so first reads find them ready or wait for that computation. With `reasoner.equality = "compact"` the sets aren't computed and such users see no inferred statements. `reasoner.support_sets` bounds the sets kept per statement
- `SERVICE` is a privilege of its own (it makes the server fetch URLs): with access enforced, a user calls the endpoints `federation.allow` lists only if one of its roles says `service = true` (administrators always). Without the privilege a `SERVICE` is an error, `SERVICE SILENT` one solution without bindings.
- graphs by IRI, or by IRI prefix (an entry ending in `*`): ontologies and data sets usually differ by prefix, so one entry covers a family of graphs.
- role names: a token's `scope`, `scp`, `role` and `roles` claims (`bearer-jwt`, `oidc-introspection`); `reader` for the static read token; the subject and `reader` for a listed client certificate; `anonymous` without authentication.
- user names: a token's `sub` (an introspection's `username` without one), a client certificate's subject, where they are letters, digits and `. _ @ + | : -`; a named user always has its personal space, and a user record can add roles or the administrator's right.
- a user's rights are the union of its roles' rules and its workspaces', and what it may write it may read; an explicit deny (`deny`, `default_graph = "deny"`) in any of its roles wins. Administrators are unrestricted.
- the policy grants as well as restricts: a role with a rule may query and read graphs even without the read role, and a role that may write a graph may update. So a role can edit some graphs without being an administrator.
- what it means for requests: [HTTP API, access control](http-api.md#graph-level-access-control).

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
- every setting (file key, older keys, environment name, kind, description) is declared once in `crates/nrese-server/src/config/settings.rs`; a test checks that this reference names every key and environment variable
- do not document runtime knobs in multiple operator docs
