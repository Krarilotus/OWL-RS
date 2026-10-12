# ADR-0011: Equality, one contract with backends per lifetime

Status: proposed (6 October 2026).

## Context

Equality has several owners today, each with its own union-find:

- `nrese-exec::classes`: the rules' representatives and the store's compact mode (G7);
- `nrese-dl::tableau::merge`: the ≈-rule, with a trail and dependency sets;
- `nrese-dl::context::equality`;
- `nrese-dl::datatypes::theory`: value equality, with dependency sets;
- `nrese-owl::ql::hazards`.

The store's version is stage B of [research-designs §1](../design/research-designs.md):

- representatives, with no reasons, supports or split path;
- a deletion that splits a class costs 66 s on a 500-member class, where rematerialising
  everything costs 0.15 s.

Two research rounds agree on the design:

- the 2 October reports: §1, and the wiki's *Store-2 equality*;
- the 6 October report, *Equality as a shared service*: Motik et al. 2015, B/F≈,
  Nieuwenhuis–Oliveras proof-producing union-find, Flatt et al., egglog, HermiT.

The research agent checked the 6 October report against the papers (wiki: *Store-2 equality*;
`reports/round4/chatgpt-equality-service.md`). The architecture holds; five attributions
were corrected, among them: the counting hybrid is Hu, Motik & Horrocks (AAAI 2018); the
cost of rich provenance rests on Flatt et al.'s Table I; no paper fixes the smallest member
as the representative, which is our policy.

The 6 October report adds what §1 left open: why one shared data structure is wrong.

- Path compression, rollback and split want contradictory structures.
- An explanation means a rule proof in one owner and a dependency set in another.
- The representative is a policy, not semantics.
- Rich provenance has a cost the monotone path must not pay.

## Decision

**One contract, shared** (in `nrese-exec`, which every equality owner already depends on):

- owner-scoped ids, with a domain per owner: a union across domains is impossible
  without an explicit bridge;
- `ReasonId`: append-only, immutable reason records:
  - explicit statement;
  - rule, functional or inverse-functional property, or key, with their premises;
  - tableau or nominal, with its dependency set;
  - datatype theory lemma;
- an explanation protocol: one causal edge always; alternative supports only where
  deletion needs them; minimisation on demand;
- the event vocabulary: `MergeDelta` and `SplitDelta` (old and new components, old and
  new representatives, moved members);
- tracing, metrics, and one set of property tests (a differential test against
  axiomatised equality) that every backend passes.

**Backends per lifetime, each owned where it is used:**

| Backend | Owner | Structure |
|---|---|---|
| Monotone | rules (batch rounds, bulk loads) | union by size, aggressive compression, compact arrays, no provenance beyond one edge; bulk classes by sorting edges into components |
| Persistent | rules with the store (compact mode, incremental maintenance) | see below |
| Rollback | DL (tableau, context, datatypes) | trail, no compression, merge direction chosen by the calculus, reasons carrying dependency sets for backjumping and learning |

**The persistent backend:**

- Support counts on the asserted equality edges only. A derived equality can be
  supported by copies made through itself (a `prp-fp` equality whose premises are such
  copies), so its count never drops to 0: counting is unsound for recursive supports.
  Derived edges carry no count; a change in their class re-validates them by B/F.
  Only an asserted edge's count dropping from 1 to 0 can change a class.
- A spanning forest per class over its asserted edges, plus the non-tree edges. Deleting
  a forest edge triggers a connectivity test inside that class. Fully dynamic
  connectivity (HDT) is the upgrade only if profiles demand it.
- The asserted facts stay exact, beneath the canonical ones (already decided in §1), so a
  split can restore them.
- A split returns a `SplitDelta`. Fact maintenance is driven from it:
  - counting first, B/F where support is ambiguous;
  - the maintenance policy's switch, inside the commit: it estimates, in facts touched,
    DRed's work over the class (its members' facts, copied through every member),
    recomputing the affected part (the members' facts and their consequences overdeleted
    at once and derived again from what remains), and rebuilding the whole state (its
    stored facts, offered only to a change that inserts nothing, as deletions introduce
    no violations), and takes the cheapest. A rebuild runs no delta at all and applies
    only the difference to the stack. The switch lives with the maintenance policy
    (`delta::policy`), not with the equality code, so other maintenance cases (large
    deletes, long chains) use it too.
- A stable class id, apart from the union-find root and the displayed representative (§1).
  For B3 it stays internal to the backend: storage keeps the smallest member as the
  representative, and a merge or split rewrites the stored facts of the side that loses
  it (counted, so the cost of a large side losing it is measured).

**Built (B3a):** the switch, without the backend. A deletion estimates the plans for the
classes it touches; recomputing re-derives the part in bulk (the copies of a remaining
fact within the classes the remaining equalities make, and what the other rules derive
in one step, to a fixpoint). Over a stack kept over representatives (compact mode) a
split is always rebuilt: the delta executor reads the old classes through the expanded
view, and translating its result into the split classes misses copies when a commit
splits and merges at once (the differential test found it on about a third of 60 seeds), so
the policy knows that any delta plan there ends in a rebuild and skips it. The backend
itself (origins, the forest, B/F re-validation, the representative view for the delta
executor, which lets compact mode recompute a part too) is B3b.

**Datatype equality is a producer:** the datatype theory computes value equality and
asserts it into the owning domain. It never makes two lexical terms one id.

**Wording that binds every backend** (the research review of 7 October):
- a representative's identity carries no meaning: only membership of a class does;
- deleting a support is a capability of a storage policy (the persistent backend), not of
  the equality service; the rollback union-find is the search's policy only, never a way to
  delete;
- explanations rest on the original equality edges and their justifications, never on a
  representative staying the same.

A split's cost of O(V_C + E_C) over the affected component is our own bound, not a
published one. Faster fully dynamic connectivity exists (HDT; Huang, Huang, Kopelowitz,
Pettie and Thorup, SODA 2017), but rebuilding the touched class stays the default until
measurements show large classes and frequent deletes together.

## Consequences

- G7's single kernel stays the monotone backend. The persistent backend is B3, owned by
  rules. The rollback backend stays in the DL crate, adopting the shared `ReasonId` and
  delta types where they fit.
- The target for splits is not "sub-millisecond". It is: never slower than rematerialising
  what the split touches. A guard measures a 500-member split against that.
- The open owners (context equality, QL hazards) adopt the contract when they are next
  touched; no rewrite for its own sake.
