//! The join orderer's budget on its characteristic-set work ([`plan::SET_BUDGET`]): a planning
//! within it plans as it would without one, to the bit; one past it is planned once more
//! without the sets, and EXPLAIN says so; a cancelled one fails and isn't planned again. Each
//! case sets its budget ([`plan::BUDGET`]) from the work its planning does without one, so the
//! cases hold whatever the constant's value. Red on the orderer without the budget (the
//! parent commit), but for the guard that says so; the code review's round 3 cases on the
//! commit before them, and the chain's count on an orderer that doesn't count links.

use super::join_order_tests::{Steps, people, scan, steps};
use super::plan::{self, BUDGET, STAR_STATE, STATISTICS, STEP};
use super::*;
use crate::Resources;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// A star of four patterns, ordered exhaustively, whose plan the sets change.
const STAR: [&str; 4] = ["?x p0 ?a", "?x p1 ?b", "?x p3 ?c", "?x city ?d"];

/// `patterns` (as [`scan`] reads them) as a BGP's scans, each with its matches.
fn bgp(ctx: &Context<'_>, patterns: &[&str]) -> (Vec<ScanPattern>, Vec<u64>) {
    let scans: Vec<ScanPattern> = patterns.iter().map(|p| scan(&ctx.snapshot, p)).collect();
    let counts = scans
        .iter()
        .map(|s| ctx.snapshot.estimate_in(ctx.model, &s.quad_pattern()))
        .collect();
    (scans, counts)
}

/// The plan of `patterns` as a BGP's by `ctx`, with EXPLAIN's estimates, admitted to the sets
/// ([`plan::ADMIT`]: these small graphs expect too few rows for the planner's own gate) with
/// `budget` the orderer's; and how often it was planned again without the sets.
fn under(
    ctx: &Context<'_>,
    patterns: &[&str],
    budget: usize,
) -> Result<(plan::Plan, usize), QueryEvaluationError> {
    let (scans, counts) = bgp(ctx, patterns);
    BUDGET.set(Some(budget));
    plan::ADMIT.set(Some(true));
    REPLANS.set(0);
    let plan = ctx.join_order(&scans, &counts, &[], true);
    plan::ADMIT.set(None);
    BUDGET.set(None);
    let plan = plan.map_err(QueryEvaluationError::from)?;
    Ok((plan, REPLANS.get()))
}

/// The plan of `patterns` without the characteristic sets: the orderer's, given none, on the
/// same inputs; in `order` where the executor fixes it (two patterns).
fn without_sets(ctx: &Context<'_>, patterns: &[&str], order: Option<&[usize]>) -> Steps {
    let (scans, counts) = bgp(ctx, patterns);
    let mut vars = Vec::new();
    let inputs = ctx.plan_inputs(&scans, &counts, &[], &mut vars);
    let plan = match order {
        Some(order) => plan::along(
            &inputs,
            vars.len(),
            PROBE_FACTOR,
            None,
            plan::SET_BUDGET,
            order,
        ),
        None => plan::order(
            &inputs,
            vars.len(),
            PROBE_FACTOR,
            None,
            plan::SET_BUDGET,
            &|| false,
        ),
    };
    steps(&plan.unwrap())
}

/// The raw id of `<http://e/name>` in `snapshot`.
fn id(snapshot: &Snapshot, name: &str) -> u64 {
    let iri = NamedNode::new_unchecked(format!("http://e/{name}"));
    snapshot.lookup(iri.as_ref().into()).expect(name).raw()
}

