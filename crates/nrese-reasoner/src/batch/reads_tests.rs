//! The store's in-memory reads as the rules make them, against REAS-2's stop contract and a
//! plain model: `Current`'s filtered scan, and the range reads of `Store` and `Current`
//! (`Source::scan_range` over `Store::visit_range`).
//!
//! The contract a stop oracle checks, from a trace of one task's charges, asks and work
//! ([`witness::traced`]):
//! - every unit of work (a position, a collected driver) spends credit paid before it, by
//!   the charge that pays for it, and no ask since that charge has voided it: an ask voids
//!   all credit paid before it, except the charge that made it ask. So nothing charged
//!   before an ask is done after the next ask, and no work is moved before its charge. A
//!   probe or a filter call belongs to its position and needs that position's credit
//!   unvoided;
//! - the work between two asks, and after the last ask up to the end (`Poller::ended` asks
//!   nothing, so a stop pending there is never seen), is within `POLL_UNITS` plus one run;
//! - no charge is larger than the read's run;
//! - after a refusal, nothing: no work, no delivery, no charge, no ask;
//! - a stopped read answers `Err(Stopped)`, never `Ok` with a part of its answer.

use super::*;
use crate::witness::{self, Event};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const INTERVAL: usize = nrese_exec::POLL_UNITS as usize;

// A range read is one run: its range, at most `MORSEL`, is at most `POLL_UNITS`.
const _: () = assert!(MORSEL <= INTERVAL);

/// Which of a task's asks a stop counts: those made inside a test's callback (by a read the
/// callback makes, nested in the one delivering to it) or the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nesting {
    Own,
    Nested,
}

/// A task's stop: it answers the asks after the poller's making, numbered from 1 in each
/// [`Nesting`], refuses at `refuse`, and traces each answer.
struct Asks {
    made: AtomicBool,
    depth: AtomicUsize,
    own: AtomicUsize,
    nested: AtomicUsize,
    refuse: Option<(Nesting, usize)>,
}

impl Asks {
    fn new(refuse: Option<(Nesting, usize)>) -> Self {
        Self {
            made: AtomicBool::new(false),
            depth: AtomicUsize::new(0),
            own: AtomicUsize::new(0),
            nested: AtomicUsize::new(0),
            refuse,
        }
    }

    fn ask(&self) -> bool {
        if !self.made.load(Ordering::Relaxed) {
            return false;
        }
        let nesting = match self.depth.load(Ordering::Relaxed) {
            0 => Nesting::Own,
            _ => Nesting::Nested,
        };
        let count = match nesting {
            Nesting::Own => &self.own,
            Nesting::Nested => &self.nested,
        };
        let n = count.fetch_add(1, Ordering::Relaxed) + 1;
        let refused = self.refuse == Some((nesting, n));
        witness::trace(Event::Ask(refused));
        refused
    }

    fn enter(&self) {
        witness::trace(Event::Enter);
        self.depth.fetch_add(1, Ordering::Relaxed);
    }

    fn leave(&self) {
        self.depth.fetch_sub(1, Ordering::Relaxed);
        witness::trace(Event::Leave);
    }
}

/// What a read's oracle allows it.
struct Bound {
    /// The read's run: the largest single charge it makes ahead.
    run: usize,
    /// Which charge pays for a position: the read's first (a range, counted ahead whole) or
    /// the latest before it at its depth (a scan's run counted ahead). A collected driver is
    /// paid by its job's first charge, the range's.
    payer: Payer,
}

#[derive(Clone, Copy)]
enum Payer {
    First,
    Latest,
}

/// A task's trace and what its stop counted.
struct Traced {
    events: Vec<Event>,
    own: usize,
    nested: usize,
    errors: Vec<String>,
}

/// A task: reads on its poller, `Err` answers checked into the errors.
type Op<'o> = dyn Fn(&Poller<'_>, &Asks, &RefCell<Vec<String>>) + 'o;

