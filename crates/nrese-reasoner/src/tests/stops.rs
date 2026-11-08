//! A stop that fires inside rule evaluation (RSH-18): the evaluation sees it within its
//! poll interval, not after a whole morsel's join, and the operation it stops is reported
//! interrupted, never complete.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::load;
use crate::batch::{self, Schema};
use crate::delta::{Base, Interrupted, MemoryBase, Rules, update_until};
use crate::eval::OperationStop;
use crate::ir::{Triple, parse_rules};
use crate::vocabulary::LocalVocabulary;

/// A base that counts the facts its scans hand out.
struct Counting {
    base: MemoryBase,
    scanned: AtomicUsize,
}

impl Base for Counting {
    fn scan(&self, pattern: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        self.base.scan(pattern, &mut |fact| {
            self.scanned.fetch_add(1, Ordering::Relaxed);
            f(fact);
        });
    }

    fn estimate(&self, pattern: [Option<u64>; 3]) -> usize {
        self.base.estimate(pattern)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.base.contains(fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.base.is_asserted(fact)
    }
}

/// One driver joined with two atoms of `n` facts each: n² bindings from one driver, in
/// one morsel.
const PRODUCT: &str = "(?s rdfs:p ?o), (?x rdfs:q ?y), (?u rdfs:q ?v)";

/// Once an update's stop fires, the evaluation under way ends within `POLL_UNITS` scanned
/// facts plus the scans open on its stack (one per body atom below the driver), and the
/// update is `Interrupted`. Each row's work is a product the base would scan in full:
/// a round's join, the consistency check through the new fact (one that reports every
/// binding, and one whose guard rejects them all), and DRed's search for a rederivation
/// that fails (N10 review r1, F2).
#[test]
fn an_evaluation_stops_within_its_poll_interval() {
    struct Row {
        what: &'static str,
        rules: String,
        asserted: Vec<String>,
        inserted: &'static str,
        deleted: &'static str,
        /// The facts the scans open on the stack can still hand out once it has stopped.
        open: usize,
        /// The facts the evaluation scans when nothing stops it.
        whole: usize,
    }
    let n = 300;
    let product: Vec<String> = (0..n)
        .map(|i| format!("ex:x{i} rdfs:q ex:y{i}"))
        .chain(["ex:a rdfs:p ex:b".to_owned()])
        .collect();
    let m = 100;
    let components: Vec<String> = ["a", "b"]
        .iter()
        .flat_map(|c| {
            (0..m)
                .flat_map(move |i| (0..m).map(move |j| format!("ex:{c}{i} rdfs:seeAlso ex:{c}{j}")))
        })
        .collect();
    let rows = [
        Row {
            what: "a round's join",
            rules: format!("join: {PRODUCT} -> (?s rdfs:r ?v)"),
            asserted: product.clone(),
            inserted: "ex:a rdfs:p ex:b",
            deleted: "",
            open: 2 * n,
            whole: n * n,
        },
        Row {
            what: "the consistency check",
            rules: format!("clash: {PRODUCT} -> false"),
            asserted: product.clone(),
            inserted: "ex:a rdfs:p ex:b",
            deleted: "",
            open: 2 * n,
            whole: n * n,
        },
        // A check whose bindings all fail its guard (checked once all are bound): it binds
        // the product and reports nothing, so a poll counted per firing never comes.
        Row {
            what: "a consistency check that never fires",
            rules: format!("clash: {PRODUCT}, ?o != rdfs:b -> false"),
            asserted: product
                .iter()
                .map(|line| line.replace("ex:a rdfs:p ex:b", "ex:a rdfs:p rdfs:b"))
                .collect(),
            inserted: "ex:a rdfs:p rdfs:b",
            deleted: "",
            open: 2 * n,
            whole: n * n,
        },
        Row {
            what: "a failing rederivation",
            rules: "chain: (?a rdfs:seeAlso ?b), (?b rdfs:seeAlso ?c), (?c rdfs:seeAlso ?d) \
                    -> (?a rdfs:isDefinedBy ?d)"
                .to_owned(),
            asserted: components,
            inserted: "",
            deleted: "ex:a0 rdfs:isDefinedBy ex:b0",
            open: 3 * m,
            whole: m * m,
        },
    ];
    for row in rows {
        let mut vocabulary = LocalVocabulary::default();
        let rules = parse_rules(&row.rules, &mut vocabulary).unwrap();
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
        };
        let asserted = load(&mut vocabulary, &row.asserted.join("\n"));
        let inserted = load(&mut vocabulary, row.inserted);
        let deleted = load(&mut vocabulary, row.deleted);
        let base = Counting {
            base: MemoryBase::new(&asserted, &[]),
            scanned: AtomicUsize::new(0),
        };
        let fires = 1000;
        let stop = || base.scanned.load(Ordering::Relaxed) >= fires;
        let result = update_until(&base, &inserted, &deleted, compiled, None, &stop);
        assert_eq!(result.err(), Some(Interrupted::default()), "{}", row.what);
        let scanned = base.scanned.load(Ordering::Relaxed);
        let bound = fires + nrese_exec::POLL_UNITS as usize + row.open;
        assert!(bound < row.whole, "{}: the bound proves nothing", row.what);
        assert!(
            scanned <= bound,
            "{}: {scanned} facts scanned after a stop at {fires} (bound {bound}; unstopped, \
             it scans at least {})",
            row.what,
            row.whole
        );
    }
}

