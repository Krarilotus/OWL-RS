//! The poller bounds attempted work, not delivered rows (REAS review r1, F1–F3): searches,
//! producers and driver positions that deliver nothing still count, and an entry that is
//! asked after its operation stopped does no work at all. Every count here is of work a
//! source was asked for, never of time.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::load;
use crate::batch::{self, Schema};
use crate::delta::{MemoryBase, Rules};
use crate::eval::{GroundProgram, NEVER, OperationStop, Seg, Source};
use crate::ir::{Triple, parse_rules};
use crate::vocabulary::LocalVocabulary;

const POLL_UNITS: usize = nrese_exec::POLL_UNITS as usize;

/// Facts in a store, counting what each kind of read is asked for; the first scan opened
/// sets `cancel` (the operation's stop reads it).
struct Asked {
    facts: batch::Store,
    scans: AtomicUsize,
    estimates: AtomicUsize,
    contains: AtomicUsize,
    cancel: AtomicBool,
}

impl Asked {
    fn new(facts: Vec<Triple>) -> Self {
        Self {
            facts: batch::Store::new(facts),
            scans: AtomicUsize::new(0),
            estimates: AtomicUsize::new(0),
            contains: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
        }
    }

    fn reads(&self) -> [usize; 3] {
        [&self.scans, &self.estimates, &self.contains].map(|n| n.load(Ordering::Relaxed))
    }
}

impl Source for Asked {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        self.scans.fetch_add(1, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        self.facts.scan(pattern, seg, f);
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        self.estimates.fetch_add(1, Ordering::Relaxed);
        self.facts.estimate(pattern, seg)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.contains.fetch_add(1, Ordering::Relaxed);
        self.facts.contains(fact)
    }
}