fn traced_run(refuse: Option<(Nesting, usize)>, op: &Op<'_>) -> Traced {
    let asks = Asks::new(refuse);
    let stop = || asks.ask();
    let poll = Poller::new(&stop).expect("not stopped");
    asks.made.store(true, Ordering::Relaxed);
    let errors = RefCell::new(Vec::new());
    let (_, events) = witness::traced(|| op(&poll, &asks, &errors));
    Traced {
        events,
        own: asks.own.load(Ordering::Relaxed),
        nested: asks.nested.load(Ordering::Relaxed),
        errors: errors.into_inner(),
    }
}

/// Whether `read` answered as its poller says: `Err` once stopped, `Ok` only if not. Returns
/// whether it stopped.
fn answered(
    read: Result<(), Stopped>,
    poll: &Poller<'_>,
    what: &str,
    errors: &RefCell<Vec<String>>,
) -> bool {
    if read.is_err() != poll.stopped() {
        errors.borrow_mut().push(format!(
            "{what} answered {read:?} with its poller stopped: {}",
            poll.stopped()
        ));
    }
    read.is_err()
}

/// A test's callback for reads on `poll`: traced, and with `lookup`, a nested lookup of each
/// fact delivered on the same poller (as a rule body's next atom is read), its answer
/// checked.
fn callback<'c>(
    asks: &'c Asks,
    poll: &'c Poller<'_>,
    lookup: Option<&'c Store>,
    errors: &'c RefCell<Vec<String>>,
) -> impl FnMut(Triple) + 'c {
    move |[s, p, o]: Triple| {
        asks.enter();
        if let Some(store) = lookup {
            let found = Source::scan(
                store,
                [Some(s), Some(p), Some(o)],
                Seg::All,
                poll,
                &mut |_| {},
            );
            answered(found, poll, "a nested lookup", errors);
        }
        asks.leave();
    }
}

/// A morsel loop's range reads of `pattern`: `MORSEL` positions at a time, until one stops.
fn ranges<'o, S: Source + ?Sized>(
    source: &'o S,
    pattern: [Option<u64>; 3],
    seg: Seg,
    lookup: Option<&'o Store>,
) -> impl Fn(&Poller<'_>, &Asks, &RefCell<Vec<String>>) + 'o {
    let len = Source::matches_len(source, pattern, seg, &Poller::never())
        .expect("never stopped")
        .expect("a range source");
    move |poll: &Poller<'_>, asks: &Asks, errors: &RefCell<Vec<String>>| {
        for start in (0..len).step_by(MORSEL) {
            let range = start..len.min(start + MORSEL);
            witness::trace(Event::Begin);
            let read = Source::scan_range(
                source,
                pattern,
                seg,
                range.clone(),
                poll,
                &mut callback(asks, poll, lookup, errors),
            );
            if answered(read, poll, &format!("the range {range:?}"), errors) {
                return;
            }
        }
    }
}

/// One scan of `pattern`.
fn scan<'o, S: Source + ?Sized>(
    source: &'o S,
    pattern: [Option<u64>; 3],
    seg: Seg,
    lookup: Option<&'o Store>,
) -> impl Fn(&Poller<'_>, &Asks, &RefCell<Vec<String>>) + 'o {
    move |poll: &Poller<'_>, asks: &Asks, errors: &RefCell<Vec<String>>| {
        witness::trace(Event::Begin);
        let read = Source::scan(
            source,
            pattern,
            seg,
            poll,
            &mut callback(asks, poll, lookup, errors),
        );
        answered(read, poll, "the scan", errors);
    }
}

const WORK: [&str; 4] = ["positions", "probes", "filter calls", "drivers"];

fn work(event: Event) -> Option<usize> {
    match event {
        Event::Position => Some(0),
        Event::Probe => Some(1),
        Event::Keep => Some(2),
        Event::Driver => Some(3),
        _ => None,
    }
}

