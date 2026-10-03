# Reasoner provenance: support counts and support graph sets (design, 3 October 2026)

**Why:**
- [Research designs §6](../plan/2026-10-02-research-designs.md) asks for support counts always, provenance at a chosen level, and maintenance chosen per commit.
- §4 asks for inferred statements to be visible to a reader who may read the graphs of at least one of their derivations (support graph sets, decided on 2 October).
- The [roadmap](../plan/2026-10-02-roadmap.md) puts both in phase 1, under "reasoning that leads".

## Today

- The inferred stack holds the closure with set semantics: a fact is there or not.
- Commits maintain it by DRed with backward/forward proofs (B/F; `nrese_reasoner::v2::delta`). Overdeletion asks the prover whether a candidate keeps a proof avoiding the deleted facts. It restarts as plain DRed when a schema or list fact changes.
- Explanations are searched backwards on demand.
- Inferred statements are all-or-nothing per access policy (`GraphAccess::inferred`).

## What the field does (the state to reach)

- **Counting** (Gupta, Mumick and Subrahmanian 1993) keeps per fact the number of its derivations. It is exact for non-recursive programs; for recursive ones, counts alone can't tell a self-supporting cycle from a real derivation.
- **Hu, Motik and Horrocks** (AAAI 2018, *Optimised Maintenance of Datalog Materialisations*; AIJ 2019 with Motik and Nenov) combine them. Each fact counts its derivations by rules that are *non-recursive* with respect to it, and the recursive part is repaired by DRed or B/F. A deleted premise removes a fact without a proof search when a non-recursive derivation survives. In their measurements, counting nearly always pays (UOBM, 1,000 deletes: B/F with counting 1.2 s, DRed with counting 179 s). Which recursive repair wins depends on the workload, and rematerialising the affected part wins once deletes invalidate much of a recursive closure.
- **Modular materialisation** (Hu, Motik and Horrocks, AIJ 2022) handles transitive closure, class hierarchies and equality by specialised modules rather than generic rule firings. NRESE already has the transitive and equality modules.

## Recursion at the right granularity

At the level of predicates, almost every OWL 2 RL rule over RDF is recursive (`rdf:type` feeds `rdf:type`). NRESE's ground program, however, bakes the schema into the rules: `cax-sco` becomes `?x a :C → ?x a :D` per subclass axiom. So recursion is decided over **keys**:
- an atom's key is its predicate;
- for `rdf:type` with a constant class, the key is the pair (`rdf:type`, class);
- an atom with a variable predicate or class has the wildcard key, which depends on every key of its predicate (all keys if the predicate is a variable).

The keys' dependency graph (body key → head key) is condensed into strongly connected components. A rule instance is **non-recursive** if its head key's component is none of its body keys' components. On ontologies, class hierarchies without equivalence cycles are then non-recursive, as Hu et al.'s class-level programs are.

## What is recorded, by level

`reasoner.provenance` (a repository setting):

| Level | Recorded per inferred fact | Serves |
|---|---|---|
| `none` (default) | nothing | the smallest store; maintenance by the one-step check, B/F and DRed |
| `acl` | its minimal support graph sets | inferred statements under graph access (§4) |

(A `count` level was built and measured, then dropped: see "Measured" below.)

`witness` (one derivation per fact, for constant-time explanations) and `full` (every derivation) come later, if explanations need them.

## Where it lives: a support stack

A third engine stack beside the asserted and inferred ones:
- **Layout:** `DefaultGraph`, holding entries `(s, p, o, tag)`, with the same runs, MVCC, WAL, checkpoints and compaction, and set semantics.
- **The tag** is a `TermId` of a reserved inline kind, `Set(id)`: one entry per minimal support graph set.
- **Reading:** an SPO-prefix scan gives a fact's provenance, adjacent and sorted.
- **No heap map:** per-fact provenance stays out of the heap (serving memory is a phase 1 goal). The runs are compressed and mapped like the others.
- **The set table** maps interned graph-id sets to stable ids, kept with the engine's metadata and checkpointed. Sets are sorted id lists, interned so that facts share them.

## Maintenance

- **Materialisation (batch):** semi-naive evaluation finds each rule instance once; each non-recursive instance adds one to its head's count. At level `acl`, a fact's sets are the minimal ones among the unions of its premises' sets over its derivations, iterated to a fixpoint for recursive derivations (monotone and finite).
- **Inserts:** new non-recursive instances add to counts; new derivations add candidate sets, and minimality is kept.
- **Deletes:**
  - overdeletion enumerates the lost rule instances (as today);
  - each lost non-recursive instance subtracts one from its head's count;
  - a candidate whose count stays above zero keeps a derivation, so it is neither overdeleted nor propagated, and the prover isn't asked;
  - the rest goes to B/F or DRed as today.

  Counting a lost instance twice, when it is found through two deleted premises, only over-subtracts. That leads to more overdeletion, which rederivation repairs; it is safe. Missing a lost instance would not be safe, and semi-naive overdeletion finds every one at least once. At level `acl`, the sets of the facts the commit touched are recomputed and the changes propagated forward until stable.