/// A store of `triples`, each `(s, p, o)` of `<http://e/name>`s.
fn graph(triples: &[(&str, &str, &str)]) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let e = |name: &str| NamedNode::new_unchecked(format!("http://e/{name}"));
    let mut tx = engine.transaction();
    for &(s, p, o) in triples {
        tx.insert(Quad::new(e(s), e(p), e(o), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The work a step noting a BGP of `patterns` planned without the sets says was done before
/// `budget` was passed ([`Context::with_sets`]).
fn noted_work(detail: &str, patterns: usize, budget: usize) -> usize {
    let prefix = format!(
        "{patterns} patterns planned without characteristic sets: \
         estimate budget of {budget} passed after "
    );
    detail
        .strip_prefix(&prefix)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{detail}"))
}

/// Spec: the budget brief (12 Nov 2026), "the check, before every read" and "the fallback",
/// as its design review reconciled them: a planning whose work without a budget is W plans
/// with a budget of W as without one, to the bit, and isn't planned again. With W - 1, or
/// with less than its first read (`p0`'s sets: a predicate in more sets than the budget), it
/// is planned again once and gets the plan without the sets, its order and rows to the bit;
/// its work stays within the budget, none of it for the first read.
#[test]
fn past_the_budget_a_bgp_gets_the_plan_without_the_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (with, replans) = under(&ctx, &STAR, usize::MAX).unwrap();
    let none = without_sets(&ctx, &STAR, None);
    assert_ne!(steps(&with), none, "the sets change this plan");
    assert_eq!(replans, 0);
    let all = with.work;
    let (within, replans) = under(&ctx, &STAR, all).unwrap();
    assert_eq!(
        (steps(&within), within.work, replans),
        (steps(&with), all, 0),
        "a budget of all {all}"
    );
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    let first = sets.star_of(id(&snapshot, "p0")).len();
    for budget in [all.saturating_sub(1), first - 1] {
        let (past, replans) = under(&ctx, &STAR, budget).unwrap();
        assert_eq!(
            (steps(&past), replans),
            (none.clone(), 1),
            "a budget of {budget}, {all} without one"
        );
        assert!(past.work <= budget, "{} of {budget}", past.work);
    }
    let (past, _) = under(&ctx, &STAR, first - 1).unwrap();
    assert_eq!(past.work, 0, "the first read would pass the budget");
}

/// Spec: as [`past_the_budget_a_bgp_gets_the_plan_without_the_sets`], for a BGP ordered
/// greedily (13 patterns, more than the exhaustive search takes; the design review's round 2).
#[test]
fn past_the_budget_a_greedy_bgp_gets_the_plan_without_the_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let star: Vec<String> = (0..13).map(|i| format!("?x p{i} ?o{i}")).collect();
    let star: Vec<&str> = star.iter().map(String::as_str).collect();
    let (with, _) = under(&ctx, &star, usize::MAX).unwrap();
    let none = without_sets(&ctx, &star, None);
    assert_ne!(steps(&with), none, "the sets change this plan");
    let all = with.work;
    let (within, replans) = under(&ctx, &star, all).unwrap();
    assert_eq!((steps(&within), replans), (steps(&with), 0), "{all}");
    let (past, replans) = under(&ctx, &star, all.saturating_sub(1)).unwrap();
    assert_eq!((steps(&past), replans), (none, 1), "{all} without a budget");
    assert!(past.work < all, "{} of {all}", past.work);
}

/// Spec: the budget brief (12 Nov 2026), "what the count includes": the sets the exhaustive
/// search copies with its states count as their reads do. A budget of this star's reads
/// alone ([`STATISTICS`]; it reads no characteristic pairs), less than its reads and copies,
/// gets the plan without the sets.
#[test]
fn copies_of_the_sets_count_against_the_budget() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    STATISTICS.with(|c| c.set((0, 0)));
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    let (reads, _) = STATISTICS.with(|c| c.get());
    let (past, replans) = under(&ctx, &STAR, reads).unwrap();
    let none = without_sets(&ctx, &STAR, None);
    assert_eq!((steps(&past), replans), (none, 1), "a budget of {reads}");
    assert!(reads < with.work, "{reads} reads, {} in all", with.work);
}

/// Spec: the budget brief's addendum 2, "what the count includes", counted by hand. Over the
/// graph below, `?x p ?y . ?y q ?z` is planned `q` first (2 matches against 4), and its
/// planning reads `q`'s sets (one, {q}), then `p`'s links (two: from {p} into {q}, and into
/// an object that is no subject), each looking up `q`: 1 + 2 · 1 = 3. A budget of 3 keeps
/// the plan with the sets; one of 2, which leaves room for `q`'s set but not for the links,
/// gets the plan without them.
#[test]
fn a_chain_counts_the_links_it_reads() {
    let engine = graph(&[
        ("a1", "p", "b1"),
        ("a2", "p", "b1"),
        ("a3", "p", "b2"),
        ("a4", "p", "d"),
        ("b1", "q", "c"),
        ("b2", "q", "c"),
    ]);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    assert!(sets.has_pairs(), "the chain is estimated by the pairs");
    let chain = ["?x p ?y", "?y q ?z"];
    let (with, replans) = under(&ctx, &chain, usize::MAX).unwrap();
    assert_eq!(
        (with.order.as_slice(), with.work, replans),
        (&[1, 0][..], 3, 0)
    );
    let none = without_sets(&ctx, &chain, Some(with.order.as_slice()));
    assert_ne!(steps(&with), none, "the pairs change this plan");
    let (within, replans) = under(&ctx, &chain, 3).unwrap();
    assert_eq!(
        (steps(&within), replans),
        (steps(&with), 0),
        "a budget of 3"
    );
    let (past, replans) = under(&ctx, &chain, 2).unwrap();
    assert_eq!((steps(&past), replans), (none, 1), "a budget of 2");
}