/// The stop contract's failures in one trace (the module's list).
fn check(case: &str, bound: &Bound, traced: &Traced) -> Vec<String> {
    let mut failures: Vec<String> = traced
        .errors
        .iter()
        .map(|error| format!("{case}: {error}"))
        .collect();
    let events = &traced.events;
    let limit = INTERVAL + bound.run;
    let mut asks = 0;
    let mut asks_before = Vec::with_capacity(events.len());
    for &event in events {
        asks_before.push(asks);
        asks += usize::from(matches!(event, Event::Ask(_)));
    }
    // The ask a charge made, right after it: it doesn't void that charge.
    let own_ask = |c: usize| usize::from(matches!(events.get(c + 1), Some(Event::Ask(_))));
    let mut depth = 0;
    // At each depth: the latest charge, and the charge that paid its last position or
    // driver; the first charge of the read under way (depth 0).
    let mut latest: Vec<Option<usize>> = vec![None];
    let mut paid_by: Vec<Option<usize>> = vec![None];
    let (mut first, mut awaiting) = (None, false);
    let mut spent: HashMap<usize, usize> = HashMap::new();
    let mut gap = [0; 4];
    let mut oversized = None;
    let mut voided = None;
    let mut overspent = None;
    for (i, &event) in events.iter().enumerate() {
        match event {
            Event::Charge(n) => {
                if n > bound.run && oversized.is_none() {
                    oversized = Some(n);
                }
                latest[depth] = Some(i);
                if depth == 0 && awaiting {
                    (first, awaiting) = (Some(i), false);
                }
            }
            Event::Ask(_) => {
                for (k, &done) in gap.iter().enumerate() {
                    if done > limit {
                        failures.push(format!(
                            "{case}: {done} {} between two asks, before event {i}: more than \
                             {INTERVAL} plus one run of {}",
                            WORK[k], bound.run
                        ));
                    }
                }
                gap = [0; 4];
            }
            Event::Begin => (first, awaiting) = (None, true),
            Event::Enter => {
                depth += 1;
                if latest.len() <= depth {
                    latest.push(None);
                    paid_by.push(None);
                }
                (latest[depth], paid_by[depth]) = (None, None);
            }
            Event::Leave => depth -= 1,
            Event::Position | Event::Probe | Event::Keep | Event::Driver => {
                let k = work(event).expect("work");
                gap[k] += 1;
                let payer = match event {
                    Event::Probe | Event::Keep => paid_by[depth],
                    Event::Driver => first,
                    _ => match (depth, bound.payer) {
                        (0, Payer::First) => first,
                        _ => latest[depth],
                    },
                };
                let Some(c) = payer else {
                    if voided.is_none() {
                        voided = Some(format!("{} at event {i} before any charge", WORK[k]));
                    }
                    continue;
                };
                let since = asks_before[i] - asks_before[c] - own_ask(c);
                if since > 0 && voided.is_none() {
                    voided = Some(format!(
                        "{} at event {i} spends the charge at event {c}, voided by {since} asks \
                         since",
                        WORK[k]
                    ));
                }
                if matches!(event, Event::Position | Event::Driver) {
                    paid_by[depth] = Some(c);
                    let Event::Charge(units) = events[c] else {
                        unreachable!("a payer is a charge")
                    };
                    let spent = spent.entry(c).or_default();
                    *spent += 1;
                    if *spent > units && overspent.is_none() {
                        overspent = Some((i, c, units));
                    }
                }
            }
        }
    }
    for (k, &done) in gap.iter().enumerate() {
        if done > limit {
            failures.push(format!(
                "{case}: {done} {} after the last ask, to the end (a stop pending there is \
                 never asked): more than {INTERVAL} plus one run of {}",
                WORK[k], bound.run
            ));
        }
    }
    if let Some(n) = oversized {
        failures.push(format!(
            "{case}: a charge of {n} units: the read's run is {}",
            bound.run
        ));
    }
    if let Some(voided) = voided {
        failures.push(format!("{case}: {voided}"));
    }
    if let Some((i, c, units)) = overspent {
        failures.push(format!(
            "{case}: work at event {i} past the {units} units the charge at event {c} paid"
        ));
    }
    if let Some(r) = events.iter().position(|&e| e == Event::Ask(true)) {
        let mut after = [0; 4];
        let mut rest = 0;
        for &event in &events[r + 1..] {
            match work(event) {
                Some(k) => after[k] += 1,
                None => {
                    rest += usize::from(matches!(
                        event,
                        Event::Enter | Event::Charge(_) | Event::Ask(_)
                    ));
                }
            }
        }
        for (k, &done) in after.iter().enumerate() {
            if done > 0 {
                failures.push(format!("{case}: {done} {} after the refusal", WORK[k]));
            }
        }
        if rest > 0 {
            failures.push(format!(
                "{case}: {rest} deliveries, charges or asks after the refusal"
            ));
        }
    }
    failures
}

