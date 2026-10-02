# ADR-0006: Built-in full-text search; ResearchSpace is adapted to NRESE

Status: accepted (2026-09-25). Index implementation: tantivy (roadmap decision D3).

## Context

ResearchSpace (v4) is the main user-facing platform on top of the store. Its shipped keyword-search templates use Blazegraph's `bds:search` magic predicate. The team has agreed to adapt ResearchSpace to NRESE, so we aren't bound to any vendor's text-search syntax. QLever (`ql:contains-word`) and GraphDB (Lucene connectors) both treat text search as a first-class feature.

## Decision

- **Engine-side text index.** A tantivy index per repository over literal terms, keyed by `TermId`, maintained incrementally from commit deltas: BM25 scoring, fuzzy and phrase queries, prefix queries for autocompletion, language-aware tokenisation. Recovery rebuilds or replays the index from the WAL, so it can't diverge from the store.
- **Query surface.** A native SPARQL extension function family under the `nrese:` namespace (`nrese:textSearch`, with score and snippet bindings), plus GraphDB `luc:` compatibility predicates (query, score, soundex).
- **Compatibility shim.** A shim that accepts the Blazegraph `bds:search` pattern (`?label bds:search "…"; bds:minRelevance …; bds:matchAllTerms …`). ResearchSpace's stock templates then work before any adaptation, and adapted templates can move to the native syntax.
- **ResearchSpace adaptation** is done in ResearchSpace's configuration and templates: repository config and search templates. The server doesn't special-case the ResearchSpace client.

## Consequences

- Text search lands in `nrese-engine` (index maintenance) and `nrese-sparql` (query binding). It isn't in the server.
- The shim is a compatibility feature with its own tests; it may be retired once ResearchSpace uses the native syntax.
