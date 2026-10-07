# Coupling the Horn bound with the hypertableau (W1: ORE 9724, 7914, 9835)

Status: design note for review (7 October 2026), no build yet. Owners: `nrese-dl::context`
(the Horn bound's saturation) and `nrese-dl::classify` (exactness, the tests).

## 1. The problem, measured

Three classification tasks of the ORE 2015 development set that NRESE doesn't solve, and
Konclude does in 7–17 s (lab, 1 CPU, 5 October). Counters of `dl_classify` built at
98496aa (lab profile, the Rust sources clean; 120 s limit; `nrese.py dl`, 7 October):

| | 9835 | 7914 | 9724 |
|---|---|---|---|
| DL, size | SHI; 46,247 classes, no ABox | SRIQ; 17,680 classes, 108.5 k class assertions | ALEHIF+ (Full-GALEN's signature); 23,136 classes, 950 properties |
| clauses with more than one head, before → after renaming | 16 → 16 | 41 → 41 | 2,736 → 0 |
| Horn bound | nothing in 60 s | nothing in 60 s | 453,791 subsumptions, exact for 2,402 classes (10 %); 83 s, past its 60 s budget (68 s in a 90 s run) |
| hypertableau tests then | 8,374 in 60 s | 388 | 11 |
| nodes per test | ~950 | ~13,000 | ~112,000 |
| branch points, clashes (all tests) | 108,141, 0 | 2, 0 | 0, 0 |
| where it stops | the deadline: 37,855 classes undecided | 17,298 classes hit the 256 MiB per-test budget | 20,734 classes hit the 256 MiB per-test budget |
| Konclude (lab, 1 CPU) | 11.8 s | 7.2 s | 16.9 s |

The Horn bound per strategy of the context core's Succ rule (same binary, the bound's budget
about 45 s within a 90 s run):

| | cautious | split (default) | eager | split with the Eq rule |
|---|---|---|---|---|
| 9835 | nothing | nothing | nothing | – |
| 7914 | nothing | nothing | nothing | – |
| 9724 | nothing (34 s) | 453,791 subsumptions, exact for 2,402 classes (68 s) | nothing (44 s) | nothing (stops after 0.25 s; why is for step 0's counters) |

So the search is never the problem: 108 k branch points and no clash at most. The cost is in
three places:
1. **A saturation that doesn't finish** (9835, 7914): every class then needs its own test.
   9835's tests are cheap (about 7 ms) but there are 46 k of them. 7914's are deterministic
   (2 branch points in 388 tests) but each builds 13 k nodes.
2. **A saturation that finishes but isn't exact** (9724): its functional properties are
   equality, which the Horn bound leaves out (the Eq rule is off, see §2), so 90 % of the
   classes still need a test.
3. **Deterministic tableau models too large to build per class** (9724: 112 k nodes per test,
   7914: 13 k): the memory budget, not the clock, ends them.

## 2. The general cause in the context core: conditional possible atoms

The context core is Bate et al.'s calculus (JAIR 2018) restricted to its Horn rules. A
context's clauses are `Γ → A` over the context's terms, and the Succ rule makes successors:
- an existential `∃R.B` at a predecessor `u` gives a successor term `f(x)`;
- the atoms that hold for `f(x)` unconditionally (`K₁`) become the successor context's core;
- every atom that holds for `f(x)` only under a condition at `u` (`K₂ \ K₁`, a *possible
  atom*) is sent to the successor as `A → A`.

Everything the successor derives from a possible atom keeps it in its body, so the
successor's clauses are conditional, and the Pred rule joins their bodies against `u`'s
clauses about `f(x)`. That makes the calculus complete for Horn-ALCHI(Q) with inverse roles:
what a successor sends back may depend on what the predecessor gives it. Its cost:
- a context holds a clause per head *and per distinct set of conditions* it was reached
  with: in the worst case exponentially many in the possible atoms, in practice the products
  of the conditions along a chain of Pred joins;
- every Pred join multiplies the alternative bodies of each atom it needs.

What this was measured to do (dl/classify, 5–6 October):
- **Cautious** (the paper's default: one context per filler `B` where `B(f(x))` holds
  unconditionally, else one shared context with the empty core) collects every uncertain
  successor's conditions in the shared context: on 9835 it held 99 % of 3.0 M clauses, and
  25–80 % of all clauses across the ORE development set, 99 % of what it derived redundant.
- **Split** (an empty-core context per Skolem function, kept: performance.md §0) stopped the
  unrelated conditions from combining: 78 tableau-path dev tasks 231.8 → 109.5 s. On 9835 and
  7914 the conditions stay: Pred joins at hub contexts exceed their step budget, the contexts
  end `Incomplete::Join`, and the bound returns nothing (above).
- **Eager** (a context per set `K₁`) makes many more contexts without removing the
  conditions.
- **The Eq rule** (Kazakov's Horn-SHIQ merges, adc8e89) inherits them: a conditional merge
  is copied both ways, and on 9724 the copies reached 54 M before the memory budget. It is off
  by default, which is why 9724's functional properties are left out.

Konclude's saturation has none of this because it isn't complete and doesn't try to be (§3).

## 3. Prior art

`corpus/dl/tableau`: steigmiller2014coupling, steigmiller2015payasyougo,
steigmiller2016thesis; `corpus/dl/consequence-based`: simancik2011beyondhorn,
bate2018sequoia, tenacucala2018nominals, tenacucala2021sroiq. Wiki: *DL gap - Steigmiller
thesis vs outliers*, *DL-B consequence-based vs hypertableau*.

**Steigmiller and Glimm, coupling tableau and saturation (IJCAR 2014; JAIR 2015, §3–4).**
- A *saturation graph* reuses one node per concept: `∃R.C` links to `v_C` itself, so there
  are at most as many nodes as concepts and no blocking.
- **No conditions.** The ∀-rule propagates only to predecessors (sound, since a reused node's
  label holds only consequences of its own concept); disjunctions add only what both
  disjuncts imply; at-least restrictions ignore the number; a nominal's consequences are read
  from its node.
- **Status detection** afterwards (Table 4): a node where a tableau rule might still add
  something (an unhandled ∀ to a successor, a disjunction, a tight at-most, a nominal merge)
  is *critical*, and so are all its predecessors.
- **Use:** known and possible subsumers from non-critical nodes; labels transferred into
  completion graphs; a completion node labelled like a non-critical, non-nominal-dependent
  saturated node needn't expand its successors.
- Paper says: all saturation optimisations on against off cut accumulated reasoning time by
  77.5 % over several thousand ontologies (Table 12).

**Steigmiller's thesis (Ulm 2016, §6; ablations Tables 6.5–6.9, pp. 142–145).**
- **SE** (subsumers from non-critical nodes): ORE2014 29,841 s → 56,469 s without it.
- **ES**, the extended saturation (pp. 127–128): a predecessor's `∀r.B` gets a *copy* of the
  successor with `B` added, one copy per extension and reused ("an efficient implementation
  of the so-called node contexts"); `≤ 1 r` merges successors into one node the same way:
  "Horn-SRIF can be almost completely supported". Full-GALEN 12.0 s with it, ≥ 300 s without
  (Table 6.8); ORE2014 29,841 → 51,400 s without it. The thesis ties this to Full-GALEN: the
  tableau "has difficulties to find appropriate blocker nodes" there (p. 144).
- **RT** (result transfer into the tests): ORE2014 29,841 → 56,128 s without it; but TONES is
  faster without it (143 vs 250 s): not free.
- Saturation is 12.1 % of Konclude's time over all repositories (Table 6.9).

**The consequence-based calculi.**
- Simančík, Kazakov and Horrocks 2011 (ALCH): ordered resolution in contexts handles
  disjunctions deterministically (ConDOR: GALEN 4.9 s, paper says; no inverses).
- Bate et al. 2018 (ALCHIQ⁺, Sequoia): ours; cautious and eager strategies; paramodulation
  for numbers. Paper says: Sequoia 131 s against ELK's 2 s on SNOMED, from a generic
  hyperresolution index.
- Tena Cucala, Cuenca Grau and Horrocks 2018, 2021 (ALCHOIQ⁺ → SROIQ): nominal and root
  contexts. Paper says: 49 timeouts on the Oxford repository, 38 of them non-Horn; Sequoia and
  HermiT complementary (21 against 16).

None of the complete calculi reports a remedy for the growth of conditions beyond redundancy
elimination and the choice of strategy. Konclude avoids it by giving up completeness in the
saturation and buying it back with critical nodes and tableau tests.

## 4. Options

| | What | Cost | What the measurements say |
|---|---|---|---|
| **A** | Keep the complete Horn calculus; make the conditions cheaper (a sublinear redundancy index, join budgets, another strategy) | small, local | No strategy gives a bound on 9835 or 7914 within its budget (§1). The redundancy index addresses the cost per clause, not the number of clauses. On its own it can't remove 46 k tests |
| **B** | **An unconditional saturation with critical contexts** (Konclude's SE in the core's terms): Succ gives a successor only `K₁`, never `A → A`; Pred sends back only unconditional clauses; a context that would need a possible atom (`K₂ ≠ K₁` at an edge into it, or a ∀ its predecessor sends that its core lacks) is *critical*, as are its predecessors; a class is exact iff its saturation reaches no critical context and no left-out clause (what `exact_for` already does for left-out clauses) | moderate: a strategy beside split, criticality in the exactness pass; soundness is easy (fewer conclusions) | Contexts become ELK's: one per filler, no conditions. Whether it pays depends on one unknown count: **the share of classes that would be critical** (§6, step 0). Konclude classifies 9835's 46 k classes in 11.8 s, which it couldn't do with most of them critical (an inference, not a measurement) |
| **C** | **ES on top of B:** copies for a predecessor's certain universals (a context per core extended by what the predecessor makes *certain*, keyed and reused, which is the eager strategy without possible atoms) and the Eq rule restricted to unconditional merges (closed, copied onto the least term only) | moderate: the copy mapping, the Eq rule's unconditional half (it exists) | Targets 9724: its non-Horn part is only equality (2,736 → 0 wide clauses after renaming). Paper says ES is what makes Full-GALEN feasible (12.0 s against ≥ 300 s). Our conditional Eq copies were the 54 M blow-up; the unconditional ones are closed |
| **D** | **Result transfer and saturated blocking in the tests** (thesis RT, JAIR 2015 §4): a node that gets class `C` gets the saturated label of `C`'s non-critical context as deterministic facts; a node whose label equals a non-critical, non-nominal-dependent context's needn't expand its successors | moderate to large: the tableau reads the core's results; a switch and an A/B on every stratum (TONES got slower) | Targets the classes B and C leave inexact. 7914's tests are deterministic and 13 k nodes each, 9724's 112 k: with saturated blocking they stop at the first node the saturation covers. No count yet of how many nodes per test a non-critical context would cover |
| **E** | A complete consequence-based calculus with disjunctions, numbers and nominals (Bate, Tena Cucala) as the bound | large | Not for W1: these three are near-Horn (16, 41 and 0 wide clauses after renaming), and the cost is already in the Horn conditions |

**Recommended:** B, then C, then D, each behind a switch; A only as the redundancy index
where B's profile still shows it. The complete conditional core stays as it is for the
ontologies it finishes, where its exactness spares every test (7127 and 7956: 0 tests).
Which mode runs first is a driver decision: the unconditional one when the conditional one
runs out of its budget, or both in the portfolio's way.

## 5. Soundness and completeness

- **B:** an unconditional saturation derives a subset of the conditional one's conclusions,
  so every subsumption it gives holds. Exactness needs the critical marking to be
  conservative: a context is non-critical only if no atom outside its core could reach it
  from any predecessor, and nothing it sends back depends on one. The thesis's Table 4 rules
  are the checklist; the guard is `taxonomies_equal_brute_force` with B switched on, plus the
  metamorphic tests (same taxonomy with B on and off).
- **C:** copies carry only certain extensions, so their consequences are certain too.
  Unconditional merges are closed (adc8e89's guards, `functional.rs`).
- **D:** transferred facts are deterministic consequences of the class (so backjumping
  ignores them). Blocking on a saturated label needs the JAIR 2015 conditions: not critical,
  not nominal-dependent, no tight at-most restriction.
- The correctness target: HermiT times out on all three (300 s), and Konclude ends 7914 with
  an error, so Konclude's taxonomies of 9724 and 9835 are the only references, and 7914 has
  none (`classify-dev-2026-10-05.tsv`'s "differ" there compares NRESE's incomplete results).
  So a taxonomy is checked by brute force on fuzzed and shrunk cases (the existing campaign)
  and, on the tasks themselves, against Konclude, every difference adjudicated by a
  hypertableau test of the pair.

## 6. Order and falsifiable expectation

0. **Counts before building** (one build of counters, no behaviour change): per task, the
   contexts, clauses, conditional clauses and `Incomplete::Join` contexts of the conditional
   core, and the share of classes whose saturation receives a possible atom (would be
   critical under B).
1. B as a strategy, with its exactness; the brute-force and metamorphic gates.
2. C: copies for certain universals, the Eq rule's unconditional half.
3. D, behind a switch; the A/B on the ORE development set by stratum.

**Expected:**
- step 0: on 9835 and 7914 most clauses are conditional (the cause in §2); if they aren't,
  the cost is elsewhere and this note is wrong about the cause;
- B: the bound finishes on 9835 and 7914 within 10 s at 1 CPU, exact for ≥ 80 % of the
  classes; 9835 then solved under 30 s;
- C: 9724's bound exact for ≥ 90 % of the classes within 20 s;
- D: 7914's nodes per test fall from 13 k to under 1 k and no test hits 256 MiB; all three
  solved under 30 s at 1 CPU, with the taxonomy checked as in §5.

**Falsified if** B's critical share on 9835 or 7914 is above 50 %: the critical marking is
then too coarse for these ontologies, C's copies (or the possible atoms B drops) are what
they need, and C comes before B. If B and C give exact bounds but the classes they leave
still exhaust memory in the tests, D is the lever, and its count (nodes per test covered by
a saturated label) decides it.