/// Runs `op` unrefused, then refusing at each of its asks of `refusals` in turn, and checks
/// every trace against `bound`.
fn oracle(case: &str, bound: &Bound, refusals: Nesting, op: &Op<'_>) -> Vec<String> {
    let whole = traced_run(None, op);
    let mut failures = check(&format!("{case}, unrefused"), bound, &whole);
    let asks = match refusals {
        Nesting::Own => whole.own,
        Nesting::Nested => whole.nested,
    };
    if asks < 2 {
        failures.push(format!(
            "{case}: {asks} {refusals:?} asks: the case doesn't reach its stop"
        ));
    }
    for k in 1..=asks {
        let at = format!("{case}, refused at {refusals:?} ask {k}");
        let refused = traced_run(Some((refusals, k)), op);
        if !refused.events.contains(&Event::Ask(true)) {
            failures.push(format!("{at}: never refused"));
            continue;
        }
        failures.extend(check(&at, bound, &refused));
    }
    failures
}

fn verdict(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} failures, the first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(12)]
    );
}

/// Subject 7's key over every run of relation 5 (input 4,096 pairs, base 8,192, recent
/// 2,000 of which the delta 1,000, so the old segment probes the recent run), and relation 6
/// with an input only (its base and recent runs empty). Chunks of the unit tests' 16 pairs:
/// a scan's runs counted ahead are a chunk's slice.
fn oracle_store() -> Store {
    let key = |from: u64, n: u64| -> Vec<Triple> { (from..from + n).map(|o| [7, 5, o]).collect() };
    let mut input = key(1_000, 4_096);
    input.extend((0..300).map(|i| [9, 6, 100 + i]));
    let mut store = Store::new(input);
    store.advance(key(20_000, 8_192));
    store.advance(key(40_000, 1_000));
    store.advance(key(60_000, 1_000));
    let five = store.relation(5).expect("relation 5");
    assert_eq!(
        [
            five.input.len(),
            five.base.len(),
            five.recent.len(),
            five.delta.len()
        ],
        [4_096, 8_192, 2_000, 1_000],
        "relation 5's runs"
    );
    assert!(
        !five.recent_is_delta(),
        "the old segment reads the recent run"
    );
    let six = store.relation(6).expect("relation 6");
    assert_eq!(
        [six.input.len(), six.base.len(), six.recent.len()],
        [300, 0, 0],
        "relation 6's runs"
    );
    store
}

/// Stale: every fifth object of subject 7's key, and every third of subject 9's.
fn oracle_stale() -> TermSet {
    let mut stale = TermSet::default();
    for o in (1_000..5_096)
        .chain(20_000..28_192)
        .chain(40_000..41_000)
        .chain(60_000..61_000)
    {
        if o % 5 == 0 {
            stale.insert(o);
        }
    }
    for i in (0..300).step_by(3) {
        stale.insert(100 + i);
    }
    stale
}

/// A key's range and every relation's facts: old positions with probes, stale facts, the
/// delta's facts skipped in the old segment, and empty runs.
const CASES: [([Option<u64>; 3], Seg); 2] = [
    ([Some(7), Some(5), None], Seg::Old),
    ([None, None, None], Seg::All),
];