/// Spec: the design review's round 1 (P1a), why the check comes before each read and not
/// before the search: `?s q ?a . ?s p ?b` with `p` in more sets than the budget and `q` in
/// one, without `p`. Its planning reads `q`'s set and then the star's one set, never `p`'s
/// list: with a budget of those two reads it keeps its plan with the sets (no subject has
/// both, 0 rows), with one less it gets the plan without them.
#[test]
fn a_long_list_the_planning_does_not_read_does_not_count() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let e = |name: &str| NamedNode::new_unchecked(format!("http://e/{name}"));
    let mut tx = engine.transaction();
    let mut add = |s: &str, p: &str| {
        tx.insert(Quad::new(e(s), e(p), e("o"), GraphName::DefaultGraph).as_ref());
    };
    for i in 0..10 {
        add(&format!("s{i}"), "p");
        add(&format!("s{i}"), &format!("r{i}"));
    }
    add("t", "q");
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    assert_eq!(sets.star_of(id(&snapshot, "p")).len(), 10, "`p`'s sets");
    let pair = ["?s q ?a", "?s p ?b"];
    let (with, replans) = under(&ctx, &pair, 2).unwrap();
    assert_eq!((with.rows[1], replans), (0.0, 0), "planned with the sets");
    let none = without_sets(&ctx, &pair, Some(with.order.as_slice()));
    assert_ne!(steps(&with), none, "the sets change this plan");
    let (past, replans) = under(&ctx, &pair, 1).unwrap();
    assert_eq!((steps(&past), replans), (none, 1), "a budget of 1");
}

/// Spec: the design review's round 2 (R2-P1), as the budget brief words it: a cancelled
/// planning fails the query as cancelled and isn't planned again, also one that would pass
/// the budget had it gone on. Guard: green without the budget too, as it is today.
#[test]
fn a_cancelled_planning_is_not_planned_again() {
    let engine = people();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token.clone()),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    // Cancelled at its first step; the budget it would pass at its end.
    STEP.set(Some(Box::new(move || {
        token.cancel();
    })));
    let planned = under(&ctx, &STAR, with.work.saturating_sub(1));
    STEP.set(None);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.err()
    );
    assert_eq!(REPLANS.get(), 0, "planned again");
}

/// Spec: the code review's round 3 (cancellation first): a query cancelled before its
/// planning fails as cancelled and isn't planned again, also where a greedy search (13
/// patterns) passes the budget with its starts, before the first step that asks. Not
/// cancelled, the same planning is planned again.
#[test]
fn a_query_cancelled_before_its_planning_is_not_planned_again() {
    let engine = people();
    let snapshot = engine.snapshot();
    let star: Vec<String> = (0..13).map(|i| format!("?x p{i} ?o{i}")).collect();
    let star: Vec<&str> = star.iter().map(String::as_str).collect();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (_, replans) = under(&ctx, &star, 0).unwrap();
    assert_eq!(replans, 1, "no read fits a budget of 0");
    let token = CancellationToken::new();
    token.cancel();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let planned = under(&ctx, &star, 0);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.err()
    );
    assert_eq!(REPLANS.get(), 0, "planned again");
}

/// Spec: the design review's round 2 (R2-P1): the planning without the sets still asks for
/// the query's cancellation at every step. It comes first, before a set search that would
/// pass its budget at its starts: cancelled at the plain planning's first step, the query
/// fails as cancelled, and nothing falls back.
#[test]
fn the_planning_without_the_sets_stops_when_cancelled() {
    let engine = people();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token.clone()),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    let first = sets.star_of(id(&snapshot, "p0")).len();
    let asked = std::rc::Rc::new(Cell::new(0usize));
    let count = std::rc::Rc::clone(&asked);
    STEP.set(Some(Box::new(move || {
        count.set(count.get() + 1);
        token.cancel();
    })));
    let planned = under(&ctx, &STAR, first - 1);
    STEP.set(None);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.err()
    );
    assert_eq!((REPLANS.get(), asked.get()), (0, 1), "fallbacks, steps");
}

