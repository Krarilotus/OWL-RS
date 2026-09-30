# SHACL validation: design

Status: design for phase C of the [parity plan](../plan/2026-09-30-graphdb-parity-plan.md) (30 September 2026). Slice C1a is done; the others are planned.

## 1. Goal and scope

Validate RDF data against SHACL shapes, natively and fast enough to run on every commit.

- **In scope:**
  - SHACL Core: every constraint component, every property path, all four target kinds, severities, deactivation, messages.
  - The validation report, as RDF and as JSON.
  - A commit gate with incremental validation (C2).
  - SHACL-SPARQL constraints and targets (C3).
- **Out of scope for now:** SHACL Advanced Features (rules, functions, node expressions) and SHACL-JS. SHACL rules overlap with the reasoner's rule IR and would be compiled into it, not interpreted here.

## 2. Where it lives

| Part | Owner | Why |
|---|---|---|
| Shape compiler, validator, report | new crate `nrese-shacl` (L2) | SHACL semantics are one concern |
| Reading data and shapes | `nrese_sparql::ReadView` | Already implemented by snapshots and open transactions, so the same validator runs on committed data and inside a commit |
| Value comparison (`sh:minInclusive`, `sh:lessThan`, …) | `nrese_sparql::value` | SHACL defines these through SPARQL's operators; one implementation |
| SPARQL constraints (C3) | `nrese-sparql`, called by `nrese-shacl` | Same reason |
| When to validate, and what a failure means for a commit | `nrese-store` (mutation pipeline) | Gates are store orchestration |
| Endpoints, formats, configuration | `nrese-server` | Transport and policy |

`nrese-shacl` depends on `nrese-sparql` (never the reverse) and on `nrese-engine` for ids and patterns. It adds no external dependency: `oxrdf`, `oxsdatatypes` and `regex` are already in the workspace.

## 3. Data model

- **Shapes live in the repository,** in one graph. Data and shapes share the dictionary, so a compiled shape holds term ids and most checks compare ids without decoding a term.
- **What is validated** is a graph selection plus a read model:
  - graphs: the default graph, one named graph, or every graph except the shapes graph (the default for the commit gate);
  - read model: asserted and inferred statements (the default, so `sh:class` sees inferred types), or asserted only.
- A shapes graph given with a request (validate-only) is loaded into a scratch graph of a snapshot-local overlay; it isn't stored. (C1b.)

## 4. Compilation

`compile(view, shapes_graph) -> Shapes` reads the shapes graph once and produces a program:

- **Shapes:** node or property shape (a shape with `sh:path` is a property shape), its targets, constraints, severity, messages and whether it is deactivated.
- **Which nodes are shapes:** subjects of a target predicate, nodes typed `sh:NodeShape` or `sh:PropertyShape`, and every node a shape-valued parameter points to (`sh:node`, `sh:property`, `sh:not`, the members of `sh:and`/`sh:or`/`sh:xone`, `sh:qualifiedValueShape`). Compilation follows those references, so unreferenced untargeted nodes cost nothing.
- **Paths** become a small tree: predicate, inverse, sequence, alternative, zero-or-more, one-or-more, zero-or-one.
- **Parameters are prepared once:** regular expressions compiled, bounds parsed to values, `sh:in` lists turned into id sets, lists walked and checked.
- **Ill-formed shapes are errors,** each naming the shape and the parameter (a path that isn't a path, a non-numeric `sh:minCount`, a broken list). A shapes graph with errors isn't used: validating with half a shape would report conformance that isn't there.

## 5. Validation

1. **Focus nodes** of each targeted, active shape:
   - `sh:targetNode`: the node;
   - `sh:targetClass` and implicit class targets: instances, through `rdf:type/rdfs:subClassOf*` in the data;
   - `sh:targetSubjectsOf`, `sh:targetObjectsOf`: one index scan each.
2. **Value nodes:** the focus node itself (node shape) or the path's values (property shape), as a set of ids.
3. **Constraints** are checked per component; each produces results as the specification says (per value node, or per focus node for cardinality, `sh:hasValue`, `sh:uniqueLang`, qualified counts).
4. **Nested shapes:**
   - `sh:property` reports the nested shape's own results;
   - `sh:node`, `sh:not`, `sh:and`, `sh:or`, `sh:xone` and `sh:qualifiedValueShape` only ask whether the value conforms.
5. **Recursion:** the specification leaves recursive shapes undefined. A node being checked against a shape it is already being checked against is taken to conform (the common reading). This is a stated choice, not a guarantee.

**Where the time goes, and what the design does about it:**

- **Id-level checks.** `sh:nodeKind`, `sh:hasValue`, `sh:in`, `sh:class`, `sh:equals`, `sh:disjoint` and most of `sh:datatype` read only ids: the engine's id encodes the term kind, and inline literal kinds name their datatype. Terms are decoded only for value-level components (ranges, lengths, patterns, languages).
- **Class membership** uses the subclass closure of each class, computed once per validation.
- **Focus nodes are independent,** so each shape's focus nodes are validated in parallel.
- **Set-at-a-time evaluation (C1c).** For the dominant case, a property shape with a predicate path over a class target, cardinality and value-type checks become one merge of two sorted scans instead of one lookup per focus node. Measured in the perf lab before and after.

**Readings the specification leaves open, as implemented:**

- A boolean parameter (`sh:closed`, `sh:deactivated`, `sh:uniqueLang`, `sh:qualifiedValueShapesDisjoint`) is on only for the literal `true`; `"1"^^xsd:boolean` doesn't switch it on (the W3C suite pins this).
- A path node that is an RDF list is a sequence path, whatever else it says (also pinned by the suite).
- An implicit class target needs the shape to be typed `sh:NodeShape` or `sh:PropertyShape` and `rdfs:Class` or `owl:Class`.
- `sh:datatype` rejects ill-formed literals of the XSD types it knows (numbers with their ranges, booleans, dates, times, durations); unknown datatypes accept any lexical form.

## 6. The report

`ValidationReport { conforms, results }`; each result has the focus node, path, value, source shape, source constraint component, severity and messages, as ids and decoded on output.

- `conforms` is false if there is any result, of any severity (as the specification defines it).
- Output: an RDF graph (`sh:ValidationReport`), and JSON for the console and the operator API.

## 7. Commit gate and incremental validation (C2)

- **Position:** after reasoning, before the commit, inside the transaction. A rejected commit changes nothing (the same contract as consistency rejection, [reasoning-semantics.md](../spec/reasoning-semantics.md)).
- **Policy, configurable:**
  - `off`;
  - `report`: validate and record, never reject;
  - `enforce`: reject a commit that adds results at or above a severity (default `sh:Violation`).
- **Incremental:** a commit can only change the results of focus nodes it touches. From the inserted and deleted statements (asserted and inferred), the gate computes the affected focus nodes per shape:
  - subjects and objects of changed statements, carried backwards along each shape's paths to the focus nodes that reach them;
  - nodes whose target membership changed (a type added or removed).

  It validates only those, against the transaction's state.
- **Existing invalid data** (shapes added later, data loaded in bulk): as with inconsistency, the store reports it and isn't bricked. A commit is rejected for results it introduces, not for ones that were already there.
- **Changing the shapes graph** recompiles the shapes and revalidates in full.
- **Cancellation:** the gate polls the same stop check as commit-path reasoning.
- **Gate:** incremental validation equals full revalidation on random deltas (differential test), and its cost follows the number of affected focus nodes.

This slice needs the per-repository configuration of D1 first.

## 8. Evidence

- **Conformance:** the W3C SHACL test suite (`w3c/data-shapes`, pinned, fetched by `scripts/fetch-w3c-tests.sh`): 98 Core tests for C1 (all pass), 23 SHACL-SPARQL tests for C3. As for SPARQL, every known failure is listed with its reason, and the run fails on a new failure or on a listed test that now passes.
- **Reports are compared** by their result sets: focus node, path (structurally), value, source shape, component and severity. Messages aren't compared (the specification leaves them free).
- **Differential:** incremental against full validation (C2).
- **Licensed references** (GraphDB's SHACL) are compared locally only.

## 9. Slices

| Slice | Scope | Done when |
|---|---|---|
| C1a ✅ | `nrese-shacl`: compiler, all Core components, paths, targets, severities, report | W3C Core suite passes: 98 of 98, no listed failures |
| C1b | Store operation and server endpoint: validate the repository, or a posted shapes graph, and return the report; configuration | HTTP tests; the report round-trips as RDF |
| C1c | Parallel and set-at-a-time evaluation | Same reports as C1a on the suite and on random data; perf-lab numbers recorded |
| C2 | Commit gate, incremental validation, policy, cancellation | Differential test; a rejected commit changes nothing |
| C3 | SHACL-SPARQL constraints and targets | W3C SHACL-SPARQL tests |