/// A range read's run: its range, counted ahead whole, at most `MORSEL`.
const RANGE: Bound = Bound {
    run: MORSEL,
    payer: Payer::First,
};

/// A scan's run: a run of a relation's positions counted ahead, at most [`RUN`].
const SCAN: Bound = Bound {
    run: RUN,
    payer: Payer::Latest,
};

/// REAS-2's stop contract (the module's list) for the range reads, the store's and
/// `Current`'s over it, read as a morsel loop reads them (a range of `MORSEL` positions at a
/// time, its callback making no read), the stop refusing at each of the task's asks in turn.
/// One run is the range, counted ahead whole: at most `MORSEL`, which is at most
/// `POLL_UNITS`.
#[test]
fn range_reads_keep_the_stop_contract() {
    let store = oracle_store();
    let stale = oracle_stale();
    let current = Current {
        store: &store,
        stale: &stale,
    };
    let mut failures = Vec::new();
    for (pattern, seg) in CASES {
        let case = format!("Store's ranges of {pattern:?} in {seg:?}");
        let read = ranges(&store, pattern, seg, None);
        failures.extend(oracle(&case, &RANGE, Nesting::Own, &read));
        let case = format!("Current's ranges of {pattern:?} in {seg:?}");
        let read = ranges(&current, pattern, seg, None);
        failures.extend(oracle(&case, &RANGE, Nesting::Own, &read));
    }
    verdict(failures);
}

/// REAS-2's stop contract (the module's list) for `Current`'s filtered scan, over range
/// patterns (a point lookup reads before it counts until step 2 of the one-layer reads), its
/// callback making no read, the stop refusing at each of its asks in turn. One run is a run
/// of a relation's positions counted ahead, at most [`RUN`].
#[test]
fn current_s_scan_keeps_the_stop_contract() {
    let store = oracle_store();
    let stale = oracle_stale();
    let current = Current {
        store: &store,
        stale: &stale,
    };
    let mut failures = Vec::new();
    for (pattern, seg) in CASES {
        let case = format!("Current's scan of {pattern:?} in {seg:?}");
        let read = scan(&current, pattern, seg, None);
        failures.extend(oracle(&case, &SCAN, Nesting::Own, &read));
    }
    verdict(failures);
}