/// Spec: the design (12 Nov 2026), "the fallback is visible", as the budget brief words
/// it: EXPLAIN shows a BGP planned without the sets because the budget was passed,
/// once, as the first step under the BGP, with the budget and the work done before it; and
/// nothing of the kind for a BGP planned within the budget (the constant's, which this star
/// reads far less than).
#[test]
fn explain_shows_a_bgp_planned_without_the_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    assert!(with.work <= plan::SET_BUDGET, "{} of the budget", with.work);
    let text = "SELECT * WHERE { ?x <http://e/p0> ?a . ?x <http://e/p1> ?b . \
                ?x <http://e/p3> ?c . ?x <http://e/city> ?d }";
    let query = Query::parse(text, None).unwrap();
    let explained = |budget: usize| {
        BUDGET.set(Some(budget));
        plan::ADMIT.set(Some(true));
        let explained = explain(&snapshot, &query, &options);
        plan::ADMIT.set(None);
        BUDGET.set(None);
        explained.unwrap().1
    };
    let budget = with.work.saturating_sub(1);
    let trace = explained(budget);
    let notes: Vec<usize> = (0..trace.len())
        .filter(|&i| trace[i].operator == "join order")
        .collect();
    assert_eq!(notes.len(), 1, "{trace:#?}");
    let (above, note) = (&trace[notes[0].saturating_sub(1)], &trace[notes[0]]);
    assert_eq!(
        (above.operator.as_str(), above.depth + 1),
        ("bgp", note.depth),
        "{trace:#?}"
    );
    let work = noted_work(&note.detail, 4, budget);
    assert!(work <= budget, "{work} of {budget}");
    let trace = explained(plan::SET_BUDGET);
    assert!(
        trace.iter().all(|s| s.operator != "join order"),
        "{trace:#?}"
    );
}

/// Spec: the code review's round 3 (EXPLAIN provenance): the plan without running
/// (`plan_query`) shows a BGP planned without the sets too, once, after the plan's nodes
/// (the order planned); and nothing of the kind within the budget.
#[test]
fn the_plan_without_running_shows_a_bgp_planned_without_the_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    let text = "SELECT * WHERE { ?x <http://e/p0> ?a . ?x <http://e/p1> ?b . \
                ?x <http://e/p3> ?c . ?x <http://e/city> ?d }";
    let query = Query::parse(text, None).unwrap();
    let planned = |budget: usize| {
        BUDGET.set(Some(budget));
        plan::ADMIT.set(Some(true));
        let planned = super::plan(&snapshot, &query, &options);
        plan::ADMIT.set(None);
        BUDGET.set(None);
        planned.unwrap().1
    };
    let budget = with.work.saturating_sub(1);
    let nodes = planned(budget);
    let notes: Vec<_> = nodes
        .iter()
        .filter(|s| s.operator == "join order")
        .collect();
    assert_eq!(notes.len(), 1, "{nodes:#?}");
    assert_eq!(nodes.last(), Some(notes[0]), "after the plan's nodes");
    assert_eq!((notes[0].depth, notes[0].estimated_rows), (0, None));
    let work = noted_work(&notes[0].detail, 4, budget);
    assert!(work <= budget, "{work} of {budget}");
    let nodes = planned(plan::SET_BUDGET);
    assert!(
        nodes.iter().all(|s| s.operator != "join order"),
        "{nodes:#?}"
    );
}

/// Spec: the code review's round 3 (EXPLAIN provenance): a cyclic BGP, which runs as a
/// worst-case-optimal join and isn't planned again, shows its planning without the sets in
/// executed EXPLAIN once, as the first step under the BGP; nothing of the kind within the
/// budget.
#[test]
fn explain_shows_a_cyclic_bgp_planned_without_the_sets() {
    let engine = graph(&[("a", "p", "b"), ("b", "q", "c"), ("c", "r", "a")]);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let triangle = ["?a p ?b", "?b q ?c", "?c r ?a"];
    let (with, _) = under(&ctx, &triangle, usize::MAX).unwrap();
    let text = "SELECT * WHERE { ?a <http://e/p> ?b . ?b <http://e/q> ?c . ?c <http://e/r> ?a }";
    let query = Query::parse(text, None).unwrap();
    let explained = |budget: usize| {
        BUDGET.set(Some(budget));
        plan::ADMIT.set(Some(true));
        let explained = explain(&snapshot, &query, &options);
        plan::ADMIT.set(None);
        BUDGET.set(None);
        explained.unwrap().1
    };
    let budget = with.work.saturating_sub(1);
    let trace = explained(budget);
    assert!(
        trace.iter().any(|s| s.operator == "wcoj"),
        "a worst-case-optimal join: {trace:#?}"
    );
    let notes: Vec<usize> = (0..trace.len())
        .filter(|&i| trace[i].operator == "join order")
        .collect();
    assert_eq!(notes.len(), 1, "{trace:#?}");
    let (above, note) = (&trace[notes[0].saturating_sub(1)], &trace[notes[0]]);
    assert_eq!(
        (above.operator.as_str(), above.depth + 1),
        ("bgp", note.depth),
        "{trace:#?}"
    );
    let work = noted_work(&note.detail, 3, budget);
    assert!(work <= budget, "{work} of {budget}");
    let trace = explained(plan::SET_BUDGET);
    assert!(
        trace.iter().all(|s| s.operator != "join order"),
        "{trace:#?}"
    );
}

