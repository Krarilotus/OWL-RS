//! Guards of the batch executor's memory and probe wins (docs/design/performance.md §0):
//! counts, not times, on a small LUBM-shaped input under OWL 2 RL, on one thread so the
//! allocations are deterministic. Measured with a counting allocator, so this file is a
//! test binary of its own, and its tests run one at a time.

use std::sync::Mutex;

use nrese_exec::heap;
use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[path = "support/lubm.rs"]
mod lubm;
use lubm::{DEPARTMENTS, cascade, grouped, lubm_like};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The tests share the allocator's counts.
static SERIAL: Mutex<()> = Mutex::new(());

/// OWL 2 RL over [`lubm_like`] with `departments`, on one thread, as the store runs it
/// (grouped input), with the heap profile of the run and the bytes live before it besides
/// the input's pairs (which become the working set's input runs).
fn materialise(departments: u64) -> (Materialisation, Vec<heap::Phase>, usize) {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let facts = lubm_like(&mut vocabulary, departments);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a pool");
    let input = grouped(facts);
    let pairs: usize = input.iter().map(|(_, pairs)| pairs.capacity() * 16).sum();
    let before = heap::live() - pairs;
    heap::start("input");
    let result = pool.install(|| batch::materialise_grouped(input, &rules, Some(&lists), &schema));
    (result, heap::finish(), before)
}

/// P1-F1 and P1-F4. Rule jobs read their drivers in the working set's runs instead of
/// copying them, and the rounds keep no list of the facts they derived beside the
/// working set (the base and recent runs are that list). Reverting either shows here:
/// copied drivers count bytes, and a derived-facts list holds 24 bytes per derived fact
/// outside the runs at the end of every round.
#[test]
fn rule_jobs_copy_no_drivers_and_rounds_keep_no_derived_facts_list() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (result, phases, before) = materialise(DEPARTMENTS);
    assert_eq!(
        result.counters.driver_bytes_copied, 0,
        "rule jobs copied their drivers"
    );
    // The heap live at the end of each round (when the next one grounds, or the
    // consistency check begins), against the runs it holds.
    let ends: Vec<usize> = phases
        .windows(2)
        .filter(|pair| match pair[1].label {
            "reasoner: grounding" => pair[0].label != "input",
            next => next == "reasoner: consistency",
        })
        .map(|pair| pair[0].live_after)
        .collect();
    assert_eq!(ends.len(), result.rounds);
    assert_eq!(result.counters.store_bytes.len(), result.rounds);
    let derived = result.derived.len();
    assert!(derived > 20_000, "{derived} derived facts: too few to tell");
    let (&end, store) = ends
        .last()
        .zip(result.counters.store_bytes.last())
        .expect("rounds");
    let outside = end - before - store.total();
    // What else a round holds is the program (schema-sized), not data: 38,812 bytes on
    // 5 October 2026, where a derived-facts list would take 893,280.
    assert!(
        outside < derived * 24 / 4,
        "{outside} bytes beside the working set's {} after the last round, {derived} derived facts",
        store.total()
    );
}

/// P1-F9 and P1-F10. A round holds its candidates as its morsels found them (not
/// concatenated, never grown by doubling), merges each predicate's into chunks without a
/// sorted copy of all of them, folds the recent run into the base a chunk at a time, and
/// the derived facts are listed while the working set is taken apart. So no phase holds
/// much beside the working set: counted in bytes per fact of the final working set
/// above it (not as a share of it, which P1-F11 shrank). With chunks of 1,024 pairs, so
/// that this input's runs have many: the rounds 3.55, the listing 0.52 bytes per fact on
/// 6 October 2026; reverted, one-vector folds 8.10, a one-vector union 8.40,
/// concatenated candidates 19.50, the listing beside the whole working set 9.74.
#[test]
fn rounds_hold_no_second_copy_beside_the_working_set() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    batch::set_chunk_pairs(1024);
    let (result, phases, before) = materialise(DEPARTMENTS);
    batch::set_chunk_pairs(1 << 20);
    let store = result.counters.store_bytes.last().expect("rounds").total();
    let facts = store_facts(&result);
    let peak = |labels: &[&str]| {
        phases
            .iter()
            .filter(|phase| labels.contains(&phase.label))
            .map(|phase| phase.peak - before)
            .max()
            .expect("the phases ran")
    };
    let rounds = peak(&[
        "reasoner: grounding",
        "reasoner: joins",
        "reasoner: modules",
        "reasoner: merge",
        "reasoner: install",
    ]);
    let per_fact = |bytes: usize| bytes.saturating_sub(store) as f64 / facts as f64;
    let rounds = per_fact(rounds);
    assert!(
        rounds <= 6.0,
        "the rounds held {rounds:.2} bytes per fact beside the working set, at most 6"
    );
    let listing = per_fact(peak(&["reasoner: derived"]));
    assert!(
        listing <= 3.0,
        "listing the derived facts held {listing:.2} bytes per fact beside the working set, at most 3"
    );
}