/// The charges of `read` on a poller that never stops, and the facts it delivers.
fn charges(read: impl FnOnce(&Poller<'_>, &mut dyn FnMut(Triple))) -> (Vec<usize>, usize) {
    let poll = Poller::never();
    let mut delivered = 0;
    let (_, events) = witness::traced(|| read(&poll, &mut |_| delivered += 1));
    let charged = events
        .into_iter()
        .filter_map(|event| match event {
            Event::Charge(n) => Some(n),
            _ => None,
        })
        .collect();
    (charged, delivered)
}

/// REAS-2 (master, 8 Nov 05:29: a wrapper's filtered positions are counted once, by the
/// source it wraps): `Current`'s scan and range read charge what the store's charge, in the
/// same order, the stale facts they filter out included. With every fact of subject 7's key
/// stale, `Current` delivers none, and a poller whose stop has fired since it was made still
/// stops its scan. Counted only as delivered, it would never poll.
#[test]
fn current_s_filtered_positions_are_counted_by_the_store() {
    let store = oracle_store();
    let mut stale = TermSet::default();
    stale.insert(7);
    let current = Current {
        store: &store,
        stale: &stale,
    };
    let pattern = [Some(7), Some(5), None];
    let (stored, all) = charges(|poll, f| {
        let _ = Source::scan(&store, pattern, Seg::Old, poll, f);
    });
    let (filtered, none) = charges(|poll, f| {
        let _ = Source::scan(&current, pattern, Seg::Old, poll, f);
    });
    assert_eq!(
        all, 13_288,
        "the store's facts: the delta's skipped in the old segment"
    );
    assert_eq!(none, 0, "every fact stale");
    assert_eq!(filtered, stored, "the scan's charges");
    let range = 2_000..2_000 + MORSEL;
    let (stored, all) = charges(|poll, f| {
        let _ = Source::scan_range(&store, pattern, Seg::Old, range.clone(), poll, f);
    });
    let (filtered, none) = charges(|poll, f| {
        let _ = Source::scan_range(&current, pattern, Seg::Old, range.clone(), poll, f);
    });
    assert_eq!(
        all, MORSEL,
        "the range's facts: in the input and the base, none skipped"
    );
    assert_eq!(none, 0, "every fact stale");
    assert_eq!(filtered, stored, "the range read's charges");
    let fired = AtomicBool::new(false);
    let stop = || fired.load(Ordering::Relaxed);
    let poll = Poller::new(&stop).expect("not stopped yet");
    fired.store(true, Ordering::Relaxed);
    let mut delivered = 0;
    let scanned = Source::scan(&current, pattern, Seg::Old, &poll, &mut |_| delivered += 1);
    assert_eq!(
        scanned,
        Err(Stopped),
        "14,288 positions filtered out without a poll"
    );
    assert_eq!(delivered, 0);
}

/// A position of the model: its fact, whether its chunk marks it (passed over), and whether
/// it is read as old (probed) and in the delta (then skipped).
struct Spot {
    fact: Triple,
    marked: bool,
    old: bool,
    in_delta: bool,
}

impl Spot {
    /// Whether the store hands it on (to a filter, if any).
    fn handed(&self) -> bool {
        !self.marked && !(self.old && self.in_delta)
    }
}

/// The model's positions of `pattern` in `seg`, in the order the store reads them: per
/// relation (in the store's order), its runs (input, base, recent; the delta alone), in each
/// run its pairs sorted by the bound term first. A marked pair is a position of a range but
/// a marked point lookup has none, and all marked leaves none (`Run::matching_unmarked`).
fn spots(store: &Store, [s, p, o]: [Option<u64>; 3], seg: Seg) -> Vec<Spot> {
    let mut out = Vec::new();
    for (&q, relation) in store.predicates.iter().zip(&store.relations) {
        if p.is_some_and(|p| p != q) {
            continue;
        }
        let delta: HashSet<Pair> = relation.delta.so.iter().copied().collect();
        let marks = relation.skips(seg);
        let runs: Vec<(&Run, bool)> = match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => vec![(&relation.delta, false)],
            Seg::All => vec![
                (&relation.input, false),
                (&relation.base, false),
                (&relation.recent, false),
            ],
            Seg::Old => {
                let mut runs = vec![(&relation.input, false), (&relation.base, false)];
                if !relation.recent_is_delta() {
                    runs.push((&relation.recent, true));
                }
                runs
            }
        };
        if marks.is_some_and(|marks| marks.all) {
            continue;
        }
        for (run, old) in runs {
            let mut pairs: Vec<Pair> = run
                .so
                .iter()
                .copied()
                .filter(|&(a, b)| s.is_none_or(|s| s == a) && o.is_none_or(|o| o == b))
                .collect();
            if s.is_none() && o.is_some() {
                pairs.sort_unstable_by_key(|&(a, b)| (b, a));
            }
            for (a, b) in pairs {
                let marked = marks.is_some() && marked_pair(relation, (a, b));
                if marked && s.is_some() && o.is_some() {
                    continue;
                }
                out.push(Spot {
                    fact: [a, q, b],
                    marked,
                    old,
                    in_delta: delta.contains(&(a, b)),
                });
            }
        }
    }
    out
}

/// Whether the relation's delta marks `pair` (its closed part).
fn marked_pair(relation: &Relation, pair: Pair) -> bool {
    let marks = relation.marks.as_ref().expect("marks");
    relation
        .delta
        .so
        .position(pair)
        .is_some_and(|(c, i)| marks.all || marked(&marks.so[c], i))
}

/// What one read did: its deliveries and the work it was seen to do.
#[derive(Debug, PartialEq, Eq)]
struct Done {
    facts: Vec<Triple>,
    work: [usize; 4],
}

