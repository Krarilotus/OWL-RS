//! REAS-2 fix check r1 (R2-F1, held counts): a count tells its visitor about a run's
//! tombstone words as it reads them, so a caller's stop ends the read within a run of words.

use super::merge::{LINEAR_UP_TO, SignedMerge};
use super::run::Run;
use super::{IndexVersion, Layout, Visitor};
use crate::quad::{AccessPlan, EncodedQuad, Permutation, QuadPattern};
use crate::term::{TermId, TermKind};
use crate::visit::{Account, Free, Stopped};
use crate::witness::{self, Site};
use std::cell::Cell;
use std::sync::Arc;

fn term(n: u64) -> TermId {
    TermId::new(TermKind::Iri, n + 1)
}

/// An account that allows its first `0` portions and refuses every one after.
struct Allow(u32);

impl Account for Allow {
    fn take(&mut self, _: u64) -> bool {
        let allowed = self.0 > 0;
        self.0 = self.0.saturating_sub(1);
        allowed
    }
}

/// REAS-3a fix check r1 F1 (design attacks D6 and r2 E3). Spec (`SignedMerge::step`): a merge
/// step its account stops leaves the merge stopped. Three ranges hold one key K: it is
/// inserted, deleted, then inserted again, and the first and last ranges hold 255 more keys
/// each, so taking K from the first decodes a block. The first step is stopped at each of its
/// portions in turn. It answers `Stopped`. After that, a step on an account that never stops
/// answers `Stopped` again and the plain iterator answers the end, and neither does any work.
/// Not stopped, the first step yields K with sign +1. So for both selectors: the three ranges
/// alone are selected linearly, and with `LINEAR_UP_TO` more ranges of one later key each,
/// by the heap.
#[test]
fn a_stopped_merge_step_stays_stopped() {
    let (p, o) = (term(1), term(2));
    let quad = |s| EncodedQuad::new(term(s), p, o, TermId::DEFAULT_GRAPH);
    let k = quad(10);
    let with = |from: u64| -> Vec<_> {
        std::iter::once(k)
            .chain((from..from + 255).map(quad))
            .collect()
    };
    let mut runs = vec![
        Run::from_delta(Layout::DefaultGraph, &with(100), &[]),
        Run::from_delta(Layout::DefaultGraph, &[], &[k]),
        Run::from_delta(Layout::DefaultGraph, &with(400), &[]),
    ];
    // The selection, then an advance for each of K's occurrences.
    stops_after_any_portion(&runs, Permutation::Spog.to_key(&k), 1 + 3);
    let later = (0..LINEAR_UP_TO as u64).map(|i| quad(1_000 + i));
    runs.extend(later.map(|q| Run::from_delta(Layout::DefaultGraph, &[q], &[])));
    // An advance with the heap's repair, one portion for each of K's occurrences.
    stops_after_any_portion(&runs, Permutation::Spog.to_key(&k), 3);
}

fn stops_after_any_portion(runs: &[Run], key: crate::quad::Key, portions: usize) {
    let parts = || {
        runs.iter().map(|run| {
            let perm = run.permutation(Permutation::Spog);
            (perm, 0, perm.keys.len())
        })
    };
    let mut stops = 0;
    for allowed in 0.. {
        let mut merge = SignedMerge::new(parts());
        match merge.step(&mut Allow(allowed)) {
            Ok(first) => {
                assert_eq!(first, Some((key, 1)), "K, inserted last, comes first");
                break;
            }
            Err(Stopped) => {
                stops += 1;
                let before = witness::get(Site::MergeWork);
                assert_eq!(merge.step(&mut Free), Err(Stopped), "stopped at {allowed}");
                assert_eq!(
                    merge.next(),
                    None,
                    "stopped at {allowed}: the plain read ends"
                );
                assert_eq!(
                    witness::get(Site::MergeWork),
                    before,
                    "stopped at {allowed}: no work after the stop"
                );
            }
        }
    }
    assert_eq!(
        stops,
        portions,
        "the first step's portions, over {} ranges",
        runs.len()
    );
}

/// The work a read's producers count: the runs it searches or probes, the comparisons its
/// searches make, the keys its scan passes and its merge's work.
fn read_work() -> u64 {
    [
        Site::RunProbes,
        Site::SearchWork,
        Site::ScanSteps,
        Site::MergeWork,
    ]
    .into_iter()
    .map(witness::get)
    .sum()
}