/// P1-F11. A relation keeps its `(object, subject)` order only if a rule of the program
/// can look it up by object alone (or grounding reads it): here `takesCourse`, `advisor`,
/// `teacherOf`, the degrees and the inverse `member`, among others, keep only their
/// subject order. Every order kept, the final working set holds 32 bytes per fact; 22.4
/// on 6 October 2026. No lookup by object meets a relation without that order.
#[test]
fn relations_keep_no_object_order_their_rules_never_use() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (result, _, _) = materialise(DEPARTMENTS);
    assert_eq!(result.counters.lookups_without_order, 0);
    let store = result.counters.store_bytes.last().expect("rounds").total();
    let facts = store_facts(&result);
    assert!(
        store <= facts * BYTES_PER_FACT,
        "{store} bytes for {facts} facts, at most {BYTES_PER_FACT} each"
    );
}

/// The facts of the working set at the end: the input's and the derived ones.
fn store_facts(result: &Materialisation) -> usize {
    let input = grouped(lubm_like(&mut LocalVocabulary::default(), DEPARTMENTS));
    input.iter().map(|(_, pairs)| pairs.len()).sum::<usize>() + result.derived.len()
}

/// P1-F8. A morsel's candidates are checked against the working set once each, in
/// (predicate, subject, object) order. Unsorted, every probe misses the cache at each
/// level of each binary search; without the deduplication about half the probes repeat.
#[test]
fn membership_probes_are_distinct_and_ordered_per_morsel() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (result, _, _) = materialise(DEPARTMENTS);
    let counters = &result.counters;
    assert_eq!(counters.unordered_probes, 0, "probes out of order");
    let probes: u64 = counters.probes.iter().sum();
    assert!(
        probes <= PROBES,
        "{probes} membership probes over {} rounds ({:?}), at most {PROBES}",
        result.rounds,
        counters.probes
    );
}

/// The probes of [`membership_probes_are_distinct_and_ordered_per_morsel`]'s run on
/// 5 October 2026 (37,014, 49,813, 8,016 and 200 in its four rounds). Deterministic: a
/// change of the rules or the input shape that adds probes for a reason re-measures it.
const PROBES: u64 = 95_043;

/// G8 and #4 of the investigation of 6 October 2026: with equality by representatives
/// the closure is computed in the working set, from the grouped input, whatever
/// `sameAs` merges it meets. Before (133671c), one `sameAs` moved the store's
/// materialisation off the grouped input onto lists of triples (24 bytes each: the
/// input, a copy with what a first pass derived, each outer pass's rewrite and closure).
/// The heap's peak with a cascade of 8 merges is within 1.1 times the peak without.
#[test]
fn equality_keeps_the_working_set_as_the_only_copy() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let peak = |depth: usize| {
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl
            .rules(&mut vocabulary)
            .expect("OWL 2 RL parses");
        let lists = ListVocabulary::new(&mut vocabulary);
        let schema = Schema::owl(&mut vocabulary);
        let mut facts = lubm_like(&mut vocabulary, DEPARTMENTS);
        if depth > 0 {
            let extra = cascade(&mut vocabulary, &facts, depth);
            facts.extend(extra);
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("a pool");
        let input = grouped(facts);
        let before = heap::live();
        heap::start("input");
        let result = pool.install(|| {
            batch::materialise_representatives_until(
                batch::Input::Grouped(input),
                &rules,
                Some(&lists),
                &schema,
                batch::Listing::Expanded,
                nrese_reasoner::eval::NEVER,
            )
            .expect("never stopped")
        });
        let phases = heap::finish();
        assert_eq!(result.merges, usize::from(depth > 0) * depth);
        phases.iter().map(|phase| phase.peak).max().expect("phases") - before
    };
    let (plain, merged) = (peak(0), peak(8));
    assert!(
        merged * 10 <= plain * 11,
        "heap peak {merged} bytes with 8 merges, {plain} without: at most 1.1 times"
    );
}

/// See [`relations_keep_no_object_order_their_rules_never_use`].
const BYTES_PER_FACT: usize = 26;