fn done(read: impl FnOnce(&Poller<'_>, &mut dyn FnMut(Triple))) -> (Done, usize) {
    let poll = Poller::never();
    let mut facts = Vec::new();
    let (_, events) = witness::traced(|| read(&poll, &mut |fact| facts.push(fact)));
    let mut seen = [0; 4];
    let mut charged = 0;
    for event in events {
        match event {
            Event::Charge(n) => charged += n,
            event => {
                if let Some(k) = work(event) {
                    seen[k] += 1;
                }
            }
        }
    }
    (Done { facts, work: seen }, charged)
}

/// What the model says a read of `spots` delivers and does, through a filter of `stale`
/// (`None`: none); its positions and probes traced if `traced`.
fn expected(spots: &[&Spot], stale: Option<&TermSet>, traced: bool) -> Done {
    let handed = spots.iter().filter(|spot| spot.handed());
    Done {
        facts: handed
            .clone()
            .filter(|spot| stale.is_none_or(|stale| !stale.mentioned(spot.fact)))
            .map(|spot| spot.fact)
            .collect(),
        work: [
            match traced {
                true => spots.iter().filter(|spot| !spot.marked).count(),
                false => 0,
            },
            match traced {
                true => spots.iter().filter(|spot| !spot.marked && spot.old).count(),
                false => 0,
            },
            match stale {
                Some(_) => handed.count(),
                None => 0,
            },
            0,
        ],
    }
}

/// Random facts over few terms, so keys span chunks and runs.
struct Random(u64);

impl Random {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % below
    }

    fn pairs(&mut self, n: u64) -> std::collections::BTreeSet<Pair> {
        (0..n)
            .map(|_| (self.next(6), 100 + self.next(40)))
            .collect()
    }

    fn facts(&mut self, n: u64) -> Vec<Triple> {
        let mut facts: Vec<Triple> = (0..n)
            .map(|_| [self.next(6), 5 + self.next(3), 100 + self.next(40)])
            .collect();
        facts.sort_unstable();
        facts.dedup();
        facts
    }
}