/// A materialisation stopped at any of its polls, those inside a morsel's join and inside
/// the consistency check included, ends `Interrupted`, or (stopped past its last poll) with
/// the whole closure and every violation: never a result missing what the cut work would
/// have found. The stop is true at one poll only and false again after it, as a memory
/// reading back under the limit is (N10-W, DC-07).
#[test]
fn a_stopped_materialisation_is_never_reported_complete() {
    use nrese_exec::memory::MemoryStopCause;
    let n: usize = 200;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        &format!("join: {PRODUCT} -> (?s rdfs:r ?v)\nclash: {PRODUCT} -> false"),
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let text: Vec<String> = (0..n)
        .map(|i| format!("ex:x{i} rdfs:q ex:y{i}"))
        .chain(["ex:a rdfs:p ex:b".to_owned()])
        .collect();
    let input = load(&mut vocabulary, &text.join("\n"));
    let unlimited = || None::<MemoryStopCause>;
    let asks = AtomicUsize::new(0);
    let counting = || {
        asks.fetch_add(1, Ordering::Relaxed);
        false
    };
    let whole = batch::materialise_owned_with(
        input.clone(),
        &rules,
        None,
        &schema,
        &OperationStop::with_poll(&counting, &unlimited),
    )
    .expect("runs");
    assert_eq!(whole.derived.len(), n, "the join's facts");
    assert_eq!(
        whole.violations.to_vec().len(),
        n * n,
        "the clash's bindings"
    );
    let polls = asks.load(Ordering::Relaxed);
    let inside = 2 * n * n / nrese_exec::POLL_UNITS as usize;
    assert!(
        polls >= inside,
        "{polls} polls: the sweep must reach the {inside} inside the join and the check"
    );
    for k in 0..=polls {
        let seen = AtomicUsize::new(0);
        let once = || seen.fetch_add(1, Ordering::Relaxed) == k;
        let stop = OperationStop::with_poll(&once, &unlimited);
        match batch::materialise_owned_with(input.clone(), &rules, None, &schema, &stop) {
            Err(stopped) => assert_eq!(stopped, Interrupted::default(), "poll {k} of {polls}"),
            Ok(closure) => {
                assert_eq!(
                    closure.derived, whole.derived,
                    "stopped at poll {k} of {polls}: reported complete without the join's facts"
                );
                assert_eq!(
                    closure.violations, whole.violations,
                    "stopped at poll {k} of {polls}: reported complete without every violation"
                );
            }
        }
    }
}

/// A support-set update (graph access) whose change's forward cone is one driver joined
/// with two atoms of 300 facts each: once its caller's stop fires, the round under way
/// ends within `POLL_UNITS` scanned facts plus the two scans open on its stack, and the
/// update ends `Cancelled`, with no sets (RSH-18, the support facet).
#[test]
fn a_cancelled_support_update_stops_within_its_poll_interval() {
    use crate::eval::{Seg, Source};
    use crate::graph_sets::{
        Change, Limits, Previous, Sets, Stopped, UNWATCHED, update_support_sets,
    };

    /// The closure, counting the facts its scans hand out.
    struct Closure {
        facts: batch::Store,
        scanned: AtomicUsize,
    }
    impl Source for Closure {
        fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
            self.facts.scan(pattern, seg, &mut |fact| {
                self.scanned.fetch_add(1, Ordering::Relaxed);
                f(fact);
            });
        }
        fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
            self.facts.estimate(pattern, seg)
        }
        fn contains(&self, fact: Triple) -> bool {
            self.facts.contains(fact)
        }
    }

    let n = 300;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        &format!("join: {PRODUCT} -> (?s rdfs:r ?v)"),
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let text: Vec<String> = (0..n)
        .map(|i| format!("ex:x{i} rdfs:q ex:y{i}"))
        .chain(["ex:a rdfs:p ex:b".to_owned()])
        .collect();
    let mut asserted = load(&mut vocabulary, &text.join("\n"));
    asserted.sort_unstable();
    let inferred = batch::materialise(&asserted, &rules, None, &schema).derived;
    assert_eq!(inferred.len(), n, "the join's facts");
    let ground = crate::delta::program(&MemoryBase::new(&asserted, &inferred), compiled);
    let mut closure: Vec<Triple> = asserted.iter().chain(&inferred).copied().collect();
    closure.sort_unstable();
    let touched = load(&mut vocabulary, "ex:a rdfs:p ex:b");
    let source = Closure {
        facts: batch::Store::new(closure.clone()),
        scanned: AtomicUsize::new(0),
    };
    let previous = |_: Triple| Previous::Sets(Sets::NONE);
    let change = Change {
        source: &source,
        in_closure: &|f| closure.binary_search(&f).is_ok(),
        touched: &touched,
        previous: &previous,
        limit: usize::MAX,
    };
    let fires = 1000;
    let stop = || source.scanned.load(Ordering::Relaxed) >= fires;
    let scratch = std::sync::Arc::new(nrese_exec::Budget::unlimited());
    let limits = Limits {
        watch: UNWATCHED,
        stop: &stop,
        scratch: &scratch,
    };
    let result = update_support_sets(&change, &ground, &[], &|_: Triple| Some(0), 1, limits);
    assert_eq!(result.err(), Some(Stopped::Cancelled));
    let scanned = source.scanned.load(Ordering::Relaxed);
    let bound = fires + nrese_exec::POLL_UNITS as usize + 2 * n;
    assert!(bound < n * n, "the bound proves nothing");
    assert!(
        scanned <= bound,
        "{scanned} facts scanned after a stop at {fires} (bound {bound}; the round's join \
         scans {})",
        n * n
    );
}