/// Spec: the code review's round 3 re-check (a note per BGP, once): a cyclic BGP joined with
/// a property path it shares no variable with is planned with the path (3 patterns and 1
/// path), which notes that planning; its patterns then run as a worst-case-optimal join.
/// Past the budget, executed EXPLAIN shows that planning once, not also the estimate's.
#[test]
fn explain_notes_a_cyclic_bgp_planned_with_a_path_once() {
    let nodes: Vec<String> = (0..=10).map(|i| format!("x{i}")).collect();
    let mut triples = vec![("a", "p", "b"), ("b", "q", "c"), ("c", "r", "a")];
    for edge in nodes.windows(2) {
        triples.push((edge[0].as_str(), "s", edge[1].as_str()));
    }
    let engine = graph(&triples);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let text = "SELECT * WHERE { ?a <http://e/p> ?b . ?b <http://e/q> ?c . \
                ?c <http://e/r> ?a . ?x <http://e/s>+ ?y }";
    let query = Query::parse(text, None).unwrap();
    let explained = |budget: usize| {
        BUDGET.set(Some(budget));
        plan::ADMIT.set(Some(true));
        let explained = explain(&snapshot, &query, &options);
        plan::ADMIT.set(None);
        BUDGET.set(None);
        explained.unwrap().1
    };
    let trace = explained(0);
    for operator in ["paths ordered with patterns", "wcoj"] {
        assert!(
            trace.iter().any(|s| s.operator == operator),
            "{operator}: {trace:#?}"
        );
    }
    let notes: Vec<_> = trace
        .iter()
        .filter(|s| s.operator == "join order")
        .collect();
    assert_eq!(notes.len(), 1, "{trace:#?}");
    assert_eq!(noted_work(&notes[0].detail, 4, 0), 0);
    let trace = explained(plan::SET_BUDGET);
    assert!(
        trace.iter().all(|s| s.operator != "join order"),
        "{trace:#?}"
    );
}

/// The sets every start of `patterns` (open stars on `?x`, as [`STAR`]) reads: each
/// predicate's, by the sets' index.
fn start_reads(snapshot: &Snapshot, sets: &CharacteristicSets, predicates: &[&str]) -> Vec<usize> {
    predicates
        .iter()
        .map(|p| sets.sets_having(id(snapshot, p)))
        .collect()
}

/// Spec: the master's order of 12 November (A), "give up before spending", and its doubt
/// review: every start of a star reads its predicate's sets before the search first checks
/// its work. Where the first two starts' reads would pass the budget, the planning reads none
/// of them, is planned again once without the sets, and says it passed after the first
/// start's reads, as it said when it made them. For the exhaustive search (4 patterns) and
/// the greedy one (13). Red on the parent, which reads `p0`'s sets first.
#[test]
fn past_the_budget_at_its_starts_a_planning_reads_no_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    let greedy: Vec<String> = (0..13).map(|i| format!("?x p{i} ?o{i}")).collect();
    let greedy: Vec<&str> = greedy.iter().map(String::as_str).collect();
    for patterns in [&STAR[..], &greedy[..]] {
        let reads = start_reads(&snapshot, &sets, &["p0", "p1"]);
        assert!(reads.iter().all(|&n| n > 0), "{reads:?}");
        let budget = reads[0] + reads[1] - 1;
        let none = without_sets(&ctx, patterns, None);
        STATISTICS.with(|c| c.set((0, 0)));
        let (past, replans) = under(&ctx, patterns, budget).unwrap();
        let (read, _) = STATISTICS.with(|c| c.get());
        assert_eq!(
            (steps(&past), replans, past.work, read),
            (none, 1, reads[0], 0),
            "{} patterns, a budget of {budget}: plan, planned again, work, sets read",
            patterns.len()
        );
    }
}

