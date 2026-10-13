# The quarantine of invalid statements

What a bulk load does with statements it can't read (the owner's decision of 13 October
2026, after the Claros case: one IRI with a `%` that starts no escape stopped a load of
millions of good statements). Code: `nrese_store::quarantine`, the bulk loader
(`nrese_store::bulk_load`), the parsers' `SkippedStatement` (`nrese_rdf_io::error`).

Not to be confused with the *reasoning* quarantine (`reasoning_state`, `/readyz` 503): a
store whose asserted data is inconsistent. This one holds statements that are not data.

## 1. The rule

1. A bulk load loads every valid statement. Each invalid one goes into the repository's
   quarantine, verbatim: its text, file, the document's line, the reason, and the load.
2. Quarantined statements are not data: queries, reasoning, exports and statistics don't
   see them. By construction: they never reach the engine.
3. The load warns: its report carries the count, the load's id and the first examples
   (file, line, reason; at most 5, in the files' order whatever the threads did); the CLI
   prints them, the import endpoints return them, the log names the first 20.
4. Resolution later: list, export, fix, then re-admit through the commit pipeline, or
   discard. Repairs only on request, recorded in the statement's resolution.
5. `strict` stops at the first invalid statement, and nothing of the load is committed (the
   behaviour before). Writes other than bulk loads (SPARQL Update, Graph Store, the
   statements API, sessions, `TELL`) stay strict: they answer their client at once.
6. `store.invalid_statements = "quarantine" | "strict"`, default `quarantine`; a load may
   ask for either (`--strict`, `--quarantine`; `invalid=` on the import endpoints).

## 2. What is kept

The parser reports the statement an error is in (`RdfSyntaxError::statement`): its span in
the document, from its first character to where parsing resumed after it, the `@base` and
`@prefix` directives in effect, and its TriG block's graph. The line formats give the
line; Turtle and TriG the statement up to its `.` (the recovery's own unit). The loader
reads the span's bytes back from the file: built on the error path only.

A record (`QuarantinedStatement`, one JSON line):

| field | |
|---|---|
| `id` | `<load>.<file>.<offset>`: unique, the same whatever the threads did |
| `load`, `file`, `line`, `column` | the load, the file as given, where the statement starts (from 1) |
| `reason` | the parser's message |
| `format` | the file's format, as its extension |
| `text` | the statement verbatim; `truncated` past 1 MiB, `not_utf8` where its bytes aren't UTF-8 (then lossy) |
| `context` | the directives in effect (Turtle, TriG), as `@base`/`@prefix` lines |
| `graph` | its TriG block's graph |
| `target` | the load's graph for a triple format (none: the default graph) |
| `base` | the base IRI the file was read with |
| `blank_nodes` | the prefix the load gave its blank-node labels |

With `context`, `graph`, `target` and `base` the text reads on its own, into the place the
load would have put it. RDF/XML and JSON-LD give the line and reason only (their recovery
skips an element whose end the parser knows only after the error): re-admitting one takes
the fixed text.

## 3. Where it is kept

`quarantine/` of the repository's data directory, beside `namespaces.json`:

- `load-<id>.jsonl`: one file per load that quarantined something, written as the load
  goes (`load-<id>.partial`) and renamed once the load committed. A load that fails or is
  cancelled removes its file; a `.partial` found at a start is from a load that didn't
  commit (a crash between the engine's commit and the rename loses that load's
  quarantine, not its data) and is removed.
- `resolutions.jsonl`: append-only, one record per resolved statement (`outcome`
  `readmitted` with the commit's revision, or `discarded`; when; the repairs; the text
  admitted where it isn't the quarantined one; a note). A record torn by a crash is
  skipped: that resolution didn't happen (a re-admission's commit did, so the statement is
  listed open again although its data is in: re-admitting it again adds nothing).

Both survive restarts; checkpoints and the WAL don't touch them. Image backups and
restores carry the engine's files only, as for the namespaces: a restored store starts
with the quarantine its directory has. A store in memory keeps them in memory.

Costs nothing without errors: the directory and a load's file come with its first
invalid statement. On the success path a load creates one collector (its id and file list),
the Turtle parser copies four counters per statement (where it starts, resolved to a line
only for an error), and the line formats do nothing new.

## 4. Re-admission

`quarantine::readmit(pipeline, requester, ticket, id, Readmission)` builds the statement's
document (its directives, its TriG block, the text kept or the fix given, the repairs
asked for applied in order), parses it (still invalid: an error, the statement stays
open), and applies it as `MutationCommand::Statements` (an `Add` of that payload, into the
load's graph for a triple format) through `MutationPipeline::apply`: the same gates,
access control and commit as any write. Committed, the resolution is recorded with the
revision; refused by a gate, the statement stays open. One resolution per statement.

Repairs (`Repair`), on request only:

- `percent-encode-iris`: inside `<…>`, a `%` that starts no escape becomes `%25`, and a
  character IRIs can't hold (space, `"`, `{`, `}`, `|`, `^`, `` ` ``, controls) its
  percent-encoded UTF-8; strings and comments stay as they are. The Claros case.

Blank nodes: the load gave its labels a prefix (`blank_nodes`), so the statement's
`_:b1` is the store's `_:<prefix>b1`. The pipeline's payloads get fresh blank nodes
(`RdfPayload`, like `INSERT DATA`); a statement with blank nodes is refused unless the
request accepts fresh ones (`fresh_blank_nodes`). To keep them the load's, the pipeline's
statements payload would need RDF4J's `preserveNodeId` for adds (the v2.0 core thread's
gated write path; proposed, not done).

## 5. The API proposed

Store (done): `StoreService::quarantine()` with `open_count`, `list(QuarantineFilter)`
(by load, open/resolved/all, offset and limit), `get(id)`, `export(filter, writer)` (JSON
lines, streamed), `discard(ids, note)` (all or none); `quarantine::readmit`.

HTTP (proposed, administrators, per repository):

| | |
|---|---|
| `GET …/repositories/{id}/quarantine?load=&state=open\|resolved\|all&offset=&limit=` | the records as JSON (`{"open": n, "items": [...]}`), each with its `resolution` once resolved |
| `GET …/quarantine/export?…` | the same as JSON lines (`application/x-ndjson`), streamed |
| `GET …/quarantine/{sid}` | one record |
| `POST …/quarantine/{sid}/readmit` | body `{"text"?, "repairs"?: ["percent-encode-iris"], "fresh_blank_nodes"?, "note"?}`: 200 with the resolution; 400 still invalid (the parser's message) or blank nodes; 409 already resolved; a gate's rejection as for any write (422 with its explanation) |
| `POST …/quarantine/discard` | body `{"ids": [...], "note"?}`: 200 with the count; 409 if one isn't open, and nothing is discarded |

`…/info` and `/summary` would carry `quarantined` (open count) beside the statistics, not
in them. CLI (offline, the same lock as `load`): `nrese-server quarantine list|export|
discard ID…|readmit ID [--repair percent-encode-iris] [--text TEXT]`.

## 6. Open

- The HTTP endpoints and CLI of §5, once the API is agreed.
- Blank nodes kept the load's on re-admission (§4): the write path's extension.
- RDF/XML and JSON-LD text: the recovery would have to return the element's span.
- A Turtle-load case in the fast suite: the cost claim of §3 is measured on N-Triples
  loads and by a parse timing of a generated Turtle file (performance.md lab log).
