# Engine and feature gaps before the frontend (2 October 2026)

The owner's order of 2 October: first the bug hunt, then the remaining engine and feature
gaps, until a full, industry-leading engine API stands; the one consolidated frontend
comes after, on that API ([vision](../spec/00-vision-and-scope.md)). This plan lists what
is left, in that order. Status of each capability: the
[capability matrix](../spec/06-target-capability-matrix.md); designs:
[research designs](2026-10-02-research-designs.md), [hardware and scaling](2026-10-01-hardware-and-scaling.md).

## G1. Bug hunt (running)

- The random differential and property tests draw new cases with `NRESE_FUZZ_SEED`;
  `scripts/fuzz-campaign.sh` runs them over many seeds on an idle machine and keeps every
  failing seed (SPARQL against the reference evaluator, reasoner closures, index models,
  the inferred stack, compact equality, incremental SHACL).
- Every failure: a fixed-seed regression test, the fix, the campaign again.
- Where a generator rarely produces non-empty answers or never reaches an operator, the
  generator improves (the audit of 30 September found that the old random test had 10 %
  non-empty results and missed four bugs).
- **Found and fixed so far (2 October):** the `ORDER BY` order was not total over literals
  of different kinds; a `LIMIT` cut through equal sort keys; compact equality lost the
  copies of a statement deleted from the default graph but asserted in a named one; a
  path between two constants answered one solution where its bag has several
  (alternatives, sequences); explanations of inferences differed between runs. Each has
  a fixed-seed regression test; the oracle's own mistakes (SAMPLE, CONSTRUCT with LIMIT,
  NOW, row order) were fixed alongside.

## G2. One engine API

Every capability an operation of the core, reachable the same way by every connector and
by the coming frontend; the protocols only translate.

- **Done so far (2 October):** namespaces and client transactions (sessions) in the store;
  every `/dataset/…` capability for every repository under `/api/v1/repositories/{id}`;
  JSON routes for repositories (create, read, remove), namespaces, sessions, imports,
  rematerialisation and explanations of inferences; users, workspaces, personal spaces,
  role rules, their history and local logins in the core (G2b, ADR-0008 accepted).
- **State in the core.** Done: namespaces and client transactions (sessions) moved from
  the RDF4J adapter into the store; access policies, users and workspaces (G2b). Next:
  repository settings, the shapes graph and rulesets as managed store objects.
- **One management API.** A versioned JSON API over HTTP for what the protocols don't
  cover: repositories, imports and exports, namespaces, sessions, rulesets and reasoning
  state, shapes and validation, explanations, policies, users and workspaces, backups,
  statistics. One typed description of it (OpenAPI), generated from the server's types,
  so clients don't drift.
- **G2b. Users, workspaces and policies in the store.** User control first: a workspace
  is a set of graphs with members and roles (owner, editor, viewer); each user has a
  personal space they alone write; policies are stored as RDF, changed through the API
  with an author and a reason, and their history is queryable. Identity from tokens
  (OIDC, JWT, client certificates) or, for standalone and desktop use, local users. The
  policy file stays as import and export.
- **Connectors.** DMW and ResearchSpace end to end (their full flows as tests), then the
  widest reach: RDF4J and GraphDB Workbench clients, Jena, rdflib, Protégé.

## G3. Reasoning that leads

- **Explanations of inferences:** why a statement holds, as derivation trees through the
  API (consistency rejects are explained already), with the support graph sets the access
  control needs to show an inference to whoever may read its premises.
- **OWL 2 DL:** classification, consistency and explanation beyond the materialisable
  profiles. A design from the research (tableaux, hypertableaux, consequence-based
  calculi, their combination with materialisation, modular and incremental
  classification), then the build, measured on the ORE corpora against HermiT, Konclude
  and ELK.
- **Equality:** incremental merges and splits of `sameAs` classes, joins on
  representatives with late expansion (W4 stage C), strict and canonical answers.

## G4. Queries

Join ordering across basic graph patterns, paths and subqueries; characteristic sets;
aggregates over products without enumerating them (BSBM BI q4); paths planned from their
bound end. Measured with Sparqloscope, WatDiv and BSBM.

## G5. Vector similarity search

The design of §2 of the research designs: several index types and codecs, filters from
SPARQL patterns, models and metrics configurable.

## G6. Hardware and scale-out

H1–H7 of the hardware plan: hardware profile, SIMD kernels, NUMA and morsels, larger than
RAM, GPUs where a workload gains, read replicas and partitioning across machines.

## G7. Smaller items

SPARQL 1.2 / RDF 1.2 `version` announced in responses; error recovery in the RDF/XML and
JSON-LD parsers; multi-architecture Docker images; the benchmark kits still missing (LDBC
SPB, Sparqloscope, WatDiv), a second oracle, soak and fuzzing of the HTTP surface.