/// Spec: as [`past_the_budget_at_its_starts_a_planning_reads_no_sets`], the other side: where
/// the starts' reads fit the budget, the search makes them and stops only at the read or copy
/// that passes it, as before.
#[test]
fn within_the_budget_at_its_starts_a_planning_reads_their_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let sets = snapshot.characteristic_sets_in(ctx.model).expect("sets");
    let starts: usize = start_reads(&snapshot, &sets, &["p0", "p1", "p3", "city"])
        .iter()
        .sum();
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    assert!(
        starts < with.work,
        "{starts} at the starts, {} in all",
        with.work
    );
    let none = without_sets(&ctx, &STAR, None);
    STATISTICS.with(|c| c.set((0, 0)));
    let (past, replans) = under(&ctx, &STAR, starts).unwrap();
    let (read, _) = STATISTICS.with(|c| c.get());
    assert_eq!((steps(&past), replans), (none, 1), "a budget of {starts}");
    assert!(
        past.work >= starts && read >= starts,
        "{} counted, {read} read, {starts} at the starts",
        past.work
    );
}

/// The plannings `with_sets` asks for, in order: with the sets (`true`) or without (`false`).
fn plannings(
    ctx: &Context<'_>,
    patterns: &[&str],
) -> (Result<plan::Plan, QueryEvaluationError>, Vec<bool>) {
    let (scans, counts) = bgp(ctx, patterns);
    let mut vars = Vec::new();
    let inputs = ctx.plan_inputs(&scans, &counts, &[], &mut vars);
    let asked = std::cell::RefCell::new(Vec::new());
    let cancelled = || {
        ctx.cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    };
    let order = |sets: Option<&CharacteristicSets>, budget: usize| {
        asked.borrow_mut().push(sets.is_some());
        plan::order(&inputs, vars.len(), PROBE_FACTOR, sets, budget, &cancelled)
    };
    let planned = ctx
        .with_sets(&inputs, &order)
        .map_err(QueryEvaluationError::from);
    (planned, asked.into_inner())
}

/// Spec: the master's decision of 13 November 23:58, "a group is refined by the sets only when
/// the expected work admits it" ([`plan::admits`]), with his order of 12 November 13:53, "plan
/// with the plain counts first": every BGP is planned without the sets first. One whose plain
/// plan expects too little, as every BGP on this small graph does under the planner's own gate
/// (its predicates in few sets and its set plan another: there was an allowance for short lists
/// until then), keeps that plan, reads no set and plans once, and EXPLAIN shows no note for it:
/// that is the normal case (the master's decision of 14 November 03:26; a BGP admitted and past
/// its budget is noted). Admitted, it is planned with the sets after, and gets their plan; past
/// its budget there, the plain plan, planned once again. Red with an allowance that admits a BGP
/// whose starts read few sets, and with a note for a BGP the gate keeps.
#[test]
fn a_bgp_is_refined_by_the_sets_only_where_its_expected_work_admits_it() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (with, _) = under(&ctx, &STAR, usize::MAX).unwrap();
    let none = without_sets(&ctx, &STAR, None);
    assert_ne!(steps(&with), none, "the sets change this plan");
    STATISTICS.with(|c| c.set((0, 0)));
    let (kept, asked) = plannings(&ctx, &STAR);
    let (read, _) = STATISTICS.with(|c| c.get());
    let kept = kept.unwrap();
    assert_eq!(
        (steps(&kept), kept.work, read, asked),
        (none.clone(), 0, 0, vec![false]),
        "the planner's own gate: plan, work, sets read, plannings"
    );
    let text = "SELECT * WHERE { ?x <http://e/p0> ?a . ?x <http://e/p1> ?b . \
                ?x <http://e/p3> ?c . ?x <http://e/city> ?d }";
    let query = Query::parse(text, None).unwrap();
    let trace = explain(&snapshot, &query, &options).unwrap().1;
    assert!(
        !trace.iter().any(|s| s.operator == "join order"),
        "{trace:#?}"
    );
    plan::ADMIT.set(Some(true));
    let (admitted, asked) = plannings(&ctx, &STAR);
    BUDGET.set(Some(with.work - 1));
    REPLANS.set(0);
    let (past, asked_past) = plannings(&ctx, &STAR);
    BUDGET.set(None);
    plan::ADMIT.set(None);
    assert_eq!(
        (steps(&admitted.unwrap()), asked),
        (steps(&with), vec![false, true]),
        "admitted: plan, plannings"
    );
    assert_eq!(
        (steps(&past.unwrap()), asked_past, REPLANS.get()),
        (none, vec![false, true], 1),
        "admitted, past its budget: plan, plannings, fallbacks"
    );
}

