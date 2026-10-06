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

- Equality edges with support counts. Only a count dropping from 1 to 0 can change a
  class.
- A spanning forest per class, plus the non-tree edges. Deleting a forest edge triggers a
  connectivity test inside that class. Fully dynamic connectivity (HDT) is the upgrade
  only if profiles demand it.
- The asserted facts stay exact, beneath the canonical ones (already decided in §1), so a
  split can restore them.
- A split returns a `SplitDelta`. Fact maintenance is driven from it:
  - counting first, B/F where support is ambiguous;
  - a switch to rematerialising the affected part when the estimated work exceeds its
    measured cost.
- A stable class id, apart from the union-find root and the displayed representative (§1).

**Datatype equality is a producer:** the datatype theory computes value equality and
asserts it into the owning domain. It never makes two lexical terms one id.

## Consequences

- G7's single kernel stays the monotone backend. The persistent backend is B3, owned by
  rules. The rollback backend stays in the DL crate, adopting the shared `ReasonId` and
  delta types where they fit.
- The target for splits is not "sub-millisecond". It is: never slower than rematerialising
  what the split touches. A guard measures a 500-member split against that.
- The open owners (context equality, QL hazards) adopt the contract when they are next
  touched; no rewrite for its own sake.