/// `n` subproperties `ex:p{i}` of `ex:q`, each a producer of `(?x ex:q ?y)` from
/// `(?x ex:p{i} ?y)`, and the asserted `ex:a ex:q ex:b`; no `ex:a ex:p{i} ex:b` holds, so
/// every producer's search for the asserted fact is an empty scan. Returns the facts, the
/// ground program and the asserted fact.
fn empty_producers(
    vocabulary: &mut LocalVocabulary,
    n: usize,
    more_rules: &str,
    more_facts: &str,
) -> (Vec<Triple>, GroundProgram, Triple) {
    let rules = parse_rules(
        &format!("spo: (?p rdfs:subPropertyOf ?q), (?x ?p ?y) -> (?x ?q ?y)\n{more_rules}"),
        vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(vocabulary);
    let mut text: Vec<String> = (0..n)
        .map(|i| format!("ex:p{i} rdfs:subPropertyOf ex:q"))
        .collect();
    text.push("ex:a ex:q ex:b".to_owned());
    text.extend(more_facts.lines().map(str::to_owned));
    let mut facts = load(vocabulary, &text.join("\n"));
    facts.sort_unstable();
    facts.dedup();
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let inferred = batch::materialise(&facts, &rules, None, &schema).derived;
    let ground = crate::delta::program(&MemoryBase::new(&facts, &inferred), compiled);
    let mut closure = facts;
    closure.extend(inferred);
    closure.sort_unstable();
    closure.dedup();
    let asserted = load(vocabulary, "ex:a ex:q ex:b")[0];
    (closure, ground, asserted)
}

/// F1: the support sets' one-step search for one asserted fact over more than `POLL_UNITS`
/// producers, every one an empty scan. Cancelled when the first scan opens, the search ends
/// `Cancelled` within `POLL_UNITS` attempted units more, not after every producer.
#[test]
fn empty_support_searches_count_towards_the_stop() {
    use crate::graph_sets::{Limits, Stopped, UNWATCHED, rederived};
    let n = 3 * POLL_UNITS;
    let mut vocabulary = LocalVocabulary::default();
    let (closure, ground, fact) = empty_producers(&mut vocabulary, n, "", "");
    assert_eq!(
        ground.unpolled().producers(fact).len(),
        n,
        "a producer per subproperty"
    );
    let source = Asked::new(closure.clone());
    let asserted = |f: Triple| closure.binary_search(&f).is_ok();
    let stop = || source.cancel.load(Ordering::Relaxed);
    let limits = Limits {
        watch: UNWATCHED,
        stop: &stop,
    };
    let result = rederived(
        &source,
        &ground,
        &[],
        &asserted,
        &[fact],
        usize::MAX,
        limits,
    );
    assert_eq!(result.err(), Some(Stopped::Cancelled));
    let opened = source.scans.load(Ordering::Relaxed);
    let bound = 1 + POLL_UNITS;
    assert!(bound < n, "the bound proves nothing");
    assert!(
        opened <= bound,
        "{opened} scans opened, the first of them cancelling (bound {bound}; {n} producers)"
    );
}

/// F2: DRed's one-step derivation checks, asked after their operation stopped, read nothing
/// and answer false: not the bodiless shortcut (its schema facts), not a producer's search.
#[test]
fn a_derivation_check_after_its_stop_reads_nothing() {
    let mut vocabulary = LocalVocabulary::default();
    let (closure, ground, fact) = empty_producers(
        &mut vocabulary,
        100,
        "sco: (?c rdfs:subClassOf ?d) -> (?c rdfs:comment ?d)",
        "ex:C rdfs:subClassOf ex:D",
    );
    let bodiless = load(&mut vocabulary, "ex:C rdfs:comment ex:D")[0];
    assert!(ground.bodiless.contains(&bodiless), "a bodiless fact");
    let all = vec![true; ground.rules.len()];
    // Guard: unstopped, the bodiless shortcut decides it.
    assert!(ground.derivable_by(&Asked::new(closure.clone()), bodiless, &all, true, NEVER));
    let (cancelled, unlimited) = (|| true, || None::<nrese_exec::memory::MemoryStopCause>);
    let stopped = OperationStop::with_poll(&cancelled, &unlimited);
    assert!(
        stopped.fired(),
        "the operation has stopped before the checks are asked"
    );
    let fired = || stopped.fired();
    for (what, fact) in [("the bodiless shortcut", bodiless), ("100 producers", fact)] {
        let source = Asked::new(closure.clone());
        let by = ground.derivable_by(&source, fact, &all, true, &fired);
        assert_eq!(
            (by, source.reads()),
            (false, [0; 3]),
            "derivable_by, {what}"
        );
        let source = Asked::new(closure.clone());
        let with = ground.derivable_with(&source, fact, true, &fired);
        assert_eq!(
            (with, source.reads()),
            (false, [0; 3]),
            "derivable_with, {what}"
        );
    }
}

/// A store whose every position of `p` is stale, as `Current` hides the facts of a
/// representative that lost its place: it counts `n` positions, and its range scans visit
/// them and deliver none. The first range scan sets `cancel`.
struct Stale {
    p: u64,
    n: usize,
    ranges: AtomicUsize,
    cancel: AtomicBool,
}

impl Source for Stale {
    fn scan(&self, _: [Option<u64>; 3], _: Seg, _: &mut dyn FnMut(Triple)) {}

    fn estimate(&self, pattern: [Option<u64>; 3], _: Seg) -> usize {
        if pattern[1] == Some(self.p) {
            self.n
        } else {
            0
        }
    }

    fn contains(&self, _: Triple) -> bool {
        false
    }

    fn matches_len(&self, pattern: [Option<u64>; 3], _: Seg) -> Option<usize> {
        (pattern[1] == Some(self.p)).then_some(self.n)
    }

    fn scan_range(
        &self,
        _: [Option<u64>; 3],
        _: Seg,
        _: std::ops::Range<usize>,
        _: &mut dyn FnMut(Triple),
    ) {
        self.ranges.fetch_add(1, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// F3: the consistency check over driver ranges that deliver nothing (every position
/// filtered before the callback), more than `POLL_UNITS` positions in several morsels.
/// Cancelled during the first range, it opens no further range and ends `Interrupted`.
#[test]
fn filtered_driver_positions_count_towards_the_stop() {
    use crate::delta::Interrupted;
    use crate::violations::Violations;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules("clash: (?x rdfs:p ?y) -> false", &mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let p = load(&mut vocabulary, "ex:x rdfs:p ex:y")[0][1];
    let source = Stale {
        p,
        n: 10 * crate::eval::MORSEL,
        ranges: AtomicUsize::new(0),
        cancel: AtomicBool::new(false),
    };
    let mut program = GroundProgram::default();
    program.ground(&source, &schema, &rules);
    assert_eq!(program.consistency.len(), 1, "the check's rule");
    let stop = || source.cancel.load(Ordering::Relaxed);
    let mut found = Violations::unlimited();
    let result = batch::check(
        &source,
        &program,
        0..program.consistency.len(),
        &[],
        &mut found,
        &stop,
    );
    assert_eq!(result.err(), Some(Interrupted::default()));
    let ranges = source.ranges.load(Ordering::Relaxed);
    assert!(
        ranges <= 2,
        "{ranges} ranges opened, the first of them cancelling (of {})",
        source.n / crate::eval::MORSEL
    );
}

/// F1 (sol-loop r7, R7-1): one changed bodiless fact with more than `POLL_UNITS` premise
/// alternatives (one per schema fact that grounds it). Each alternative is attempted work:
/// cancelled at the first read of its earlier sets, the update reads at most `POLL_UNITS`
/// more and ends `Cancelled`.
#[test]
fn each_premise_alternative_counts_towards_the_stop() {
    use crate::eval::Grounding;
    use crate::graph_sets::{Annotation, Change, Limits, Stopped, UNWATCHED, update_support_sets};
    let n = 3 * POLL_UNITS;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "sco: (?c rdfs:subClassOf ?d) -> (?c rdfs:comment ?c)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let text: Vec<String> = (0..n)
        .map(|i| format!("ex:C rdfs:subClassOf ex:D{i}"))
        .collect();
    let mut facts = load(&mut vocabulary, &text.join("\n"));
    facts.sort_unstable();
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let inferred = batch::materialise(&facts, &rules, None, &schema).derived;
    let ground = crate::delta::program(&MemoryBase::new(&facts, &inferred), compiled);
    let fact = load(&mut vocabulary, "ex:C rdfs:comment ex:C")[0];
    assert_eq!(
        ground
            .unpolled()
            .premise_alternatives(Grounding::Bodiless(fact))
            .len(),
        n,
        "an alternative per subclass axiom"
    );
    let mut closure = facts;
    closure.extend(&inferred);
    closure.sort_unstable();
    let source = batch::Store::new(closure.clone());
    let asked = AtomicUsize::new(0);
    let previous = |_: Triple| {
        asked.fetch_add(1, Ordering::Relaxed);
        Annotation::default()
    };
    let touched = [fact];
    let change = Change {
        source: &source,
        in_closure: &|f| closure.binary_search(&f).is_ok(),
        touched: &touched,
        previous: &previous,
        limit: usize::MAX,
    };
    let stop = || asked.load(Ordering::Relaxed) >= 1;
    let limits = Limits {
        watch: UNWATCHED,
        stop: &stop,
    };
    let result = update_support_sets(&change, &ground, &[], &|_| vec![0], 1, limits);
    assert_eq!(result.err(), Some(Stopped::Cancelled));
    let read = asked.load(Ordering::Relaxed);
    let bound = 1 + POLL_UNITS;
    assert!(bound < n, "the bound proves nothing");
    assert!(
        read <= bound,
        "{read} premise sets read, the first of them cancelling (bound {bound}; {n} alternatives)"
    );
}

/// F1 (sol-loop r6, R6-1): a support-set update's first pass over more than `POLL_UNITS`
/// changed facts. Each fact's prelude (its bodiless sets, which read the sets the facts
/// had before, and its producers) is attempted work: cancelled during the first fact's,
/// the pass reads the earlier sets of at most `POLL_UNITS` more facts, and the update ends
/// `Cancelled`. One thread, so the bound is one task's.
#[test]
fn a_support_update_s_first_pass_counts_each_fact() {
    use crate::graph_sets::{Annotation, Change, Limits, Stopped, UNWATCHED, update_support_sets};
    let n = 3 * POLL_UNITS;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "sco: (?c rdfs:subClassOf ?d) -> (?c rdfs:comment ?d)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let text: Vec<String> = (0..n)
        .map(|i| format!("ex:C{i} rdfs:subClassOf ex:D{i}"))
        .collect();
    let mut facts = load(&mut vocabulary, &text.join("\n"));
    facts.sort_unstable();
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let inferred = batch::materialise(&facts, &rules, None, &schema).derived;
    assert_eq!(inferred.len(), n, "a bodiless fact per subclass axiom");
    let ground = crate::delta::program(&MemoryBase::new(&facts, &inferred), compiled);
    assert!(inferred.iter().all(|f| ground.bodiless.contains(f)));
    let mut closure = facts;
    closure.extend(&inferred);
    closure.sort_unstable();
    // The bodiless facts changed graphs: the update recomputes each of them.
    let touched = inferred;
    let source = batch::Store::new(closure.clone());
    let asked = AtomicUsize::new(0);
    let previous = |_: Triple| {
        asked.fetch_add(1, Ordering::Relaxed);
        Annotation::default()
    };
    let change = Change {
        source: &source,
        in_closure: &|f| closure.binary_search(&f).is_ok(),
        touched: &touched,
        previous: &previous,
        limit: usize::MAX,
    };
    let stop = || asked.load(Ordering::Relaxed) >= 1;
    let limits = Limits {
        watch: UNWATCHED,
        stop: &stop,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let result =
        pool.install(|| update_support_sets(&change, &ground, &[], &|_| vec![0], 1, limits));
    assert_eq!(result.err(), Some(Stopped::Cancelled));
    let read = asked.load(Ordering::Relaxed);
    let bound = 1 + POLL_UNITS;
    assert!(bound < n, "the bound proves nothing");
    assert!(
        read <= bound,
        "{read} facts' earlier sets read, the first of them cancelling (bound {bound}; {n} facts)"
    );
}