/// Spec: the budget's cancellation rule, and the doubt review of the gate (12 November): a
/// fixed order's planning (two patterns) doesn't ask for the cancellation, so a query cancelled
/// before it fails as cancelled between the plannings, whether the gate would keep it from the
/// sets or not, and never gets a plan. Red without the check after the plain plan.
#[test]
fn a_cancelled_query_with_a_fixed_order_gets_no_plan() {
    let engine = graph(&[("a1", "p", "b1"), ("a2", "p", "b2"), ("b1", "q", "c")]);
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    token.cancel();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let (scans, counts) = bgp(&ctx, &["?x p ?y", "?y q ?z"]);
    for admit in [None, Some(true)] {
        plan::ADMIT.set(admit);
        let planned = ctx.join_order(&scans, &counts, &[], true);
        plan::ADMIT.set(None);
        assert!(
            matches!(
                planned.map_err(QueryEvaluationError::from),
                Err(QueryEvaluationError::Cancelled)
            ),
            "admit {admit:?}"
        );
    }
}

/// Spec: as [`a_bgp_is_refined_by_the_sets_only_where_its_expected_work_admits_it`] and the
/// budget's cancellation rule: a query cancelled in its plain planning (planned first) fails as
/// cancelled, and its set search never runs, though the sets would admit it.
#[test]
fn a_query_cancelled_in_its_plain_planning_reads_no_sets() {
    let engine = people();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token.clone()),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    // Cancelled at the plain planning's first step.
    STEP.set(Some(Box::new(move || token.cancel())));
    plan::ADMIT.set(Some(true));
    let (planned, asked) = plannings(&ctx, &STAR);
    plan::ADMIT.set(None);
    STEP.set(None);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.err()
    );
    assert_eq!(asked, vec![false]);
}

/// Spec: the budget's cancellation rule and the code review of 13 November (F1): a query
/// cancelled before its BGP is planned fails as cancelled and plans nothing, with the sets
/// or without. Red without the check before the first planning.
#[test]
fn a_query_cancelled_before_its_planning_plans_nothing() {
    let engine = people();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    token.cancel();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let (planned, asked) = plannings(&ctx, &STAR);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.err()
    );
    assert_eq!(asked, Vec::<bool>::new(), "plannings");
}

/// Spec: the budget's cancellation rule, and the gate's second doubt review (12 November): a
/// query cancelled while its set search runs fails as cancelled, though the search (as a fixed
/// order's, which doesn't ask) returned a plan. Red without the check after it.
#[test]
fn a_query_cancelled_in_its_set_search_gets_no_plan() {
    let engine = people();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    let options = QueryOptions {
        resources: Resources {
            cancellation: Some(token.clone()),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let (scans, counts) = bgp(&ctx, &STAR);
    let mut vars = Vec::new();
    let inputs = ctx.plan_inputs(&scans, &counts, &[], &mut vars);
    let order = |sets: Option<&CharacteristicSets>, budget: usize| {
        let planned = plan::order(&inputs, vars.len(), PROBE_FACTOR, sets, budget, &|| false);
        if sets.is_some() {
            token.cancel();
        }
        planned
    };
    plan::ADMIT.set(Some(true));
    let planned = ctx
        .with_sets(&inputs, &order)
        .map_err(QueryEvaluationError::from);
    plan::ADMIT.set(None);
    assert!(
        matches!(planned, Err(QueryEvaluationError::Cancelled)),
        "{:?}",
        planned.ok().map(|p| steps(&p))
    );
}

/// Spec: the master's order of 13 November, "plain planning pays only for what it uses: no
/// search state built when the gate keeps a group from the sets": a search without the
/// characteristic sets keeps no star predicates or edges in its states (only the estimates
/// from the sets read them); the same patterns searched with the sets keep them. Red with
/// that state built whatever the search.
#[test]
fn a_search_without_the_sets_keeps_no_star_or_edge_state() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let (scans, counts) = bgp(&ctx, &STAR);
    let mut vars = Vec::new();
    let inputs = ctx.plan_inputs(&scans, &counts, &[], &mut vars);
    let sets = snapshot.characteristic_sets_in(ctx.model).unwrap();
    let kept = |sets: Option<&CharacteristicSets>| {
        STAR_STATE.set(0);
        plan::order(&inputs, vars.len(), PROBE_FACTOR, sets, usize::MAX, &|| {
            false
        })
        .unwrap();
        STAR_STATE.get()
    };
    assert_eq!(kept(None), 0, "without the sets");
    assert!(kept(Some(&*sets)) > 0, "with them");
}

/// Spec: as [`a_search_without_the_sets_keeps_no_star_or_edge_state`], "plain planning pays
/// only for what it uses": a BGP whose stars all have a constant object that keeps a share of
/// its predicate's statements (no open star, [`plan::Star::open`]) is planned once, without
/// the sets, which would give it the same plan, also where the gate would admit it. Red where
/// any star admits the set search.
#[test]
fn a_bgp_without_an_open_star_is_planned_once() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let patterns = ["?x type Student", "?x type Person", "a anchor ?x"];
    let (scans, counts) = bgp(&ctx, &patterns);
    let mut vars = Vec::new();
    let inputs = ctx.plan_inputs(&scans, &counts, &[], &mut vars);
    assert!(
        inputs.iter().any(|i| i.star.is_some())
            && !inputs.iter().any(|i| i.star.is_some_and(|s| s.open())),
        "stars, none open"
    );
    let sets = snapshot.characteristic_sets_in(ctx.model).unwrap();
    let with = plan::order(
        &inputs,
        vars.len(),
        PROBE_FACTOR,
        Some(&*sets),
        usize::MAX,
        &|| false,
    );
    plan::ADMIT.set(Some(true));
    let (planned, asked) = plannings(&ctx, &patterns);
    plan::ADMIT.set(None);
    assert_eq!(
        (steps(&planned.unwrap()), asked),
        (steps(&with.unwrap()), vec![false]),
        "the plan the sets give, planned once"
    );
}