- **The choice per commit** (`reasoner.maintenance = auto | bf-count | dred-count | remat`): `auto` estimates the impact from the deleted facts' fan-out and the counts. Small commits use B/F with counting; a commit that invalidates much of a recursive closure rematerialises the affected modules.

## Reading under graph access

- **Visible sets:** for a reader with readable graphs R, the set ids S with S ⊆ R are computed lazily per (policy epoch, R) as a bitmap over set ids.
- **Visible facts:** an inferred fact is visible iff one of its `Set` entries is in the bitmap. Inferred scans join the support stack on the fact's key, which is sorted the same way.
- **No filter needed** for readers who may read every graph, and in the compatibility mode (`GraphAccess::inferred` as today).
- **The cap:** per fact at most `reasoner.support_sets` minimal sets are kept (default 4, the smallest first). Beyond the cap, a fact may stay hidden from a reader one of whose derivations it could see. That is the safe direction, and it is documented and counted.

## Measured (3 October): counts aren't needed for maintenance

Step 1 built counting (`v2::supports`, `delta::update_counted`), exact against fresh counts
over 40 random seeds. It also showed a cheaper way to the same effect.
- **The one-step check:** without stored counts, a candidate is kept when a non-recursive rule still derives it in *one step* from what is left. The premises lie in lower components; if one goes later in the commit, its loss brings the candidate back, as with counting.
- **In parallel:** that check and the B/F proofs of the rest run in parallel, with a prover per worker.

Measured on LUBM(100) (13.4 M asserted, 8.7 M inferred statements; the rematerialisation takes 2.3 s), deletes per commit:

| Deleted | Before (B/F alone) | One-step check, parallel | Stored counts |
|---|---|---|---|
| 1 | 0.61 ms | 0.48 ms | 0.46 ms |
| 1,000 | (LUBM(10): 111 ms) | 6.7 ms | 8.8 ms |
| 10,000 | (LUBM(10): 134 ms) | 55 ms | 74 ms |
| 100,000 | | 129 ms | 174 ms |

Stored counts are slower at scale: hash-map updates on every insert and delete, and an initial count (4.5 s on LUBM(100)), against a check that is nearly as fast without them.

**So:**
- Maintenance runs on the one-step check.
- The counting code was removed (the owner, 3 October: nothing that measures slower stays). What remains is the recursion analysis, `nrese_reasoner::v2::recursion`.
- The `count` level is dropped; the levels are `none` and `acl`.
- The support stack (step 2) is needed only for the support graph sets of level `acl`.
- The steps below are re-ordered accordingly.

## Steps

1. **Recursion analysis and counts, in memory first.** Done, together with the one-step check (above), which is what maintenance uses.
   - Keys, components, and non-recursive counts during materialisation and in the delta executor's overdeletion.
   - Checked by the property tests (incremental = rematerialisation over random changes) and measured on the delete probes against the previous build.
   - Engine changes only once the counts prove their worth.
2. **Support graph sets over a closure** (done, 3 October): `nrese_reasoner::v2::graph_sets`, annotated semi-naive evaluation over the ground program with every alternative grounding's schema premises; equal to the closure of every set of graphs over 30 seeds × 150 random ontologies × 8 sets; LUBM(10) in 1.1 s against 0.2 s for the materialisation.
3. **Reads under graph access** (done, 3 October): `inferred = "supported"`; `nrese_store::support` keeps the sets of the latest revision in memory, computed on the first restricted read, and each reader's view as a snapshot with the invisible inferred statements removed (`Snapshot::with_inferred_subset`): no read path needs a filter of its own. This replaces the bitmap join of "Reading under graph access" above for now; the view costs O(k log k) for the smaller side k of the split, once per revision and access set.
4. **The support stack** in the engine, with persistence across restarts, holding support graph sets (level `acl`). Counts there only if a workload shows they pay.
5. **Maintenance on commits:** the sets of the facts a commit touches recomputed and propagated forward, instead of all of them at the next restricted read.
6. **`auto` maintenance** and the rematerialisation threshold.
7. **Explanations** that prefer derivations whose premises the reader may read.
