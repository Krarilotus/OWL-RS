# Dynamic backtracking in the hypertableau (W3C DL-662, 663, 664)

Status: proposed (7 October 2026), for the v2 gate. Owner: `nrese-dl::tableau`. It touches
ADR-0011's rollback equality backend (§4).

## 1. The problem, measured

DL-662 to 664 (consistency of OilEd ALC(D) knowledge bases) give up at every budget. Counters on
`dl/classify` (lab profile, 20–30 s per case, no portfolio):

| | 662 | 663 | 664 |
|---|---|---|---|
| branch points | 2.95 M | 2.28 M | 1.98 M |
| backjumps | 8,539 | 4,478 | 347 |
| levels a backjump discarded | 2.94 M | 2.28 M | 1.97 M |
| of those, decided again | 99.1 % | 99.4 % | 99.2 % |
| of those, the same alternative | 99.0 % | 98.4 % | 99.2 % |
| discarded levels whose premise depends on the culprit (transitively) | 0 | 0 | 0 |

- Time on 662: search 38 %, expansion 27 %, blocking 19 %, saturation 9 %. Rebuilding the
  discarded part dominates; choosing is less than half of it.
- The conflict order with an activity heap (patch kept in the lab) decides at plain rates and
  cuts clashes (663: 14,918 → 134), but then each backjump discards about 50,000 levels.

So the search throws away and rebuilds the same independent work after nearly every clash.
Phase saving changes nothing (the choices already repeat), and replaying the trail would save
the choosing only (at most about 1.6×).

## 2. Prior art (reported; not yet in `literature/`, to fetch before it is cited)

- Ginsberg, *Dynamic backtracking* (JAIR 1993): on a conflict, retract only the culprit
  decision and what depends on it; keep the rest, with an eliminating explanation per
  retracted value. Complete, polynomial space.
- Nadel and Ryvchin, *Chronological backtracking* (SAT 2018), and Möhle and Biere, *Backing
  backtracking* (SAT 2019): CDCL backtracks one level instead of jumping far, with assignments
  out of level order; the invariants that keep conflict analysis sound. CaDiCaL uses this
  (cited in `corpus/solvers/sat/biere2024cadical`).

The tableau's case is the friendliest one: every fact already carries its dependency set.

## 3. Design

**Retract by level, not by trail position.** On a clash with dependency set `C`, `k = max(C)`:

1. Retract level `k`. Every fact, edge, node, inequality and merge whose dependency set
   contains `k` is retracted. So is every frame above `k` whose premise does (none, on these
   cases).
2. Frame `k` takes its next alternative, with the failed one's negation under `C \ {k}`
   (semantic branching: Ginsberg's eliminating explanation).
3. The other frames above `k` stay, with everything they built.
4. A frame with no alternative left still pops as now (its clash goes to
   `max(premise ∪ failed)`). The first version may fall back to truncation there; the counters
   say how often.

**Level identity.** Dependency sets name stack positions today. Frames get stable,
increasing ids instead; `max` is still "the most recent". A frame removed in the middle then
renumbers nothing.

**Graph.**
- The arenas stay append-only.
- A retracted fact gets a dead bit and leaves the indexes (`unary_ix`, `edge_ix`, …). Its
  slot is reclaimed at the next `cut` below it. `cut(mark)` stays for restarts, probes and
  base rollback.
- In-place changes on the trail (flags, representative) record their dependency set, so
  retraction undoes exactly the entries that contain `k`, in any order.

**What depends on absence.**
- Blocking and the ≥-rule's witnesses are re-evaluated on the nodes retraction touched (the
  `touched` list incremental blocking already keeps).
- Pending disjunctions whose premise contains `k` are dropped.
- Those that a retracted fact had satisfied open again (a reopen list, not a cursor reset).

**Datatypes and the NI rule.** Their state has its own rollback with dependency sets. Version
1 retracts there too if it is cheap. Otherwise a level that touched them falls back to
truncation (counted). 662 to 664 are ALC(D), so this is measured before deciding.

## 4. Equality: ADR-0011's rollback backend

The rollback backend gains `retract(level)` beside LIFO `undo`. It undoes every representative
pointer whose merge reason contains the level, in any order. This is valid because:
- there is no path compression (already the backend's contract);
- a merge involving a class an earlier merge formed inherits that merge's dependency set
  (`canonical_pair`), so no surviving merge relies on a retracted one;
- facts copied by a merge carry its dependency set and go with it;
- the subtree it pruned is restored from trail entries that now record their dependency set.

Nothing changes for the monotone and persistent backends. The shared `ReasonId` and delta
types fit as they are: a reason already carries the dependency set.

## 5. Soundness and completeness

- Retraction removes only what was derived from the retracted decision, so the kept state is
  what the calculus derives from the remaining decisions, minus some redundant work.
- Completeness follows Ginsberg's argument: each retracted value keeps its eliminating
  explanation (the negated alternative under `C \ {k}`) while that explanation's levels stand.
- Termination by the same argument (reported, to verify).
- The 3.9 proof obligation covers this together with learning.

**Gates:**
- A switch `Config::dynamic_backtracking`, off until the A/B.
- Metamorphic: no answer changes on or off, on the tableau suites and the W3C suite.
- The brute-force campaigns with the switch on.

## 6. Order and falsifiable expectation

1. Stable level ids.
2. Dead bits and index removal for facts, with the queues.
3. Trail entries with dependency sets and `retract(level)` (equality).
4. Datatypes and NI, or the counted fallback.
5. The switch, the gates, then counts on 662 to 664 and the A/B.

**Expected:** branch points per backjump fall to about the alternatives tried (rebuilt levels
from about 99 % to near 0); the same 30 s then covers roughly 75× the clashes.

**Falsified if** the rebuilt share stays high (retraction misses dependencies, or the
fallback dominates). If the rebuilt share drops but 662 to 664 still need more than 10⁶
clashes, the bottleneck is the clash count, and nogood learning (3.9) comes next.