/// Spec: the master's order of 12 November 13:53, "effort follows expected work", and the
/// gate's code review (F3, catalogue D16-559): a BGP's set search may do
/// [`plan::SET_ENTRIES_PER_ROW`] per row its plain plan expects where that is more than the
/// least budget. With the least budget ([`plan::FLOOR`]) below the work its set plan takes, a
/// BGP that expects enough rows keeps that plan, without falling back; given only that least
/// budget, it falls back to the plain plan. Admitted ([`plan::ADMIT`]: this small graph
/// expects too few rows for the gate), with no test's budget: the planner's own. Red where the
/// planning's budget doesn't follow the rows it expects.
#[test]
fn a_bgp_that_expects_much_work_plans_past_the_least_budget() {
    // Ten subjects with `a`, `b` and `c`, twenty of each; ninety with one `a` alone: the sets
    // say `b` follows `a` on few subjects, with many objects.
    let names: Vec<[String; 3]> = (0..10)
        .flat_map(|s| {
            (0..20).flat_map(move |k| {
                ["a", "b", "c"].map(|p| [format!("s{s}"), p.into(), format!("{p}{k}")])
            })
        })
        .chain((10..100).map(|s| [format!("s{s}"), "a".into(), "a0".into()]))
        .collect();
    let triples: Vec<(&str, &str, &str)> = names
        .iter()
        .map(|[s, p, o]| (s.as_str(), p.as_str(), o.as_str()))
        .collect();
    let engine = graph(&triples);
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let patterns = ["?x a ?y", "?x b ?z", "?x c ?w"];
    let (with, _) = under(&ctx, &patterns, usize::MAX).unwrap();
    let none = without_sets(&ctx, &patterns, None);
    assert!(
        with.work > 0 && steps(&with) != none,
        "the sets change the plan"
    );
    let least = with.work - 1;
    let (scans, counts) = bgp(&ctx, &patterns);
    plan::FLOOR.set(least);
    plan::ADMIT.set(Some(true));
    REPLANS.set(0);
    let planned = ctx.join_order(&scans, &counts, &[], false);
    let replans = REPLANS.get();
    plan::ADMIT.set(None);
    plan::FLOOR.set(plan::SET_BUDGET);
    assert_eq!(
        (
            steps(&planned.map_err(QueryEvaluationError::from).unwrap()),
            replans
        ),
        (steps(&with), 0),
        "the least budget {least} below its work, the rows it expects allow it"
    );
    let (floor, replans) = under(&ctx, &patterns, least).unwrap();
    assert_eq!(
        (steps(&floor), replans),
        (none, 1),
        "given the least budget only, the plain plan"
    );
}
