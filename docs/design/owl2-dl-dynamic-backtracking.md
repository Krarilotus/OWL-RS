# Dynamic backtracking in the hypertableau (W3C DL-662, 663, 664)

Status: approved (7 October 2026, with the two changes below folded in), for the v2 gate. Owner: `nrese-dl::tableau`. It touches
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

## 2. Prior art (`corpus/solvers/sat`: ginsberg1993dynamic, nadel2018chrono, mohle2019backing; wiki: *Backtracking that keeps independent work*)

- Ginsberg, *Dynamic backtracking* (JAIR 1993): on a conflict, retract only the culprit
  decision and what depends on it; keep the rest, with an eliminating explanation per
  retracted value. Complete, polynomial space. The reassigned variable moves to the end of
  the order (Def. 2.2; Alg. 4.1/4.3 step 3, §4) and the culprit is the most recent variable
  of the conflict (step 5): checked against the paper, the fresh largest id and `k = max(C)`
  below match, as do the kept part and the eliminating explanation (eq. 2). Lifting the
  restriction to the most recent culprit can loop (§7.2.1): the culprit stays strictly the
  most recent here, and a heuristic choice of the level to retract would need an argument
  of its own.
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
   (semantic branching: Ginsberg's eliminating explanation). It moves to the top of the
   decision order: it gets a fresh id, the largest, and its new alternative's facts carry
   that id, as in dynamic backtracking, where a variable given a new value moves to the end
   of the assignment order.
3. The other frames above `k` stay, with everything they built.
4. A frame with no alternative left still pops as now (its clash goes to
   `max(premise ∪ failed)`). The first version may fall back to truncation there; the counters
   say how often.

**Level identity.** Dependency sets name stack positions today. Frames get stable,
increasing ids instead, and a re-decided frame a fresh one: `max(C)` then always means the
most recently decided level. A frame removed in the middle renumbers nothing. The stack is
then no longer in the order of the frames' graph marks, so a fallback truncation to a frame's
mark pops every frame whose mark is at or after it, not only those above it.

**Graph.**
- The arenas stay append-only.
- A retracted fact gets a dead bit and leaves the indexes (`unary_ix`, `edge_ix`, …). Its
  slot is reclaimed at the next `cut` below it. `cut(mark)` stays for restarts, probes and
  base rollback.
- In-place changes on the trail (merge flags, representative) record their dependency set
  and the bits they set, so retraction undoes exactly the entries that contain `k`, in any
  order. Blocking writes its flags untrailed and recomputes them anyway.
- Retraction then **removes** its level's entries from the trail, and patches the `old`
  value of later entries on the same node. It must not append undo entries of its own: a
  later `cut` to a mark taken before the retraction (a kept frame's) would pop them and
  restore the retracted merge, while its copied facts stay dead. `PRUNED` is recomputed
  from the live merged ancestors of the touched subtree.
- Dead facts need no trail: they leave the indexes, and a `cut` truncates the arenas as
  before (a fact re-derived after a retraction always sits above the dead one, so the
  index stays consistent under any cut).

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
- Termination by the same argument (Theorem 4.2): its proof strengthens each nogood with the
  current values of all variables before the culprit (eq. 8); each is valid, none follows
  from the earlier ones, and the consequences of their conjunction grow monotonically
  (Lemma A.1, which needs the order: the re-decided frame at the end, here its fresh id).
- The 3.9 proof obligation covers this together with learning. There termination rests on
  keeping the learned clauses, not on an order (Möhle and Biere, rule Jump, Props. 1–2):
  with permanent nogoods the backtrack level becomes free.

**Gates:**
- A switch `Config::dynamic_backtracking`, off until the A/B.
- Checkable invariants (`Config::check_retraction`, on in debug builds and in the
  campaigns): after each `retract(level)`, no surviving fact, edge, merge pointer or pending
  disjunction has a dependency set containing a retracted level, and every node in
  `touched` has been re-checked for blocking. A missed dependency then fails at once, not
  as a wrong answer three levels later.
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

## 7. Step 2, as built (8 October 2026)

Behind `Config::dynamic_backtracking` (off), with `Config::check_retraction`.

**What building it added to §3:**
- **The closure.** A frame above `k` whose premise names a retracted level depends on it.
  So does a frame whose *failed* set names one: its eliminated alternatives may be
  possible again. These are Ginsberg's eliminating explanations that mention the
  retracted variable. Such frames are retracted with `k`, and their disjunctions are
  decided afresh.
- **Exhausted culprits.** When `k` has no alternative left, it is retracted and its
  disjunction's clash (premise ∪ failed) goes on, retracting again.
- **Subtrees.** A node dies with its parent. Facts made by clauses that apply to every
  node depend on nothing, so a successor born from them would otherwise outlive a
  retracted parent. The blocking checker found this (seed 94543, case 118).
- **Re-firing.** The live facts of the touched nodes and their neighbours are joined again.
  The disjunctions behind the scan that bind a touched node are reopened. Resetting the
  scan cursor cost 1.6× more.
- **No checkpoint older than a retraction is restored.** A retraction kills facts below
  the marks of the frames that stay, and what redoes them (the re-decided choice, the
  reopened disjunctions) lies above. A backtrack that falls back to truncation (a merge
  since its culprit) would restore a checkpoint that keeps the kills without the redoing.
  On fuzz seed 131, case 138, it lost a re-decided choice and found a model of an
  inconsistent ontology. Such a backtrack now starts the search again from the first
  branch point as it was before the run's first retraction, with dynamic backtracking off
  for the rest of the run (`restarts` in the telemetry). Checkpoints also keep the
  retraction queues, which a backtrack restores as they were. Without the fallback (the
  rollback backend, step 3) no restart would be needed.

**Measured** (662 to 664, lab profile):
- Truncating backjumps fall to 0 and rebuilt levels to 0. Branch points fall from millions
  to 130–330 k per 30 s.
- None is decided, not in 300 s either (662: 36,814 clashes).
- A clash now costs about 8 ms, against 0.73 ms in the plain search (662: 41,221 clashes in
  30 s). Every retraction scans what was built since the culprit's mark: the frames above
  for the closure (on average 85,000), the fact arenas and the pending entries. The plain
  search, by contrast, discards about 370 levels per backjump.
- The plain search on 664 for 20 minutes: 117 M branch points, 19,794 clashes, not decided.

**Next:** make retraction cost what it retracts, not what it keeps:
- frames by the levels their premise and failed set name;
- facts and pending disjunctions by their interned dependency set, each set registered
  under its levels when it is interned;
- pending disjunctions by node, for the reopening.

Dynamic backtracking doesn't reduce the number of clashes, only their cost. Whether 662 to 664
are then decided depends on how many clashes they need. If that stays out of reach, nogood
learning (3.9) is the next angle.

