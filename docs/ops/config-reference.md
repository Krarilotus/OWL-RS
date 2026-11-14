# NRESE Runtime Configuration Reference

## Purpose

This document is the operator-facing source of truth for `nrese-server` runtime configuration: every setting's file key, environment variable, type, default and scope, and the precedence between them. Each setting's row has an anchor named as its file key, so a link can name one setting: [`config-reference.md#store.mode`](#store.mode). Implementation ownership remains in `crates/nrese-server/src/config/`.

## Precedence

Every setting has a key in the configuration file and an environment variable (the tables below; the server's registry of them is `crates/nrese-server/src/config/settings.rs`). A value is taken from the first of:

1. the command line: `--set KEY=VALUE` with a file key (`--set budgets.query_timeout=2min`), repeatable, for every command; secrets (tokens, keys, passwords) are refused there, since a command line shows in the process list
2. the environment variable
3. the configuration file: the path from `--config` or `-c`, else `NRESE_CONFIG_PATH`
4. the built-in default

Notes:

- Environment variables always override values loaded from `config.toml`.
- If no config file path is provided by CLI or `NRESE_CONFIG_PATH`, the server runs from env/defaults only.
- **Reset:** to return a setting to its default, remove it from the command line, the environment and the file, then restart. A repository's own value (scope `repo` below) is removed with `PATCH /api/v1/repositories/{id}` and `null` ([HTTP API](http-api.md#engine-api-apiv1)).
- The reasoner's profile comes from `reasoner.mode` and `reasoner.rules` alone (there is no `preset` or feature-override input); the server resolves them into one runtime profile before diagnostics, capabilities and the console see it. The other `reasoner.*` settings below tune it.

### Types and scopes

The tables use these words.

| Type | What the value is |
|---|---|
| text | Any text |
| bool | `true` or `false`; the environment also takes `1`/`0`, `yes`/`no`, `on`/`off` |
| integer | A whole number, 0 or more, without a unit |
| size | Bytes: a number, or with a unit: `512MiB`, `4GiB`, `2GB` (`k`/`KiB`, `m`/`MiB`, `g`/`GiB`, `t`/`TiB`; `kb`, `mb`, `gb`, `tb` in powers of 1000) |
| size or share | A size, or a share of the memory the server may use: `50%` (0 % to 100 %) |
| duration | Milliseconds: a number, or with a unit: `500ms`, `30s`, `2min`, `1h` |
| limit | A size, duration or count as named, or `0` or `unlimited` for none |
| list | Text with the separator named, or in the file a TOML list of texts |
| choice | One of the words listed |

| Scope | When it takes effect |
|---|---|
| start | Read when the server or a command starts. Change it, then restart |
| repo | As start, and a repository's settings may set their own value, changed at once with `PATCH /api/v1/repositories/{id}` ([HTTP API](http-api.md#engine-api-apiv1)). The default repository's changed settings are kept in `repository.json` of the data directory and win over the configuration after a restart |
| process | As start, one value for the whole process and every repository ([below](#process-wide-settings)) |

Besides a repository's settings, only the result cache's pins and the access state have APIs that change them while the server runs (named where they are listed).

## Validation

- Unknown keys in the TOML file are a startup error that names the key (for example a misspelt `mdoe`). Known aliases such as `server.bind_addr` still work.
- `nrese-server config-schema` prints the JSON Schema of the configuration file (2020-12; of the commit it is built from): every key with its type, allowed values, description, environment variable (`x-env`), older keys (`x-older-keys`), and `writeOnly` for credentials. Editors and the console can validate and complete `config.toml` with it.
- The schema's allowed values are each choice's canonical names. The aliases a row names (such as `store.mode`'s `durable`) are accepted as well, from the file, the environment and `--set` alike; an editor that validates against the schema flags them.
- A value of the wrong kind (text for a flag, a negative number) is a startup error that names the key.
- `nrese-server check-config [--config FILE]` loads and validates the configuration (file, then environment overrides), prints the main effective settings and exits ([commands](server-setup.md#commands)). It doesn't print every setting: `budgets.process_memory`, `budgets.huge_pages`, `budgets.reasoning_release`, the `dl.*` settings and the `store.reader_*` settings are among those it leaves out. Credentials are never printed: authentication and AI show their mode and provider only. `reasoner.semantics` shows the ruleset and its semantic fingerprint (the stored inferences are rebuilt when it changes).

## Minimal Example

```toml
[server]
bind_address = "127.0.0.1:8080"
deployment_posture = "internal-authenticated"

[store]
mode = "in-memory"
data_dir = "./data"
ontology_path = "./ontology.ttl"

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

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="server.bind_address"></a>`server.bind_address` (older `server.bind_addr`) | `NRESE_BIND_ADDR` | text: `IP:PORT` | `127.0.0.1:8080` | start | The address and port the server listens on. Port `0`: the system picks one, and the start log names it. `0.0.0.0:8080` listens on every interface (put a TLS proxy in front: [server-setup.md](server-setup.md)) |
| <a id="server.deployment_posture"></a>`server.deployment_posture` | `NRESE_DEPLOYMENT_POSTURE` | choice: `open-workbench`, `read-only-demo`, `internal-authenticated`, `replacement-grade` | `open-workbench` | start | What the deployment is for (below) |

Posture effects:
- `read-only-demo` disables SPARQL Update, `TELL`, Graph Store writes, and admin mutation surfaces (they answer 404)
- `internal-authenticated` requires `auth.mode` other than `none`
- `replacement-grade` additionally requires on-disk storage and `problem-json` SPARQL parse errors
- a combination these rules refuse is a startup error

## Store

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="store.mode"></a>`store.mode` | `NRESE_STORE_MODE` | choice: `in-memory` (aliases `inmemory`, `memory`), `on-disk` (aliases `ondisk`, `disk`, `durable`) | `in-memory` | start | Where the store keeps its data; `in-memory` loses it at exit. Unknown values are a startup error |
| <a id="store.data_dir"></a>`store.data_dir` | `NRESE_DATA_DIR` | text: a path | `./data` | start | The data directory (on disk: the log, checkpoints, repositories; one process at a time, [locked](server-setup.md#err-data-directory-locked)) |
| <a id="store.ontology_path"></a>`store.ontology_path` | `NRESE_ONTOLOGY_PATH` | text: a path | none | start | A file loaded at startup; a missing file is a startup error. Unset: nothing is preloaded (no implicit fallback locations) |
| <a id="store.default_graph"></a>`store.default_graph` | `NRESE_DEFAULT_GRAPH` | choice: `default`, `union` | `default` | start | What a query that names no dataset reads (below) |
| <a id="store.geosparql_relations"></a>`store.geosparql_relations` | `NRESE_GEOSPARQL_RELATIONS` | choice: `computed`, `stated` | `computed` | start | What a GeoSPARQL relation in a triple pattern matches (below) |
| <a id="store.search_scope"></a>`store.search_scope` | `NRESE_SEARCH_SCOPE` | choice: `global`, `reader` | `global` | repo | What full-text relevance, IRI search and vector selection are computed over (below) |
| <a id="store.reader_corpus_bytes"></a>`store.reader_corpus_bytes` | `NRESE_READER_CORPUS_BYTES` | size | `256MiB` | repo | What the corpora of a `reader` search scope may keep (below) |
| <a id="store.reader_walk_slice"></a>`store.reader_walk_slice` | `NRESE_READER_WALK_SLICE` | integer, at least 1 | `1024` | start | Words of bits one slice of a corpus walk covers (below) |
| <a id="store.reader_documents_per_probe"></a>`store.reader_documents_per_probe` | `NRESE_READER_DOCUMENTS_PER_PROBE` | integer, at least 1 | `10` | start | When a `reader` corpus is derived instead of walked again (below) |
| <a id="store.import_directory"></a>`store.import_directory` | `NRESE_IMPORT_DIR` | text: a path | none | start | The directory administrators import files from by name (`POST /api/v1/repositories/{id}/import/files`): large loads without uploading them, as jobs. Paths can't leave it. Unset: no server-side imports |
| <a id="store.verify_on_open"></a>`store.verify_on_open` | `NRESE_VERIFY_ON_OPEN` | bool | `false` | start | Check the whole newest checkpoint when opening (below) |
| <a id="store.map_checkpoints"></a>`store.map_checkpoints` | `NRESE_MAP_CHECKPOINTS` | bool | `true` | start | Serve checkpointed data from the mapped file (below) |
| <a id="store.checkpoint_after_wal"></a>`store.checkpoint_after_wal` | `NRESE_CHECKPOINT_AFTER_WAL_BYTES` | size | `256MiB` | start | Write a checkpoint once the log has grown this much (below) |
| <a id="store.wal_archive"></a>`store.wal_archive` | `NRESE_WAL_ARCHIVE` | bool | `false` | start | Keep covered WAL segments for point-in-time restores (below) |
| <a id="store.wal_damage"></a>`store.wal_damage` | `NRESE_WAL_DAMAGE` | choice: `refuse`, `keep-prefix` | `refuse` | start | What opening does with damage in the newest log segment (below) |
| <a id="store.index_encoding"></a>`store.index_encoding` | `NRESE_INDEX_ENCODING` | choice: `fast`, `compact` | `fast` | process | How index blocks are encoded when built (below) |
| <a id="store.vocabulary"></a>`store.vocabulary` | `NRESE_VOCABULARY` | choice: `plain`, `fsst` | `plain` | process | How checkpoints store the dictionary's keys (below) |
| <a id="store.invalid_statements"></a>`store.invalid_statements` | `NRESE_INVALID_STATEMENTS` | choice: `quarantine`, `strict` | `quarantine` | start | What a bulk load does with a statement it can't read (below) |

The result cache (`budgets.result_cache`, older key `store.query_cache_bytes`) and its pins (`budgets.result_cache_pins`) are in [Budgets](#budgets), with their details [below](#the-result-cache).

Details:

- **`store.default_graph`**: what a query, or an update's `WHERE`, reads when it names no dataset:
  - `default`: only the default graph, as the SPARQL specification describes a plain dataset;
  - `union`: the merge of all graphs, as GraphDB, RDF4J stores and Blazegraph do. A statement counts once, however many graphs hold it.
  - use `union` for clients that write into named graphs and query without naming them (ResearchSpace, the Datamodel Workflow's exports)
  - `GRAPH` patterns, `FROM` clauses and the protocol's dataset parameters mean the same in both modes
  - an update without `GRAPH` still writes to, and deletes from, the default graph only
  - cost: `union` runs on the native executor like `default`, but reads graph-last index orders and drops repeated statements, and the shortcuts that answer from index counts alone (`COUNT(*)` of one pattern, range filters, the worst-case-optimal join) don't apply. How much slower that is hasn't been measured yet
  - `/version` reports the mode as `default_graph`
- **`store.geosparql_relations`**: what a GeoSPARQL relation in a triple pattern (`?building geo:sfWithin ex:Berlin`) matches:
  - `computed`: the statements that assert it and the pairs whose geometries stand in it (GeoSPARQL's query-rewrite extension);
  - `stated`: the asserted statements only (the topology vocabulary without the rewrite), for data whose relations are curated, or where computing them from geometries is unwanted
  - the filter functions (`geof:sfWithin(?a, ?b)`) compute from geometries in both modes
- **`store.search_scope`** (a repository's `search_scope` overrides it for that repository): what full-text relevance (`bds:search`, `text:query`, `luc:`), IRI autocompletion and vector selection (`nrv:search`) are computed over:
  - `global`: search uses the dictionary-wide text and IRI statistics and the existing vector-search strategy (the shared HNSW graph for large spaces). Returned data respects graph access, but relevance scores and approximate selection may depend on data outside the reader's graphs. The fastest.
  - `reader`: search ranks text and IRIs, and selects vectors, from the reader's snapshot, read model and active dataset only: a graph the reader may not read, an open transaction's terms, a later version or a term no statement uses changes no score, rank or selection. Vectors are compared exactly (recall 1) among the visible ones. Costs: the first search of a scope at a version walks its statements to build its corpus (kept, and after a small commit derived from the kept one by probing what the commit touched); a vector search scans the space's vectors, testing each for visibility. Admission may still depend on the dictionary's span, the index walk's scratch, the touched terms charged before the scope check and the shared corpus cache's retention (approved R2-F2 exception, [http-api.md](http-api.md)): a refusal can reveal hidden volume, a score can't.
  - measured cost of `reader` against `global`, per query once the corpus is kept, median of 6 passes a side; one A-B-B-A round, 3 blocks, on a Ryzen 9 5950X (fleet `abba-syn24-reader-a64451b3`):
    - full-text search over 1,000,000 short documents (`text-search`) shows no loss on four of its five series: 69.2 against 70.0 ms for a common word, 4.89 against 4.95 ms, 0.375 against 0.375 ms, and 99.7 against 100.4 ms for a prefix. The rarest word (8 against 8 µs) is unresolved at one round.
    - k-NN (k = 10) with a one-in-ten filter over 200,000 64-dimensional vectors (`vector-knn-filtered`) takes 3.7–4.0 ms against 2.4–2.6 ms, about 1.5×. `reader` compares every visible vector exactly; `global` searches the shared HNSW graph, which is approximate.
    - memory: no change shown in either case.
  - graph access, EXPLAIN and refusal sanitising are the same in both modes
- **`store.reader_corpus_bytes`** (a repository's `reader_corpus_bytes` overrides it): what the corpora a `reader` search scope keeps (one per scope and version, with the lineage that derives the next version's from it) may take, within the memory the queries draw on. Past it the least recently used go first; a corpus that still doesn't fit is built for its search alone, on that request's memory. This is the only bound on how many corpora and lineage steps are kept
  - why 256 MiB: a corpus of 1,000,000 documents takes 250,336 bytes, so about a thousand of that size fit (measured, fleet `syn24-lineage-probe-8faeb458`)
  - unused under `global`: nothing is allocated until a `reader` search runs
- **`store.reader_walk_slice`**: words of bits (64 terms each) one slice of a corpus walk covers at most; a slice also holds a group count of 16 bytes a term, so the default slice holds at most 1 MiB of counts. Smaller slices hold less scratch per thread, larger ones split the walk less. `0` is a startup error
  - why 1,024: a walk of 1,000,000 documents in slices of 256, 1,024 and 4,096 words does 111.7, 110.4 and 111.9 million instructions (fleet `syn24-work-probe-2-06961a4e`) and takes 20.8, 9.1 and 9.4 ms on 12 threads (fleet `syn24-tuning-probe-f4b4dd86`); 1,024 holds no more scratch than 4,096
- **`store.reader_documents_per_probe`**: after commits, a `reader` search derives its corpus from a kept ancestor's by probing the terms the commits touched, while there are at most the corpus's documents divided by this; past that it walks the scope's statements again. Larger values walk sooner, smaller ones probe longer. `0` is a startup error
  - why 10: measured in work (single-threaded instructions), so it holds whatever the core count. At 1,000,000 documents a probe does about 1,100 instructions and a walk about 110 million: they cost the same at about 100,000 touched terms, which is 10 documents per probe. A walk has no fixed cost in work (1,000 documents: 157,000–203,000 instructions), so small corpora get no floor (fleet `syn24-work-probe-2-06961a4e`)
  - the trade-off: a walk runs on every core, a derivation on one. On 12 threads the walk's wall time matches 90–190 documents per probe (fleet `syn24-tuning-probe-f4b4dd86`), so just below the crossover a derivation can take up to about 10× the parallel walk's wall time. Under concurrent readers and shared hosts, throughput and the other tenants pay for work, which is why the default counts work. A latency-first deployment with one tenant can raise it toward the wall-time crossover
- **`store.verify_on_open`**: on disk, the newest checkpoint is used in place from a memory map: opening reads only its structure, so a restart takes milliseconds and memory grows with what queries touch. `true` checks the whole checkpoint when opening (its CRC, every index block, every dictionary key): opening then reads the whole file once, and a damaged checkpoint is refused at once instead of failing when a query reaches the damage
- **`store.map_checkpoints`**: on disk, once a checkpoint is written (after a bulk load, and by background or explicit checkpoints) the data it holds is served from the file, mapped, as after a restart, and its copies in memory are freed: memory then grows with what queries touch, and the OS can page out the rest. `false` keeps everything in memory as well (the file is still written)
- **`store.checkpoint_after_wal`**: on disk, a checkpoint is written in the background once the write-ahead log has grown by this much since the last one. Smaller: a restart replays less, and the WAL takes less disk; larger: fewer checkpoints written under heavy writing. The checkpoint covers the whole store, so its cost grows with the data, not with the log
- **`store.wal_archive`**: on disk, keep the WAL segments that checkpoints cover in `wal-archive/` of the data directory instead of deleting them. With an image backup they restore the store to any later revision (`nrese-server restore DIR --wal ARCHIVE --wal DATA/wal --until-revision N`). The archive grows until pruned (`nrese-server prune-archive R`, with `R` the oldest image backup's revision plus one). Sync it to another machine for recovery from a lost disk
- **`store.wal_damage`**: on disk, what opening does with damage in the newest write-ahead log segment. What a crash during a commit leaves (one incomplete last record, or zeros to the end of the file, with no intact record after it) is always cut: that commit was never acknowledged. Its bytes are kept in `wal-cut/` of the data directory, and a warning is logged. Any other damage (a complete record failing its checksum, or damage with intact records after it) may hold acknowledged commits. `refuse` doesn't open the store and leaves the log as it is, so an operator can back the directory up and decide. `keep-prefix` opens with the log up to the damage, copies the rest to `wal-cut/` first and logs the cut as an error: commits acknowledged after the damage are then only in that copy. Neither costs anything on healthy commits; the choice is downtime after a damaged crash against losing acknowledged commits without anyone's word. Damage in an older segment always refuses to open
- **`store.index_encoding`**: `fast` (frame of reference only, the fastest scans) or `compact` (a block position with few, far-apart values is stored as a palette of them; on host A DBpedia core's store is 3.3 % smaller with queries 7 % slower, Wikidata lexemes' 8.6 % smaller with queries even). Applies to blocks built later (loads, compactions, checkpoints); both are read either way, so switching needs no reload. This version writes checkpoint format 12 and reads every earlier format from 4; which binary reads which: [rolling back](server-setup.md#13-upgrade-strategy)
- **`store.vocabulary`**: `plain`, or `fsst` (compressed with two FSST symbol tables, one for IRIs and one for the other terms, trained on a sample when the checkpoint is written). On host A's stores the keys shrink to 39 % (Wikidata lexemes, 576 → 224 MiB), 45 % (DBpedia core) and 53 % (YAGO); a key read from a compressed checkpoint costs a decode (about 17 ns in order, more at random), so result-heavy and string-scanning queries pay for it. Terms interned since the last checkpoint stay plain until the next. Both forms are read whatever the setting: switching takes effect at the next checkpoint
- **`store.invalid_statements`**: what a bulk load (`nrese-server load`, the import endpoints) does with a statement it can't read: `quarantine` loads every valid statement and keeps each invalid one verbatim in the repository's quarantine (its text, file, line, reason and load; `quarantine/` of the data directory), invisible to queries, reasoning, exports and statistics until it is re-admitted through the commit gates or discarded (docs/design/quarantine.md); the load reports the count and the first examples. `strict` stops the load at the first, and nothing of it is committed. A load may ask for either (`--strict`, `--quarantine`; `invalid=` on the import endpoints). Writes other than bulk loads (SPARQL Update, Graph Store, statements) are always strict

### The result cache

Set by `budgets.result_cache` (`NRESE_QUERY_CACHE_BYTES`; older key `store.query_cache_bytes`) and `budgets.result_cache_pins` (`NRESE_PINNED_QUERIES`); types and defaults in [Budgets](#budgets).

- the default is 2% of the memory the server may use (the container's limit, else the machine's), at least 64 MiB and at most 8 GiB; `0` disables the cache
- what it holds: the results of query parts (every operator the executor runs: a join, a group, a subquery, the whole query; and each prefix of a basic graph pattern's joins) are kept as id columns and shared by every query on an unchanged store, so a repeated query and a query sharing a part with an earlier one are answered from memory. Parts that differ only in their variables' names are one part. A part is valid only for the snapshot it was computed on (its revision and the statements it shows: a transaction's pending state never meets a committed part) and for everything else its result depends on: the dataset (`FROM`, the protocol's graphs), the read model (asserted, inferred, both), the user's access, the equality reading and the base IRI. So a commit makes every older part unreachable, and they are dropped when a query runs on the new revision. Parts calling `NOW()`, `RAND()`, `UUID()`, `STRUUID()` or `BNODE()`, or reading a `SERVICE`, are never kept
- what is kept: a part that took longer to compute than its copy takes (at least 20 µs, and 1 µs per 10 kB twice over, `COPY_BYTES_PER_MICRO` in `nrese-sparql/src/cache.rs`), no larger than a quarter of the budget; ranked by its cost times its hits over its bytes (GreedyDual-Size-Frequency), the lowest evicted first; a part worth less than what it would evict is not kept. The budget counts the columns, the computed values, the keys and the bookkeeping (about 770 bytes per entry), and the cache never holds more heap than it (a test checks it against the allocator)
- queries asking for the same part at the same time compute it once: the others wait for it (and compute it themselves if that query fails); a query running on a Rayon worker never waits, it computes the part beside the other ([the bounded rule](../design/research-designs.md#8-query-progress-under-shared-cache-waits-decided-the-bounded-rule))
- a query's whole answer is kept too, as the bytes of each output format it was written in (beside its id columns, in the same budget and ranking), where writing it cost more than twice copying it: a repeated query is a copy of what was written before. An answer's key also holds what the store's answer depends on: under `owl2-dl` the mode and the `dl-answers` asked for (answers over the lower bound L are cached for L, never for the snapshot), the completeness status the answer is sent with, and the OWL 2 QL rewriting where it applies; the status header is computed for every request, so a cached answer carries the one a fresh answer would
- EXPLAIN marks each operator answered from the cache (`"cache": "hit"`) or taken from another query computing it (`"cache": "shared"`); `GET /api/v1/repositories/{id}/cache` shows the counts, `DELETE` on it drops every result not pinned
- pins (`budgets.result_cache_pins`, paths separated by `;` in the environment): SPARQL query files run at startup and pinned in the result cache, each under its file name without the extension (`top-classes.rq` is the pin `top-classes`); a missing file, a query that can't be cached, or results larger than the budget leaves are a startup error. A pinned result is never evicted while the store doesn't change; after a commit it is pinned again the next time its query runs. Pins are also set and removed while the server runs, through `PUT`/`DELETE /api/v1/repositories/{id}/cache/pins/{name}` ([http-api.md](http-api.md))

## SHACL

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="shacl.shapes_graph"></a>`shacl.shapes_graph` | `NRESE_SHACL_SHAPES_GRAPH` | text: an IRI | `http://rdf4j.org/schema/rdf4j#SHACLShapeGraph` (the graph RDF4J and GraphDB use) | start | The graph whose shapes `GET /dataset/shacl` validates against ([http-api.md](http-api.md#shacl-validation)) and the gate uses |
| <a id="shacl.gate"></a>`shacl.gate` | `NRESE_SHACL_GATE` | choice: `off`, `report`, `enforce` | `off` | start | SHACL as a commit gate (below) |
| <a id="shacl.gate_severity"></a>`shacl.gate_severity` | `NRESE_SHACL_GATE_SEVERITY` | choice: `violation`, `warning`, `info` | `violation` | start | The least severity `enforce` rejects |

`report` validates every commit against the shapes graph and logs what it introduces; `enforce` also rejects a commit that introduces a result at or above `gate_severity`, or a validation failure, and the commit changes nothing. Only what a commit introduces counts: data that was invalid before doesn't block writes (validation at `/dataset/shacl` reports it). The check validates the focus nodes the commit can affect, after reasoning; a commit that changes the shapes graph is validated in full.

## Reasoner

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="reasoner.mode"></a>`reasoner.mode` | `NRESE_REASONING_MODE` | choice: `disabled` (aliases `none`, `off`), `rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`, `owl2-ql`, `owl2-rl`, `owl2-dl`, `custom` | `disabled` | repo (as `reasoning`) | The ruleset materialised into the inferred stack (below) |
| <a id="reasoner.rules"></a>`reasoner.rules` | `NRESE_REASONING_RULES` | text: a `.n3` or `.pie` file | none | repo (as `rules`) | User rules (below) |
| <a id="reasoner.equality"></a>`reasoner.equality` | `NRESE_REASONING_EQUALITY` | choice: `representatives`, `compact`, `replicate` | `representatives` | start | How `owl:sameAs` is reasoned with and stored |
| <a id="reasoner.equality_answers"></a>`reasoner.equality_answers` | `NRESE_REASONING_EQUALITY_ANSWERS` | choice: `strict`, `canonical` | `strict` | start | With `compact`: every identity, or one per class |
| <a id="reasoner.equality_expansion"></a>`reasoner.equality_expansion` | `NRESE_REASONING_EQUALITY_EXPANSION` | choice: `late`, `early` | `late` | start | With `compact`: when classes are expanded |
| <a id="reasoner.unnamed_classes"></a>`reasoner.unnamed_classes` | `NRESE_REASONING_UNNAMED_CLASSES` | choice: `derive`, `skip` | `derive` | start | Memberships in unnamed union classes nothing consumes |
| <a id="reasoner.ql_rewriting"></a>`reasoner.ql_rewriting` | `NRESE_REASONING_QL_REWRITING` | choice: `auto`, `on`, `off` | `auto` | repo (as `ql_rewriting`) | OWL 2 QL answers through existentials |
| <a id="reasoner.support_sets"></a>`reasoner.support_sets` | `NRESE_REASONING_SUPPORT_SETS` | integer, at least 1 | `16` | start | Support graph sets kept per inferred statement |

`reasoner.mode` values:
- `disabled`: no reasoning; reads see asserted statements only
- `rdfs`: the RDFS closure is materialised into the inferred stack (the six rules queries over data use)
- `rdfs-full`: every RDFS entailment rule and the axiomatic triples
- `rdfs-plus`: RDFS with equality, inverse, symmetric, transitive, functional and inverse functional properties, equivalent classes and properties
- `owl-horst`: RDFS-Plus with `hasValue`, `someValuesFrom` and `allValuesFrom` (ter Horst's pD*)
- `owl2-ql`: OWL 2 QL materialised; commits that violate its disjointness axioms are rejected
- `owl2-rl`: the OWL 2 RL/RDF closure is materialised, and commits that violate a consistency rule are rejected with the rule and the facts it matched
- `owl2-dl`: OWL 2 DL ([design](../design/owl2-dl.md) §8). The OWL 2 RL closure is materialised as the lower bound, as with `owl2-rl`, and the DL engines are added: commits checked for consistency under OWL 2 DL, the upper bound, and a completeness status on every answer (the `dl.*` settings below)
- `custom`: the user's rules only (`reasoner.rules`)
- unknown values are a startup error (a typo must not silently disable consistency checking); `rules-mvp`, the removed v1 reasoner, is an error that names its replacement

Details:

- **`reasoner.rules`**: a Notation3 file (`.n3`) or a GraphDB ruleset (`.pie`) of user rules. With `custom` they are the whole program; with any other reasoning mode they are added to its ruleset. They are compiled at startup, and a rule the reasoner can't run stops the startup with the rule and the reason. What compiles:
  - `{ premises } => { conclusions } .` (and `<=`), with quick variables `?x`; blank nodes in the premises are variables
  - `{ premises } => false .`: a consistency rule; commits that make it hold are rejected, naming it (`rules.n3#4`)
  - `log:notEqualTo` and `log:equalTo` in the premises
  - plain triples outside formulas: facts that hold whatever the data
  - not (yet): other builtins (`math:`, `string:`, `list:`, `time:`), blank nodes in conclusions (existentials), formulas as terms
  - a GraphDB ruleset (`.pie`) is read too: prefixes, axioms, `Id:` rules with `[Constraint a != b]` (and `!= blank_node`), `Consistency:` checks, `[Context <g>]` on statements with a constant predicate (their statements get a predicate of their own, `urn:nrese:pie-context:<g>#<p>` with `%` and `#` in `<g>` and `<p>` percent-encoded, one per context and predicate, and are in the inferred stack, where GraphDB hides them); `[Cut]` is ignored. GraphDB's RDFS, RDFS-Plus, OWL-Horst and Publishing rulesets (and their optimised variants) compile; its OWL 2 RL and QL ones don't (a context on a variable predicate, existential conclusions), and NRESE's own `owl2-rl` and `owl2-ql` modes cover those profiles
  - a different rules file (one character is enough) makes the recorded closure stale, so the next startup rematerialises
- **`reasoner.equality`**: with a ruleset that reasons with equality (`owl:sameAs`: `owl-horst`, `owl2-rl`), `representatives` makes a full materialisation (load, rematerialisation) compute the closure over one representative per `owl:sameAs` class and expand it to every identity, instead of the replacement rules copying every fact to every identity while the closure is computed (`replicate`). The stored closure is the same either way (W4 stage A; a property test checks it on random ontologies); on the integration workload's cohort tier the closure took 0.21 s instead of 3.09 s. Commits maintain it incrementally as before
  - `compact` (W4 stage B) also keeps the closure over representatives in the store: one copy of each fact per class instead of one per combination of identities, and each identity stored as `identity owl:sameAs representative`. Reads of the default graph expand it to every identity, so queries answer as with the other modes (a differential test compares all three after inserts, deletions, merges and splits of classes, and named graphs). The trade-off: storage and commits shrink by the replication factor; scans of facts about identities pay the expansion, and the engine's columnar, range and group-count fast paths step aside while any class exists. A commit that merges or splits classes recomputes the closure after it. Changing the mode recomputes the closure at the next start.
- **`reasoner.equality_answers`**: with `reasoner.equality = "compact"`: `strict` answers with every identity of each `owl:sameAs` class, as if the closure were replicated; `canonical` with one identity per class, its representative (the smallest id), for analytics where the identities are one thing: the query is then answered over the quotient of the dataset by the classes (named graphs included): a term the query names (in a pattern, VALUES, a FILTER, BIND or `IRI()`) stands for its class too, and is answered by its representative. A query of another default graph (`FROM <g>`) or of the asserted model is answered as stored; over a store holding a triple term that names another identity than its representative, or with a literal in a class, canonical queries are refused ([research-designs §1](../design/research-designs.md), "Canonical answers"; work package W4 stage C)
- **`reasoner.equality_expansion`**: with `reasoner.equality = "compact"` and strict answers: `late` evaluates the parts of a query whose answer can't tell identities apart (basic graph patterns, joins, OPTIONAL without a condition, UNION) over representatives and expands each solution once, instead of every read expanding every statement to every identity before the joins (`early`, stage B). The answers are the same (a differential test against `early` and against replication checks it); FILTER, BIND, VALUES, MINUS, property paths, named graphs and the operators that count or order stay expanded early. It applies where the materialised model is read and no named graph holds statements; EXPLAIN shows it as `late equality expansion`
- **`reasoner.unnamed_classes`**: `derive` is OWL 2 RL as written; `skip`: memberships in unnamed union classes that nothing consumes (an anonymous union used only as a domain or range, as the GND ontology does) are not derived. Everything else stays; members of such a class declared `owl:Class` are still `owl:Thing`s. With the GND ontology on the integration workload's example tier: 219,840 inferred statements become 99,213, with the same answers to its questions. A commit that makes such a class used (a subclass axiom on it) triggers a rematerialisation
- **`reasoner.ql_rewriting`** (a repository's `ql_rewriting` overrides it, changed at once): `auto` rewrites while the inferred stack is current under `owl2-ql`; `on` under `owl2-rl` too (not the default there: `owl2-rl` keeps the standard OWL 2 RL/RDF closure); `off` never; never under `owl2-dl`, whose bounds give those answers with their status. Where it applies, queries also get the OWL 2 QL answers that go through individuals an existential axiom implies (`Employee ⊑ ∃worksFor.Organisation`: `SELECT ?x { ?x :worksFor [] }` finds every employee), by tree-witness rewriting of their basic graph patterns over the closure ([design](../design/ql-rewriting.md)). It applies to queries over the default graph that read the inferred statements, without a dataset of their own; under graph access, a reader who sees only supported inferences gets the existentials of its readable graphs, one who sees every inference all of them, one who sees none no rewriting. A variable counts as existential when the query doesn't read it outside its pattern. The rewriting of a pattern is bounded in size and in work (50,000 steps); past a bound the pattern runs as written and the answer says `sound-only`. Bag queries keep their materialised rows and get each further answer once. EXPLAIN lists `ql-tree-witness` (or `ql-limit` when a pattern was too large to rewrite and ran as written) and reports the rewriting under `ql`. Incompleteness is never silent: where an axiom the closure's rules don't answer (one outside the closure's profile) or the rewriting doesn't follow (transitivity, chains, functional properties, keys, cardinalities, `allValuesFrom`, `hasValue`, RL constructs on the left) could add an answer the query reads, through named individuals or those an existential implies, or where a pattern reached a bound or runs without the rewriting, the answer says `sound-only` with the reasons (the `NRESE-Completeness` header, EXPLAIN's `ql.completeness`), or `unsound` where such a pattern sits under `MINUS` or a negated `EXISTS`; otherwise `complete`: every certain answer under the OWL 2 Direct Semantics. A schema without existentials on the right changes no query
- **`reasoner.support_sets`**: the support graph sets kept per inferred statement for `inferred = "supported"` in the access policy, the smallest first (graphs in IRI order). It bounds memory and work, not what users see: a statement past it is marked, and a user none of its kept sets fits is decided by the closure of the graphs it may read, computed once for the user's view of a revision (slower, same answers); the log reports how many statements are past it
- with `reasoner.mode = "owl2-dl"` (the [`owl2-dl` mode](../spec/reasoning-semantics.md)), all scope start:
  - `dl.answers` (`NRESE_DL_ANSWERS`), choice, default `certain-where-complete`: the certain answers where the store can prove them complete, else the sound ones, the status saying which; `sound`: the lower bound alone, never checked against the upper bound; `exact`: certain answers, or the query fails with 422, problem type `incomplete-answer`, naming the reasons and the unresolved candidates. A query may ask for another with `dl-answers` ([http-api.md](http-api.md#sparql))
  - `dl.consistency` (`NRESE_DL_CONSISTENCY`), choice, default `inline`: a commit that makes the data inconsistent under OWL 2 DL is rejected, as the RL gate rejects; `off`: not checked, and every answer's status then says the consistency is unknown
  - `dl.threads` (`NRESE_DL_THREADS`), integer, default `0` (every core): the maximum workers of a DL operation
  - **The DL budgets.** None is built into the engines ([`nrese_dl::limits`](../../crates/nrese-dl/src/limits.rs)): each is one of these settings, of type limit (a number, with a unit where it has one, or `0` or `unlimited` for none). A run that reaches one stops with a reason naming the setting (`dl.max_nodes reached (…)`): its candidate stays unresolved, its class untested, its commit's status `unknown`, never a wrong answer. A guard holds the engines to it (`crates/nrese-dl/tests/tableau/limits.rs`). The CLIs (`dl_classify`, `tableau_consistency`) take the same budgets as options, none by default.

    | Setting (env) | Default | Why this default |
    |---|---|---|
    | `dl.timeout` (`NRESE_DL_TIMEOUT_MS`), duration | 30min | The server's safety default: one DL task (a commit's consistency check, a query's exact services, a classification) can't hold its writer slot or worker forever; what isn't decided by then is reported, never guessed |
    | `dl.memory` (`NRESE_DL_MEMORY`), size | none | Memory comes from the request's envelope (`budgets.query_memory` within `budgets.total_query_memory`), which every run of the request draws from; a classification has an envelope of its own within the catalog's budget. Set it to give each engine run a cap of its own inside that, divided among a batch's workers. What it covers and how it can overshoot: [owl2-dl.md §13](../design/owl2-dl.md#13-configuration) |
    | `dl.max_candidates` (`NRESE_DL_MAX_CANDIDATES`), count | 1000 | The candidate answers (in the upper bound, not the lower) a query checks with the exact services: bounded so that a query with a large gap answers within `budgets.query_timeout`, the rest reported unresolved, instead of running its exact tests until the request times out |
    | `dl.max_nodes` (`NRESE_DL_MAX_NODES`), count | none | A deterministic budget of one hypertableau run, for reproducible runs and tests; time and memory bound a run otherwise |
    | `dl.max_branch_points` (`NRESE_DL_MAX_BRANCH_POINTS`), count | none | As `dl.max_nodes`: so that a decision doesn't depend on the machine's speed where that matters |
    | `dl.max_merges` (`NRESE_DL_MAX_MERGES`), count | none | As `dl.max_nodes`, for a search whose work is merging |
    | `dl.explanation_tests` (`NRESE_DL_EXPLANATION_TESTS`), count | none | The entailment tests one explanation or a rejection's evidence runs; past them the justification is known-entailing, not minimal (an explanation is undecided if none could run). `dl.timeout` bounds it otherwise |
    | `dl.lower_bound_timeout` (`NRESE_DL_LOWER_BOUND_TIMEOUT_MS`), duration | 60s | A route threshold, not a budget: past it a classification's Horn lower bound stops and the hypertableau tests the classes; never more than half of what is left of `dl.timeout` (`unlimited`: only that half). 60 s because ORE's ore_ont_7127 (65 k classes) needs 3.9 s for it and then no test, where 65 k tests take 43 s |
    | `dl.max_join` (`NRESE_DL_MAX_JOIN`), `dl.max_join_steps` (`NRESE_DL_MAX_JOIN_STEPS`), count | 1048576, 65536 | Route thresholds of the context core: a join past them goes to the hypertableau, which is cheaper there than finishing it (the ORE development set) |
- with any mode but `disabled`:
  - `nrese-server load` and startup materialise the closure (startup skips it when `reasoning.state` in the data directory records that the inferred stack is current for this build's semantics; `/version` reports them as `reasoning_semantics`, e.g. `owl2-rl v2 <fingerprint>`)
  - what each mode derives and omits: [reasoning-semantics.md](../spec/reasoning-semantics.md)
  - every commit maintains it incrementally, inside the same transaction
  - queries see asserted and inferred statements by default; `infer=false` or `FROM <http://www.ontotext.com/explicit>` reads asserted statements only, `FROM <http://www.ontotext.com/implicit>` inferred ones
- switching reasoning off clears the inferred stack at the next startup

## Replication

Read replicas ([replication.md](replication.md)); both sides need an on-disk store (else a startup error).

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="replication.mode"></a>`replication.mode` | `NRESE_REPLICATION_MODE` | choice: `off`, `primary`, `replica` | `off` | start | `primary`: serve the log and an image to administrators. `replica`: start from the primary's image when the data directory holds no store, follow its log, take no writes, don't reason |
| <a id="replication.primary"></a>`replication.primary` | `NRESE_REPLICATION_PRIMARY` | text: a base URL | none | start | A replica's primary (`https://primary.example:8080`) |
| <a id="replication.token"></a>`replication.token` | `NRESE_REPLICATION_TOKEN` | text, secret | none | start | The bearer token a replica presents to its primary (an administrator's); never printed, refused by `--set` |
| <a id="replication.poll"></a>`replication.poll` | `NRESE_REPLICATION_POLL_MS` | duration | `500ms` | start | How long a replica waits before asking again once caught up |
| <a id="replication.batch_bytes"></a>`replication.batch_bytes` | `NRESE_REPLICATION_BATCH_BYTES` | size | `8MiB` | start | About how much log a replica asks for at once |
| <a id="replication.round_timeout"></a>`replication.round_timeout` | `NRESE_REPLICATION_ROUND_TIMEOUT_MS` | limit (duration) | `5min` | start | The longest one round of following the primary's log may take, its answer whole; `0` or `unlimited`: no total limit |
| <a id="replication.image_timeout"></a>`replication.image_timeout` | `NRESE_REPLICATION_IMAGE_TIMEOUT_MS` | limit (duration) | `1h` | start | The longest a replica waits for its primary to prepare an image (the primary writes and checks it before its answer begins); `0` or `unlimited`: no limit |

Fixed limits on every exchange with the primary: a log answer must begin within 30 s, and every answer's body may pause at most 30 s between pieces (an image streams as long as it keeps coming, however large); a round past a limit is an error, retried with a growing pause and shown in the status.

## Federation (`SERVICE`)

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="federation.allow"></a>`federation.allow` | `NRESE_FEDERATION_ALLOW` | list (`,`): `http(s)` IRIs or prefixes, or `*` | none (`SERVICE` off) | start | The endpoints `SERVICE` may call (`https://query.wikidata.org/`), or `*` for any. Empty: `SERVICE` is an error, and `SERVICE SILENT` one solution without bindings. Allowing `*` lets anyone who may query make the server fetch any URL |
| <a id="federation.timeout"></a>`federation.timeout` | `NRESE_FEDERATION_TIMEOUT_MS` | duration | `30s` | start | Per request to an endpoint |
| <a id="federation.max_rows"></a>`federation.max_rows` | `NRESE_FEDERATION_MAX_ROWS` | integer | `1000000` | start | Rows one request may return |

A `SERVICE` joined to a pattern sends the pattern's distinct values as `VALUES`, 200 rows per request (up to 20,000 values; beyond, the block goes once, unbound); redirects are not followed; queries with `SERVICE` are never answered from the result cache. With access enforced, calling `SERVICE` is a role's privilege ([below](#graph-level-access-control)).

## Budgets

Every limit on memory, time and request size is in one table, `[budgets]`. Memory shares range from `0%` to `100%` and use the process's capacity (including container limits), read once when configuration loads. `budgets.query_memory = "50%"` gives each query half of that capacity; `0%` has the same meaning as `0`.

**Which memory limit.** Three settings bound memory; none of them bounds the store's own data and indexes.
- `budgets.query_memory`: what one query's evaluation may hold. Past it the query gets `413`.
- `budgets.total_query_memory`: what all running queries and updates hold together. A request that finds no room gets `503` and may succeed later.
- `budgets.process_memory`: what the whole process may hold before a long operation (a load's or a commit's reasoning) stops with an error instead of taking the machine. Queries count toward it too: one that finds no room left in it gets the same `503`.

To give the server's request work 2 GiB, set `budgets.total_query_memory = "2GiB"` (Docker: `-e NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES=2GiB`), and lower `budgets.query_memory` too if one query may not take all of it. To cap the process as a whole, reasoning included, set `budgets.process_memory` (`-e NRESE_PROCESS_MEMORY_BYTES=2GiB`); in a container with a memory limit its default is already 75 % of that limit. To see them in force: `GET /version` reports `query_memory` and `total_query_memory` under `budgets`, in bytes (`0` = unlimited); `nrese-server check-config` prints both; `/metrics` shows the total as `nrese_query_memory_limit_bytes`. `budgets.process_memory` is reported by none of these in this build: it shows only in the error of an operation it stops (`stopped at the process's memory limit (N MiB); nothing was applied`), so check that it is set where you set it.

| Key | Environment | Type | Default | Scope | What it bounds |
|---|---|---|---|---|---|
| <a id="budgets.query_memory"></a>`budgets.query_memory` | `NRESE_MAX_QUERY_MEMORY_BYTES` | size or share | 4 GiB | start | One query's memory envelope: its intermediate and retained results and, under `owl2-dl`, its DL work (the exact services' compilation scratch and engines). A query whose evaluation needs more is answered `413`; DL candidates past it are left unresolved, the status says so (`422` under `dl.answers = exact`). `0` = unlimited |
| <a id="budgets.total_query_memory"></a>`budgets.total_query_memory` | `NRESE_MAX_TOTAL_QUERY_MEMORY_BYTES` | size or share | 50 % of the machine's memory | start | Every running request's envelope across the catalog: queries, updates (their WHERE evaluation and `owl2-dl` commit gate), DL entailment checks and explanations. A query that asks for more than is left is answered `503` and may succeed later. `0` = unlimited |
| <a id="budgets.process_memory"></a>`budgets.process_memory` | `NRESE_PROCESS_MEMORY_BYTES` | size or share | 75 % of the machine's memory (the container's limit where there is one) | start | The private memory (mapped store files excluded) the server may hold before a long operation stops instead of taking the machine: a materialisation (load, startup, change of rules) or the reasoning of a commit ends with an error and applies nothing. `0` = unlimited. An operation is admitted by this limit less what the process holds, not by what the system has free at that moment: a container shared with other large users is the operator's to size. A size above the memory the process may use counts as that memory, where it can be read. The default is refused at startup where the memory the process may use can't be read, Linux before 3.15 among them (its kernel doesn't show which mount a file was opened on): set this there. On cgroup v2 a limit set above the container's cgroup namespace (Docker's `--cgroup-parent`, a Kubernetes pod without a limit of its own under a node's cap) isn't visible to it: set this there. Likewise on Windows only the job object the server runs in directly is visible, not an enclosing one: when a launcher or service wrapper caps a parent job, set this there |
| <a id="budgets.huge_pages"></a>`budgets.huge_pages` | `NRESE_HUGE_PAGES` | choice: `on`, `off`, `bulk` | `on` | process | Whether transparent huge pages may back the server's memory (Linux; elsewhere nothing changes). `on`: work over large heaps runs faster (LUBM 100's materialisation 26 % faster), but while serving many small requests the allocator holds about 2.7 times the memory (a whole 2 MiB page resident for a touched byte). `off`: less memory held while serving, slower materialisation. `bulk`: on while a bulk load or a materialisation runs, off otherwise (serving): their speed with serving's memory. See [server-setup.md](server-setup.md) §12.1 |
| <a id="budgets.reasoning_release"></a>`budgets.reasoning_release` | `NRESE_REASONING_RELEASE` | choice: `off`, `on` | `off` | process | Whether a materialisation gives back between its rounds, after the last one and before the inferred stack what the last round freed. `on`: a lower and steadier peak, as the allocator keeps a freed large block about a round's time (host A, LUBM 100: 2,074 → 1,776 MiB, 5.6 % more reasoning time; OWL2Bench RL-1: −25 % for 6.6 %; a small RDFS materialisation: −28 % for 14 %). `off`: the fastest materialisation. For a memory budget near its limit, or for peaks that must compare between runs |
| <a id="budgets.bulk_load_memory"></a>`budgets.bulk_load_memory` | `NRESE_BULK_LOAD_MEMORY` | size or share | 25 % of the machine's memory | start | The quads of a bulk load (`nrese-server load`, or a load into an empty store). Past it they are sorted in chunks of a third of it (one filling, one being sorted, its sorted copy), spilled to `bulk-spill/` in the data directory and merged into each index permutation while the checkpoint is written: one more pass over the disk, but the load's quads take this much plus one packed permutation whatever the data's size. The dictionary is apart (it grows with the distinct terms). Needs `store.map_checkpoints`. `0` = unlimited |
| <a id="budgets.query_timeout"></a>`budgets.query_timeout` | `NRESE_QUERY_TIMEOUT_MS` | duration | 30 s | repo (as `query_timeout_ms`, not the default repository) | A query, until its last result is sent (`408`) |
| <a id="budgets.update_timeout"></a>`budgets.update_timeout` | `NRESE_UPDATE_TIMEOUT_MS` | duration | 60 s | start | A SPARQL update, reasoning included |
| <a id="budgets.graph_read_timeout"></a>`budgets.graph_read_timeout` | `NRESE_GRAPH_READ_TIMEOUT_MS` | duration | 30 s | start | A streamed read of stored statements (Graph Store `GET`, RDF4J `GET /statements`, the N-Quads export): the wait for its first chunk, then for each next one. A client that keeps reading is never cut off; one that stops for this long is. A Graph Store `HEAD` |
| <a id="budgets.graph_write_timeout"></a>`budgets.graph_write_timeout` | `NRESE_GRAPH_WRITE_TIMEOUT_MS` | duration | 60 s | start | A Graph Store write |
| <a id="budgets.query_text"></a>`budgets.query_text` | `NRESE_MAX_QUERY_BYTES` | size | 1 MiB | start | The text of a query (`413`) |
| <a id="budgets.update_size"></a>`budgets.update_size` | `NRESE_MAX_UPDATE_BYTES` | size | 16 MiB | start | A SPARQL update request (`413`) |
| <a id="budgets.upload_size"></a>`budgets.upload_size` | `NRESE_MAX_RDF_UPLOAD_BYTES` | size | 128 MiB | start | An RDF payload (Graph Store, TELL, SHACL shapes); larger data goes through `nrese-server load`. An N-Quads restore (`POST …/restore`) has no limit of its own: the server's body limit bounds it, 3 × the largest of this, `budgets.update_size` and `budgets.query_text`, plus 64 KiB (about 384 MiB, some 3 million statements, by default), and it holds the whole payload while it parses. Store-sized backups restore offline: `nrese-server load --replace` for an N-Quads export, `nrese-server restore DIR` for an image |
| <a id="budgets.result_cache"></a>`budgets.result_cache` | `NRESE_QUERY_CACHE_BYTES` | size or share | 2% of memory (64 MiB to 8 GiB) | start | Results of query parts (id columns) kept for repeated queries and shared sub-patterns ([the result cache](#the-result-cache)). `0` switches the cache off. Where the memory can't be read, 64 MiB |
| <a id="budgets.result_cache_pins"></a>`budgets.result_cache_pins` | `NRESE_PINNED_QUERIES` | list (`;`): paths | none | start | Query files whose results are pinned in the result cache at startup ([the result cache](#the-result-cache)) |

`/version` reports, under `budgets`, `query_memory`, `total_query_memory`, `query_text`, `update_size`, `upload_size` and `result_cache` in bytes, and `query_timeout_ms`, `update_timeout_ms`, `graph_read_timeout_ms` and `graph_write_timeout_ms`. It doesn't report `process_memory`, `bulk_load_memory`, `huge_pages`, `reasoning_release` or the pins.

How query memory accounting works:

- Each request has one envelope within the catalog's budget, and everything it holds draws from it: operator tables and scratch (joins, groups, sorts), retained native results and DL bound tables until they drop, and under `owl2-dl` the DL engines' capacity and their compilation scratch (`docs/design/execution-core.md`, "Execution and resource lifetime"). A table that is being built may take half of what is left, because growing it, and merging the parts of a parallel join, holds its rows twice for a moment. Decoded graph payloads and encoder buffers are not counted; classification and realisation, shared per revision, have an envelope of their own within the catalog's budget, and the process limit.
- The machine's memory is the container's limit where there is one (cgroups, or a Windows job object), else the machine's, read on Linux and Windows. Where it can't be read (the reading fails, or another platform), a limit given as a share such as `50%` (the defaults of `total_query_memory`, `process_memory` and `bulk_load_memory` included) stops the server at startup, naming the setting: set a size, or `0` for no limit. `result_cache` falls back to 64 MiB, since it is a cache, not a limit.
- `process_memory` needs the process's memory read while it runs: on a platform without a reader a non-zero value is refused at startup. A reading that fails while an operation runs stops the operation, with an error saying the memory couldn't be read.
- The store's own memory (the data and its indexes) is not part of either budget.
- Password hashing (local logins, `login-argon2`): each Argon2id run holds about 19 MiB outside every budget, and at most one run per CPU (`std::thread::available_parallelism`, which follows a container's CPU quota) goes at once, so together they hold at most CPUs × 19 MiB. A check that can't get its turn within 10 s of starting (after the request's blocking task has started, so the response can take longer) is answered `503` with `Retry-After: 1` and isn't counted as a failed login; a remembered credential runs no hash. In v1.6 the runs are charged to the process's admission root instead.
- A request over a size limit is answered `413`. Requests are held in memory while they are handled, so a size limit is also memory a request may take.

Server startup installs `budgets.process_memory` as the process fallback. Opening an
embedded store never replaces that global policy; embedding hosts may install it explicitly.
Store-local watches cover participating operations, including DL bounds and diagnostics;
an explicit zero remains disabled. These are cooperative checkpoints, not allocator limits
or preemption of schema reading, normalisation and compilation.

### Process-wide settings

In this build four settings are the process's, not a repository's: `store.index_encoding`, `store.vocabulary`, `budgets.huge_pages` and `budgets.reasoning_release`. They are set once for the whole process (`nrese-store/src/process_settings.rs`): the server sets them from its configuration at start-up, before it opens a store, and refuses to start if a store opened earlier set other values. Every store opened after that leaves them as they are, so the operator's values hold for every repository, for `restore`'s recovery, and through the stores the server opens for itself (the system store, a draft check's scratch store). `print-query --schema` reads no configuration and runs with the defaults. Under `bulk`, the bulk loads and materialisations of every store count together: huge pages stay on while any one runs. Switching them off doesn't take back huge pages the process already holds. An embedding application sets them with `nrese_store::apply_process_settings` before it opens a store, or gets the first opened store's everywhere; `nrese_store::process_settings()` reads the values in effect.

One diagnostic is read from the environment outside this configuration: `NRESE_CLASSIFY_TRACE` (set to anything) makes the DL classification write its phases with their start times to stderr (`nrese-dl/src/classify/mod.rs`). It changes no result; it is for the lab, not an operator setting.

The same settings have older names, which still work: `policy.limits.max_query_bytes`, `max_query_memory_bytes`, `max_update_bytes`, `max_rdf_upload_bytes`; `policy.timeouts.query_ms`, `update_ms`, `graph_read_ms`, `graph_write_ms`; `store.query_cache_bytes`. A setting under both names is a startup error.

## Policy Rate Limits

Requests per client and window; a client over its limit gets `429`.

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="policy.rate_limits.window_secs"></a>`policy.rate_limits.window_secs` | `NRESE_RATE_LIMIT_WINDOW_SECS` | integer (seconds) | `60` | start | The window |
| <a id="policy.rate_limits.read_requests_per_window"></a>`policy.rate_limits.read_requests_per_window` | `NRESE_READ_REQUESTS_PER_WINDOW` | integer | `0` (no limit) | start | Queries, Graph Store reads, service descriptions |
| <a id="policy.rate_limits.write_requests_per_window"></a>`policy.rate_limits.write_requests_per_window` | `NRESE_WRITE_REQUESTS_PER_WINDOW` | integer | `0` (no limit) | start | Updates, `TELL`, Graph Store writes |
| <a id="policy.rate_limits.admin_requests_per_window"></a>`policy.rate_limits.admin_requests_per_window` | `NRESE_ADMIN_REQUESTS_PER_WINDOW` | integer | `0` (no limit) | start | Operator reads, administration, metrics |

## Timeouts and writes

A write that times out before its commit starts is never committed, and the request gets 408. The deadline reaches every phase of the write:
- the `WHERE` evaluation of an update;
- commit-path reasoning, polled between rounds and per work unit.

Once a reasoning run observes cancellation and finishes unwinding, it discards the asserted
and inferred changes and releases the writer. The DL gate shares its deadline and token
across pending/prior consistency checks and rejection evidence. Cancellation is cooperative;
schema reading, normalisation and compilation are not all internally interruptible.
Two rule-reasoning steps also don't poll the deadline:
- the one-off full materialisation that runs when no current reasoning state is recorded;
- closing a newly declared transitive property.

A fixed limit comes before every authenticated request: a body of known length up to 64 KiB is read whole before the answer (clients that send the headers and the body apart need that), and that read waits at most 10 s. A body still missing then gets 401 if authentication refused the request's credentials, else 408.

## SPARQL Parse Error Profile

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="policy.sparql_parse_error_profile"></a>`policy.sparql_parse_error_profile` | `NRESE_SPARQL_PARSE_ERROR_PROFILE` | choice: `problem-json`, `plain-text`, `fuseki-plain-text` (lower case) | `problem-json` | start | How invalid SPARQL syntax is answered |

`plain-text` reports invalid SPARQL syntax as a plain-text error instead of a problem document ([example](http-api.md#example-a-sparql-syntax-error)); the rest of the API keeps problem+json. `fuseki-plain-text` is another name for `plain-text`.

## Endpoint Exposure

| Key | Env | Type | Default | Scope | Meaning |
|---|---|---|---|---|---|
| <a id="policy.exposure.operator_ui"></a>`policy.exposure.operator_ui` | `NRESE_ENABLE_OPERATOR_UI` | bool | `true` | start | Whether the operator console (`/ops`) is served |
| <a id="policy.exposure.metrics"></a>`policy.exposure.metrics` | `NRESE_ENABLE_METRICS` | bool | `true` | start | Whether `/metrics` is served |

## Auth Modes

All authentication settings are scope start. Secrets are never printed and are refused by `--set`.

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.mode"></a>`auth.mode` | `NRESE_AUTH_MODE` | choice: `none`, `bearer-static`, `bearer-jwt`, `mtls`, `oidc-introspection` | `none` | How requests are authenticated; each mode's required settings are below, and a missing one is a startup error |

### Static Bearer

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.bearer_static.read_token"></a>`auth.bearer_static.read_token` | `NRESE_AUTH_READ_TOKEN` | text, secret | none | The token that may read |
| <a id="auth.bearer_static.admin_token"></a>`auth.bearer_static.admin_token` | `NRESE_AUTH_ADMIN_TOKEN` | text, secret | required | The token that may administer |

### JWT Bearer

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.bearer_jwt.shared_secret"></a>`auth.bearer_jwt.shared_secret` | `NRESE_AUTH_JWT_SECRET` | text, secret | required | The HMAC secret tokens are signed with |
| <a id="auth.bearer_jwt.issuer"></a>`auth.bearer_jwt.issuer` | `NRESE_AUTH_JWT_ISSUER` | text | none | The issuer tokens must name |
| <a id="auth.bearer_jwt.audience"></a>`auth.bearer_jwt.audience` | `NRESE_AUTH_JWT_AUDIENCE` | text | none | The audience tokens must name |
| <a id="auth.bearer_jwt.read_role"></a>`auth.bearer_jwt.read_role` | `NRESE_AUTH_JWT_READ_ROLE` | text | `nrese.read` | The role that may read |
| <a id="auth.bearer_jwt.admin_role"></a>`auth.bearer_jwt.admin_role` | `NRESE_AUTH_JWT_ADMIN_ROLE` | text | `nrese.admin` | The role that may administer |
| <a id="auth.bearer_jwt.leeway_seconds"></a>`auth.bearer_jwt.leeway_seconds` | `NRESE_AUTH_JWT_LEEWAY_SECS` | integer (seconds) | `30` | Clock leeway for expiry checks |

### Proxy-Terminated mTLS

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.mtls.subject_header"></a>`auth.mtls.subject_header` | `NRESE_AUTH_MTLS_SUBJECT_HEADER` | text | `x-client-cert-subject` | The header the TLS-terminating proxy passes the client certificate's subject in |
| <a id="auth.mtls.read_subjects"></a>`auth.mtls.read_subjects` | `NRESE_AUTH_MTLS_READ_SUBJECTS` | list (`;`) | none | The subjects that may read |
| <a id="auth.mtls.admin_subjects"></a>`auth.mtls.admin_subjects` | `NRESE_AUTH_MTLS_ADMIN_SUBJECTS` | list (`;`) | required | The subjects that may administer |
| <a id="auth.mtls.trusted_proxies"></a>`auth.mtls.trusted_proxies` | `NRESE_AUTH_MTLS_TRUSTED_PROXIES` | list (`,`): addresses or ranges such as `10.0.0.0/8` | loopback | The peers that terminate TLS and may pass the subject header on. From any other peer the header is dropped before authentication, so a client that reaches the server's port directly can't claim a subject. With the proxy in another container or host, list its address or network here |

File format of the subject lists:
- string: `admin_subjects = "CN=admin,O=Example"`
- list: `admin_subjects = ["CN=admin-1,O=Example", "CN=admin-2,O=Example"]`

### OIDC Introspection

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.oidc_introspection.introspection_url"></a>`auth.oidc_introspection.introspection_url` | `NRESE_AUTH_OIDC_INTROSPECTION_URL` | text: a URL | required | The OAuth 2.0 token introspection endpoint |
| <a id="auth.oidc_introspection.client_id"></a>`auth.oidc_introspection.client_id` | `NRESE_AUTH_OIDC_CLIENT_ID` | text | none | The client id the server introspects as |
| <a id="auth.oidc_introspection.client_secret"></a>`auth.oidc_introspection.client_secret` | `NRESE_AUTH_OIDC_CLIENT_SECRET` | text, secret | none | The client secret the server introspects with |
| <a id="auth.oidc_introspection.read_role"></a>`auth.oidc_introspection.read_role` | `NRESE_AUTH_OIDC_READ_ROLE` | text | `nrese.read` | The role that may read |
| <a id="auth.oidc_introspection.admin_role"></a>`auth.oidc_introspection.admin_role` | `NRESE_AUTH_OIDC_ADMIN_ROLE` | text | `nrese.admin` | The role that may administer |
| <a id="auth.oidc_introspection.timeout_ms"></a>`auth.oidc_introspection.timeout_ms` | `NRESE_AUTH_OIDC_TIMEOUT_MS` | integer (milliseconds, no unit) | `5000` | Per introspection request |

### Graph-Level Access Control

The rules are the server's access state (users, workspaces, personal spaces, role rules; [ADR-0008](../adr/0008-users-workspaces-policies.md)), kept in `system/` of the data directory and changed through `/api/v1/access` while the server runs ([HTTP API](http-api.md#users-workspaces-and-policies-apiv1access)). Until access is enforced, every user reads and writes what its authentication grants allow.

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="auth.access_policy"></a>`auth.access_policy` | `NRESE_ACCESS_POLICY` | text: a path | none | An access policy file, imported into the access state at the first start (it turns enforcement on). Later changes of the file are reported at start and apply once imported (`POST /api/v1/access/import`); `GET /api/v1/access/export` writes the state's rules in this form |
| <a id="auth.local_logins"></a>`auth.local_logins` | `NRESE_AUTH_LOCAL_LOGINS` | bool | `true` | Whether users of the access state with a password log in (`Basic` credentials, or a session from `POST /api/v1/access/login`) besides the authentication mode; for standalone and desktop installations without an identity provider |
| <a id="auth.trusted_proxies"></a>`auth.trusted_proxies` | `NRESE_AUTH_TRUSTED_PROXIES` | list (`,`): addresses or ranges | loopback | The reverse proxies whose `X-Forwarded-For` names the client. Failed local logins are counted per user name and client address (past 10, each further attempt waits, from one second doubling up to five minutes) and per client address over all names (past 100); behind a proxy that isn't listed here, every client has the proxy's address, so list it |
| <a id="auth.workspace_base"></a>`auth.workspace_base` | `NRESE_WORKSPACE_BASE` | text | `urn:nrese:` | What workspace graph prefixes start with: a personal space is `{base}space/{user}/` (a local user's `{base}space/{user}~{id}/`), a workspace `{base}workspace/{name}~{id}/` ([HTTP interface](http-api.md#users-workspaces-and-policies-apiv1access)) |

The policy file (TOML), per role:

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

- inferred statements live in the default graph, so only users who may read it see any. `hidden` shows them none, `visible` all of them (derived from any graph), `supported` those one of whose derivations uses graphs the user may read alone: what the user could derive itself (support graph sets; reasoner v2 rulesets). The sets are computed on the first read that needs them (about four times the materialisation time: LUBM(10) over 8 graphs 2.4 s) and then updated with each commit for what it can affect; each user's view of a revision is built once, from the user's previous one (LUBM(10): a restricted read after a one-statement commit 2 ms, after a thousand 53 ms). Bulk loads, rematerialisations and schema changes compute them afresh. They are computed in the background at start and when the policy turns to `supported`, and after a rematerialisation while users read with them, so first reads find them ready or wait for that computation. With `reasoner.equality = "compact"` the sets aren't computed and such users see no inferred statements. A statement asserted only in graphs a user can't read is shown to them as inferred where it would be without that assertion (`supported`: when the graphs they may read derive it; `visible`: when it follows from the other statements without that assertion, a cycle through it supporting nothing; checked once per revision and updated with each commit for what the commit can reach; not yet with `reasoner.equality = "compact"`, where `visible` users don't see such a statement). `reasoner.support_sets` bounds the sets kept per statement, never what users see. What the sets and views keep is charged to the queries' memory (`budgets.total_query_memory`), and what computing them reads to the reading query's: a read that finds no room fails with the memory error, never with fewer statements
- `SERVICE` is a privilege of its own (it makes the server fetch URLs): with access enforced, a user calls the endpoints `federation.allow` lists only if one of its roles says `service = true` (administrators always). Without the privilege a `SERVICE` is an error, `SERVICE SILENT` one solution without bindings.
- graphs by IRI, or by IRI prefix (an entry ending in `*`): ontologies and data sets usually differ by prefix, so one entry covers a family of graphs.
- role names: a token's `scope`, `scp`, `role` and `roles` claims (`bearer-jwt`, `oidc-introspection`); `reader` for the static read token; the subject and `reader` for a listed client certificate; `anonymous` without authentication.
- user names: a token's `sub` (an introspection's `username` without one), a client certificate's subject, where they are letters, digits and `. _ @ + | : -`; a named user always has its personal space, and a user record can add roles or the administrator's right.
- a user's rights are the union of its roles' rules and its workspaces', and what it may write it may read; an explicit deny (`deny`, `default_graph = "deny"`) in any of its roles wins. Administrators are unrestricted.
- the policy grants as well as restricts: a role with a rule may query and read graphs even without the read role, and a role that may write a graph may update. So a role can edit some graphs without being an administrator.
- what it means for requests: [HTTP API, access control](http-api.md#graph-level-access-control).

## AI Query Suggestions

All AI settings are scope start. Secrets are never printed and are refused by `--set`.

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="ai.enabled"></a>`ai.enabled` | `NRESE_AI_ENABLED` | bool | `false` | Whether suggestions are offered; while `false` the provider is off whatever `ai.provider` says |
| <a id="ai.provider"></a>`ai.provider` | `NRESE_AI_PROVIDER` | choice: `disabled`, `gemini`, `openrouter` (lower case) | `disabled` | Who makes the suggestions; `gemini` and `openrouter` need their API key (a startup error without it) |
| <a id="ai.model"></a>`ai.model` | `NRESE_AI_MODEL` | text | `gemini-2.5-flash` | The provider's model |
| <a id="ai.timeout_ms"></a>`ai.timeout_ms` | `NRESE_AI_TIMEOUT_MS` | integer (milliseconds, no unit) | `20000` | Per suggestion request |
| <a id="ai.max_suggestions"></a>`ai.max_suggestions` | `NRESE_AI_MAX_SUGGESTIONS` | integer | `4` | Suggestions per request |
| <a id="ai.system_prompt"></a>`ai.system_prompt` | `NRESE_AI_SYSTEM_PROMPT` | text | a built-in prompt asking for strict JSON and safe read queries | The system prompt of suggestion requests |

### Gemini

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="ai.gemini.api_key"></a>`ai.gemini.api_key` | `NRESE_AI_GOOGLE_API_KEY` | text, secret | none (`GOOGLE_API_KEY` is the fallback when `NRESE_AI_GOOGLE_API_KEY` is not set) | The Gemini API key |
| <a id="ai.gemini.api_base"></a>`ai.gemini.api_base` | `NRESE_AI_GOOGLE_API_BASE` | text: a URL | `https://generativelanguage.googleapis.com` | The Gemini API's base URL |

### OpenRouter

| Key | Env | Type | Default | Meaning |
|---|---|---|---|---|
| <a id="ai.openrouter.api_key"></a>`ai.openrouter.api_key` | `NRESE_AI_OPENROUTER_API_KEY` | text, secret | none | The OpenRouter API key |
| <a id="ai.openrouter.api_base"></a>`ai.openrouter.api_base` | `NRESE_AI_OPENROUTER_API_BASE` | text: a URL | `https://openrouter.ai/api/v1` | The OpenRouter API's base URL |
| <a id="ai.openrouter.site_url"></a>`ai.openrouter.site_url` | `NRESE_AI_OPENROUTER_SITE_URL` | text | none | The site URL sent to OpenRouter |
| <a id="ai.openrouter.app_name"></a>`ai.openrouter.app_name` | `NRESE_AI_OPENROUTER_APP_NAME` | text | none | The application name sent to OpenRouter |

## Ownership Rules

- typed runtime defaults and validation live in owning crates
- external config parsing and precedence live in `crates/nrese-server/src/config/`
- every setting (file key, older keys, environment name, kind, description) is declared once in `crates/nrese-server/src/config/settings.rs`; a test checks that this reference names every key and environment variable
- do not document runtime knobs in multiple operator docs
