# ADR-0012: Query depth: sequences n-ary in the algebra, one nesting limit, the caller's stack

Status: proposed (12 October 2026).

## Context

A long left-nested `UNION` chain overflowed the stack after parsing (bench agent,
7 October): union-1000 needed more than 2 MiB, union-5000 overflowed at 8 MiB. The server
runs its queries on tokio's threads, 2 MiB each, so one request of about 150 KB could abort
the process.

Three causes:

- **Representation.** `nrese-sparql-syntax`'s algebra held `UNION`, joins, path sequences
  and alternatives, and runs of `BIND`s as binary or unary nodes, built as a fold. A tree
  of them is as deep as the query is long. The same goes for a SELECT's expressions, GROUP
  BY's, and the parts the executor's triple-term rewrite binds. The logical plan already
  held joins and unions as lists, but lowered them back to chains.
- **Passes.** About 90 functions in the SPARQL crates, the reference evaluator, SHACL and
  the store recurse over the algebra on their own: the optimiser's rewrites, EXPLAIN, the
  cache key, QL rewriting, the writer, evaluation. Each recursed once per element of a
  chain.
- **The limit.** The parser's nesting limit (128) counted braces, brackets and expression
  chains. A chain is flat in braces and deep in the algebra.

## Decision

1. **Sequences are n-ary and flattened when built.** `GraphPattern::Union` and `Join`
   hold a list of patterns, and `PropertyPathExpression::Sequence` and `Alternative` a list
   of paths. `GraphPattern::Extend` holds its bindings in order, each seeing the ones
   before it: a run of `BIND`s, a SELECT list or GROUP BY's expressions are one node.
   The constructors flatten: `union_all`, `join_all`, `PropertyPathExpression::sequence`,
   `alternative` and `GraphPattern::extend`. `&&` and `||` are balanced trees
   (`Expression::balanced`), log n deep. The plan's lists map one to one. Every pass
   iterates over a node's children, so it recurses as deep as the query nests, never as
   long as it is. That holds for the rewrites too: the triple-term rewrite, QL rewriting,
   eager aggregation and filter pushdown add a level or two, not a level per element.
2. **What nests stays nested.** `OPTIONAL`, `MINUS` and `LATERAL` each take everything
   before them in their group as their left side, and they aren't associative. They stay
   binary, as do `FILTER` over a group, `GRAPH`, `SERVICE`, subqueries and their
   modifiers, `EXISTS`, and expressions. Their depth is the query's real nesting.
3. **One limit counts the algebra.** `DEFAULT_MAX_NESTING` bounds how deep the algebra a
   query builds may nest. The parser counts as it reads: braces, brackets, expression
   chains, and each `OPTIONAL`, `MINUS`, `LATERAL` and `BIND` (unless the `BIND`
   continues a run of them) that wraps its group. Once a query or update is parsed, it
   measures `GraphPattern::depth`, which counts the levels a subquery's modifiers add.
   Past the limit, the query is a syntax error ("nested deeper than N levels"), HTTP 400.
   The shared walker (`nrese_sparql_syntax::visit`) and `depth` use a stack of their own,
   so searching or measuring a tree never recurses.
4. **The caller's stack, with no stack of its own for queries.** Queries run on their
   caller: in the server on tokio's threads (2 MiB), and elsewhere on the mutation
   pipeline's, Rayon's, tests' and embedding applications' threads. The limit is the
   guarantee; the stack size is not raised.
   - Any stack overflows at some depth, so a larger stack can't replace the limit. It
     would only allow a larger one.
   - There is no single query thread to give a larger stack. A stack size set in the
     server's runtime would leave every other caller of the store on its own 2 MiB.
   - With sequences flat, the depth that remains is what a query nests. The limit of 128
     covers that with room: at the limit, the deepest shape takes 35 % of 2 MiB in
     release (measured below).
   - If a real query ever needs deeper nesting, first lower the bytes the deepest pass
     takes per level. A larger stack then belongs to whoever owns the thread: the hardware
     profile's per-workload pools (H1, [hardware-and-scaling](../design/hardware-and-scaling.md)).

## Evidence

`depth_tests::stack_per_level` (`nrese-sparql`, ignored, a measurement) finds the smallest
stack, to 1 KiB, that each shape answers on through parse, plan, EXPLAIN and evaluation
with the result cache, one process per run. It measures each nested shape at the deepest
the parser accepts (n) and at 4 n with the limit lifted. Measured on office-pc on
12 October; release, with the test profile in brackets:

| Shape | n at the limit | KiB at n | Bytes per step |
|---|---|---|---|
| `OPTIONAL` after `OPTIONAL` in a group (`MINUS`, `LATERAL` alike) | 126 | 722 (594) | 5,632 (4,591) |
| `FILTER EXISTS` nested | 63 | 543 (492) | 8,403 (7,547) |
| subqueries nested | 63 | 373 (393) | 5,629 (6,111) |
| `OPTIONAL` nested, `GRAPH` nested, groups nested | 126–128 | 262–311 (306–309) | 2,000–2,272 (2,352) |
| `{ } UNION { … }` nested | 63 | 258 (245) | 3,938 (3,711) |
| `BIND` after a triple, repeated | 63 | 233 (241) | 3,537 (3,651) |
| brackets; `1 + 1 + …` | 125–126 | 254–278 (227–380) | 1,886–2,129 (1,679–2,944) |

The worst shape at the limit takes 35 % of 2 MiB. Sequences take the same stack however
long: union, joined groups, `&&`, `||`, path sequences and alternatives, `VALUES`, `BIND`
runs and SELECT lists need 29–44 KiB at 500 and at 2,000 (49 KiB in the test profile).
Before, union-1000 needed more than 2 MiB.

The test profile compiles the SPARQL crates at `opt-level` 1 (`Cargo.toml`): its bytes per
level are 0.8 to 1.4 times release's, well inside the margin, so the guard's 2 MiB thread
stands for the server's.

## Consequences

- Every consumer of the algebra changed its `Union`, `Join`, `Extend`, `Sequence` and
  `Alternative` arms: the syntax crate, `nrese-sparql`, the reference evaluator, SHACL, the
  store and the Oxigraph comparison. A pass written for a binary node now loops.
- The cache key, EXPLAIN's steps and the plan show one node where a chain showed several.
- The guard (`nrese-sparql`, `depth_tests`): chains of 5,000 of every sequence answer on a
  2 MiB thread, through parse, plan, EXPLAIN and evaluation with the result cache, and so
  does an update. Each nested shape answers at the deepest the parser accepts and is
  refused at 5,000.