/// Spec (REAS-2's reads as the rules make them): over random relations in random runs (an
/// input, then three rounds; on odd seeds relation 7 a closed family's, its delta's closed
/// part marked, all of it on every fourth seed), with random stale terms (none on every third
/// seed), the store's and `Current`'s scans and range reads deliver exactly a plain model's
/// facts, in its order, and visit exactly its positions, probes and filter calls: every
/// shape of pattern, every segment, ranges over the whole, inside it and empty. A position
/// is traced where a run is read in counted runs (`in_runs`) and by the range read; a scan's
/// point lookups and marked runs trace none yet. What a read charges is at least what it was
/// seen to visit; the totals are printed. A recharge after a nested ask may charge more than
/// today's reads: design r3 states the excess allowed.
#[test]
fn reads_match_a_plain_model() {
    let segs = [
        Seg::All,
        Seg::Old,
        Seg::Delta,
        Seg::DeltaNotBy(1),
        Seg::DeltaNotBy(2),
    ];
    let (mut charged_total, mut visited_total) = (0, 0);
    let mut failures = Vec::new();
    for seed in 0..24u64 {
        let mut random = Random(seed * 7_919 + 1);
        let n = 60 + random.next(140);
        let mut input = random.facts(n);
        input.push([0, 7, 100]);
        let mut store = Store::new(input);
        for _ in 0..3 {
            let n = random.next(120);
            let round = random.facts(n);
            store.advance(round);
        }
        if seed % 2 == 1 {
            let mut relation = Relation {
                closed_by: Some(1),
                ..Relation::default()
            };
            for _ in 0..2 {
                let n = 10 + random.next(60);
                let open = random.pairs(n).into_iter().collect();
                let (pairs, marks) = new_pairs(vec![open], Vec::new());
                relation.advance(pairs, marks);
            }
            let held: HashSet<Pair> = relation.pairs().into_iter().collect();
            let fresh: Vec<Pair> = random
                .pairs(40)
                .into_iter()
                .filter(|pair| !held.contains(pair))
                .collect();
            let (closed, open): (Vec<Pair>, Vec<Pair>) = fresh
                .into_iter()
                .partition(|&(a, b)| seed % 4 == 3 || (a + b) % 3 == 0);
            let (pairs, marks) = new_pairs(vec![open], vec![closed]);
            relation.advance(pairs, marks);
            let at = store.index[&7];
            store.relations[at] = relation;
        }
        let mut stale = TermSet::default();
        if seed % 3 != 0 {
            for _ in 0..3 {
                stale.insert(random.next(6));
                stale.insert(100 + random.next(40));
            }
        }
        let current = Current {
            store: &store,
            stale: &stale,
        };
        let filter = (!stale.is_empty()).then_some(&stale);
        let mut patterns = Vec::new();
        for p in [Some(5), Some(6), Some(7), Some(99), None] {
            let (s, o) = (random.next(6), 100 + random.next(40));
            patterns.extend([
                [Some(s), p, None],
                [None, p, Some(o)],
                [None, p, None],
                [Some(s), p, Some(o)],
                [Some(0), p, Some(100)],
            ]);
        }
        for pattern in patterns {
            let point = pattern[0].is_some() && pattern[2].is_some();
            for seg in segs {
                let model = spots(&store, pattern, seg);
                let all: Vec<&Spot> = model.iter().collect();
                let at = format!("seed {seed}, {pattern:?} in {seg:?}");
                let traced = !point
                    && store
                        .predicates
                        .iter()
                        .zip(&store.relations)
                        .all(|(&q, relation)| {
                            pattern[1].is_some_and(|p| p != q) || relation.skips(seg).is_none()
                        });
                for (name, filtered) in [("Store", None), ("Current", filter)] {
                    let source: &dyn Source = match filtered {
                        None => &store,
                        Some(_) => &current,
                    };
                    let (seen, charged) = done(|poll, f| {
                        Source::scan(source, pattern, seg, poll, f).expect("never stopped")
                    });
                    let mut want = expected(&all, filtered, traced);
                    if !traced {
                        want.work[0] = seen.work[0];
                        want.work[1] = seen.work[1];
                    }
                    if seen != want {
                        failures.push(format!("{at}: {name}'s scan {seen:?}, the model {want:?}"));
                    }
                    if charged < seen.work[0] {
                        failures.push(format!(
                            "{at}: {name}'s scan charged {charged} for {} positions",
                            seen.work[0]
                        ));
                    }
                    charged_total += charged;
                    visited_total += seen.work[0];
                    let len = Source::matches_len(source, pattern, seg, &Poller::never())
                        .expect("never stopped")
                        .expect("every run has its object order");
                    if len != model.len() {
                        failures.push(format!("{at}: {len} positions, the model {}", model.len()));
                        continue;
                    }
                    let (a, b) = (
                        random.next(len as u64 + 1) as usize,
                        random.next(len as u64 + 1) as usize,
                    );
                    let k = random.next(len as u64 + 1) as usize;
                    for range in [0..len, a.min(b)..a.max(b), k..k] {
                        let (seen, charged) = done(|poll, f| {
                            Source::scan_range(source, pattern, seg, range.clone(), poll, f)
                                .expect("never stopped")
                        });
                        let want = expected(&all[range.clone()], filtered, true);
                        if seen != want {
                            failures.push(format!(
                                "{at}: {name}'s range {range:?} {seen:?}, the model {want:?}"
                            ));
                        }
                        if charged < seen.work[0] {
                            failures.push(format!(
                                "{at}: {name}'s range {range:?} charged {charged} for {} positions",
                                seen.work[0]
                            ));
                        }
                        charged_total += charged;
                        visited_total += seen.work[0];
                    }
                }
            }
        }
    }
    println!(
        "reads_match_a_plain_model: {charged_total} units charged, {visited_total} positions \
         traced"
    );
    verdict(failures);
}