/// `read` run again for each `k` from 0, its visitor answering yes `k` times and then no,
/// until it finishes without a no: the most work done between two asks of the visitor, or
/// after its last ask until `read` returned, over all the runs.
fn largest_gap(read: impl Fn(Visitor<'_>)) -> u64 {
    let mut largest = 0;
    for yeses in 0.. {
        let (asked, last, most) = (Cell::new(0), Cell::new(read_work()), Cell::new(0));
        let gap = || {
            let now = read_work();
            most.set(most.get().max(now - last.replace(now)));
        };
        read(&|_| {
            gap();
            asked.set(asked.get() + 1);
            asked.get() <= yeses
        });
        gap();
        largest = largest.max(most.get());
        if asked.get() <= yeses {
            return largest;
        }
    }
    unreachable!("a read asks its visitor finitely often")
}

/// REAS-3a fix check r1 F1 (design attack r2, E2). Spec (`crate::visit`: a unit is a key
/// comparison a read's search makes, counted before it is made): a counted read's range
/// searches stay within its visitor's interval, also where they find nothing. An interval of
/// runs, 32 statements each, and a subject above every stored one, so that each run's range is
/// empty (no cursor, no merge step): stopped after each of its visitor's yeses, a counted
/// scan, count, existence probe and membership probe each do at most an interval of work
/// between two asks (and after the last). Counting one unit per run's search, a chunk of runs
/// counted at once, an interval of searches of about six comparisons each ran unseen.
#[test]
fn counted_reads_search_empty_ranges_within_their_visitor_s_interval() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let (p, o) = (term(1), term(2));
    let runs: Vec<_> = (0..interval)
        .map(|r| {
            let quads: Vec<_> = (0..32)
                .map(|i| EncodedQuad::new(term(10 + 32 * r + i), p, o, TermId::DEFAULT_GRAPH))
                .collect();
            Arc::new(Run::from_delta(Layout::DefaultGraph, &quads, &[]))
        })
        .collect();
    let version = IndexVersion {
        layout: Layout::DefaultGraph,
        runs: runs.into(),
        len: 32 * interval,
    };
    let above = term(10 + 32 * interval);
    let plan = AccessPlan::for_pattern(&QuadPattern {
        subject: Some(above),
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let quad = EncodedQuad::new(above, p, o, TermId::DEFAULT_GRAPH);
    let gaps = [
        (
            "scan",
            largest_gap(|visit| version.scan_plan_iter_counted(&plan, visit).for_each(drop)),
        ),
        (
            "count",
            largest_gap(|visit| {
                let _ = version.count_plan_visiting(&plan, visit);
            }),
        ),
        (
            "existence",
            largest_gap(|visit| {
                let _ = version.any_plan_counted(&plan, visit);
            }),
        ),
        (
            "membership",
            largest_gap(|visit| {
                let _ = version.contains_counted(&quad, visit);
            }),
        ),
    ];
    let over: Vec<_> = gaps.iter().filter(|(_, gap)| *gap > interval).collect();
    assert!(
        over.is_empty(),
        "work between two asks of the visitor past {}: {over:?}",
        interval
    );
}

/// REAS-3a fix check r1 F1 (design attacks D1, E4). Spec (`crate::visit`: a unit is a key
/// comparison a merge makes, a cursor it advances or a key it decodes, counted before it is
/// done): a counted scan steps through many deltas of one statement within its visitor's
/// interval. Three intervals of runs that insert and delete one statement in turn (inserted
/// last, so it is there), then a run with a second statement: stopped after each of its
/// visitor's yeses, the scan does at most an interval of work between two asks (and after
/// the last), and unstopped it reads both statements. Counting a step's cursors before it and
/// then taking it whole, the first step's three intervals of cursors ran unseen.
#[test]
fn a_counted_scan_steps_through_many_deltas_within_its_visitor_s_interval() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let (p, o) = (term(1), term(2));
    let k = EncodedQuad::new(term(10), p, o, TermId::DEFAULT_GRAPH);
    let l = EncodedQuad::new(term(11), p, o, TermId::DEFAULT_GRAPH);
    let mut runs: Vec<_> = (0..=3 * interval)
        .map(|i| match i % 2 {
            0 => Arc::new(Run::from_delta(Layout::DefaultGraph, &[k], &[])),
            _ => Arc::new(Run::from_delta(Layout::DefaultGraph, &[], &[k])),
        })
        .collect();
    runs.push(Arc::new(Run::from_delta(Layout::DefaultGraph, &[l], &[])));
    let version = IndexVersion {
        layout: Layout::DefaultGraph,
        runs: runs.into(),
        len: 2,
    };
    let plan = AccessPlan::for_pattern(&QuadPattern {
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let largest = largest_gap(|visit| version.scan_plan_iter_counted(&plan, visit).for_each(drop));
    let read: Vec<_> = version.scan_plan_iter_counted(&plan, &|_| true).collect();
    assert_eq!(
        read,
        [k, l],
        "the statement inserted last and the second one"
    );
    assert!(
        largest <= interval,
        "{largest} units of work between two asks of the visitor: at most {}",
        interval
    );
}

/// Spec (`IndexVersion::count_plan_visiting`, `Snapshot::count_in_visiting`'s contract):
/// `visit(n)` is called before the work it counts, `n` at most `POLL_UNITS`, and nothing more
/// is read once it returns false. A version of `n` quads `s_i p o`, then a run deleting all
/// but the first: counting `? p ?` reads that run's tombstone bitmap, `n / 64` words, twice
/// `POLL_UNITS` of them. A visitor that answers "stop" at its first call after any bitmap
/// word was read: the count answers nothing, and at most `POLL_UNITS` words plus the range's
/// two edge words were read. Each call reports at most `POLL_UNITS` units.
#[test]
fn a_held_count_reads_its_tombstones_within_a_run_of_its_visitor() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let n = 2 * interval * 64 + 1;
    let (p, o) = (term(1), term(2));
    let quads: Vec<EncodedQuad> = (0..n)
        .map(|i| EncodedQuad::new(term(10 + i), p, o, TermId::DEFAULT_GRAPH))
        .collect();
    let version = IndexVersion::empty(Layout::DefaultGraph)
        .with_run(Run::from_delta(Layout::DefaultGraph, &quads, &[]))
        .with_run(Run::from_delta(Layout::DefaultGraph, &[], &quads[1..]));
    let plan = AccessPlan::for_pattern(&QuadPattern {
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let before = witness::get(Site::TombstoneWords);
    let largest = std::cell::Cell::new(0);
    let counted = version.count_plan_visiting(&plan, &|units| {
        largest.set(largest.get().max(units));
        witness::get(Site::TombstoneWords) == before
    });
    let largest = largest.get();
    let read = witness::get(Site::TombstoneWords) - before;
    assert!(
        read <= interval + 2,
        "{read} bitmap words read: more than {interval} plus the two edge words before the \
         visitor was asked"
    );
    assert_eq!(counted, None, "a count its visitor stopped answers nothing");
    assert!(
        largest <= interval,
        "a visit of {largest} units: at most {interval} at once"
    );
}

/// REAS-3a, sol-loop r1 S2. Spec (`crate::visit`: a unit is each run a read searches or
/// probes, counted before it is looked at): a counted existence probe looks at its runs within
/// its visitor's interval. Three intervals of runs, one statement each, none with tombstones
/// and none matching the probe (so it reads every run): between two asks of the visitor (and
/// after the last) it looks at no more than an interval of runs. Reading every run's
/// tombstones before the first ask, it looked at all of them unseen.
#[test]
fn a_counted_existence_probe_looks_at_its_runs_within_its_visitor_s_interval() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let n = 3 * interval;
    let (p, o) = (term(1), term(2));
    let runs: Vec<_> = (0..n)
        .map(|i| {
            let quad = EncodedQuad::new(term(10 + i), p, o, TermId::DEFAULT_GRAPH);
            std::sync::Arc::new(Run::from_delta(Layout::DefaultGraph, &[quad], &[]))
        })
        .collect();
    let version = IndexVersion {
        layout: Layout::DefaultGraph,
        runs: runs.into(),
        len: n,
    };
    let plan = AccessPlan::for_pattern(&QuadPattern {
        subject: Some(term(5)),
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let last = std::cell::Cell::new(witness::get(Site::RunProbes));
    let largest = std::cell::Cell::new(0);
    let gap = || {
        let now = witness::get(Site::RunProbes);
        largest.set(largest.get().max(now - last.replace(now)));
    };
    let found = version.any_plan_counted(&plan, &|_| {
        gap();
        true
    });
    gap();
    assert_eq!(found, Ok(false), "no run holds the probed statement");
    let largest = largest.get();
    assert!(
        largest <= interval,
        "{largest} runs looked at between two asks of the visitor: at most {}",
        interval
    );
}

/// REAS-3a review r1 R2. Spec (`crate::visit`: a counted read asks its visitor before the work
/// it counts; a merge step over its cursors is one step, counted before it is taken): a counted
/// scan over many runs reads its cursors within its visitor's interval. Five intervals of runs,
/// one statement each, all matching (a range in every run, a cursor each): from the scan's
/// first ask to its third quad, between two asks of the visitor (and after the last), it
/// does no more than an interval of work: runs searched, their comparisons, cursors read and
/// the merge's work (fix check r1 F1: a step compared every cursor after one count). Its
/// credit's hint summed every cursor before each ask, five intervals unseen.
#[test]
fn a_counted_scan_reads_its_cursors_within_its_visitor_s_interval() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let n = 5 * interval;
    let (p, o) = (term(1), term(2));
    let runs: Vec<_> = (0..n)
        .map(|i| {
            let quad = EncodedQuad::new(term(10 + i), p, o, TermId::DEFAULT_GRAPH);
            std::sync::Arc::new(Run::from_delta(Layout::DefaultGraph, &[quad], &[]))
        })
        .collect();
    let version = IndexVersion {
        layout: Layout::DefaultGraph,
        runs: runs.into(),
        len: n,
    };
    let plan = AccessPlan::for_pattern(&QuadPattern {
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let work = || read_work() + witness::get(Site::CursorReads);
    let last = std::cell::Cell::new(work());
    let largest = std::cell::Cell::new(0);
    let gap = || {
        let now = work();
        largest.set(largest.get().max(now - last.replace(now)));
    };
    let visit = |_| {
        gap();
        true
    };
    let first: Vec<EncodedQuad> = version
        .scan_plan_iter_counted(&plan, &visit)
        .take(3)
        .collect();
    gap();
    assert_eq!(first.len(), 3, "every run's statement matches");
    let largest = largest.get();
    assert!(
        largest <= interval,
        "{largest} runs, cursors and keys read between two asks of the visitor: at most {}",
        interval
    );
}

/// An account granted runs of `run` units (or a portion, where that is more) that never
/// stops: the most work done after a grant past what the grant allowed.
struct Runs {
    run: u64,
    left: u64,
    granted: u64,
    since: u64,
    over: u64,
}

impl Runs {
    fn new(run: u64) -> Self {
        Self {
            run,
            left: 0,
            granted: 0,
            since: read_work(),
            over: 0,
        }
    }

    /// Ends the last grant; its excess counts too.
    fn over(mut self) -> u64 {
        self.grant(0);
        self.over
    }

    fn grant(&mut self, n: u64) {
        let done = read_work() - self.since;
        self.over = self.over.max(done.saturating_sub(self.granted));
        (self.granted, self.since) = (self.run.max(n), read_work());
        self.left = self.granted;
    }
}

impl Account for Runs {
    fn take(&mut self, n: u64) -> bool {
        if self.left < n {
            self.grant(n);
        }
        self.left -= n;
        true
    }
}

/// REAS-3a fix check r3 H1. Spec (`crate::visit`: after a yes at most the units it allowed
/// follow; `Ledger`: each portion's work is done before the next portion is taken): a merge
/// does no work after a grant that it counted before it. Two to six runs of 600 statements
/// each, their subjects alternating, merged with an account granted runs of 1 to 64 units:
/// by the linear selector (2 and 3 runs) and the heap (4 and 6). The linear selector counted
/// all its inputs' tests at once and tested the inputs after an advance's grant; the heap
/// counted an advance, then took the repair's portion, and advanced after the repair's grant.
#[test]
fn a_merge_does_no_work_after_a_grant_that_it_counted_before() {
    let (p, o) = (term(1), term(2));
    for k in [2, 3, 4, 6] {
        let runs: Vec<_> = (0..k)
            .map(|r| {
                let quads: Vec<_> = (0..600)
                    .map(|i| EncodedQuad::new(term(10 + k * i + r), p, o, TermId::DEFAULT_GRAPH))
                    .collect();
                Run::from_delta(Layout::DefaultGraph, &quads, &[])
            })
            .collect();
        for run in 1..=64 {
            let mut merge = SignedMerge::new(runs.iter().map(|delta| {
                let perm = delta.permutation(Permutation::Spog);
                (perm, 0, perm.keys.len())
            }));
            let mut account = Runs::new(run);
            let mut keys = 0u64;
            while merge.step(&mut account).expect("never stopped").is_some() {
                keys += 1;
            }
            assert_eq!(keys, 600 * k, "every statement of {k} runs");
            assert_eq!(
                account.over(),
                0,
                "{k} runs, grants of {run}: work past a grant"
            );
        }
    }
}
