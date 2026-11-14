//! A point read's stops and paths (D16-543, design note `2026-11-14-d16-543-point-reads-design.md`
//! r3, tests 2 and 3; D16-574, design note `2026-11-14-d16-574-lever2-point-loop-design.md` r3):
//! the point read does no work its visits did not allow, on each of its paths, takes the
//! expanded path where equality applies and its own elsewhere, sums each stack's signs by that
//! stack's layout, and ends as its caller says.

use std::cell::Cell;
use std::ops::ControlFlow;

use nrese_rdf::NamedNode;

use crate::index::Layout;
use crate::quad::{GraphSelector, QuadPattern};
use crate::visit::{Stopped, Visitor};
use crate::witness::{self, Site};
use crate::{
    CompactionPolicy, CountedSnapshot, EncodedQuad, EncodedTriple, Engine, EngineConfig, ReadModel,
    Snapshot, TermId,
};

/// [`CountedSnapshot::point_graphs`] with a caller that goes on after each graph, as a list:
/// the point's quad in each graph it was handed, then [`Stopped`] where the read answered that.
pub(super) fn point_answers(
    counted: &CountedSnapshot,
    model: ReadModel,
    point: [TermId; 3],
    visit: Visitor<'_>,
) -> Vec<Result<EncodedQuad, Stopped>> {
    let mut answers = Vec::new();
    let end = counted.point_graphs(model, point, visit, |graph| {
        answers.push(Ok(quad(point, graph)));
        Ok(ControlFlow::Continue(()))
    });
    if end == Err(Stopped) {
        answers.push(Err(Stopped));
    }
    answers
}

const MODELS: [ReadModel; 3] = [
    ReadModel::Materialised,
    ReadModel::Asserted,
    ReadModel::Inferred,
];

/// The work a read's producers witness: the runs it searches, its searches' comparisons, the
/// keys its scans pass and its merges' work (`index::count_tests`' `read_work`).
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

/// The design note's oracle (r3, E1; `index::count_tests`' `largest_gap`, per visit): `read`
/// run once for each refusal point, its visitor answering yes `yeses` times and then no, from
/// `yeses` = 0 (stopped before it starts) until a run is not refused. In every run, the work
/// witnessed in each gap, from the read's start to its first visit, between two visits and from
/// its last visit to its end, is within what the visit before it allowed: nothing before the
/// first, the units a yes was asked for, nothing after a no. A refused run answers its quads so
/// far, a prefix of the whole answer, then [`Stopped`]. Returns the whole answer.
fn refusal_sweep(
    case: &str,
    read: impl Fn(Visitor<'_>) -> Vec<Result<EncodedQuad, Stopped>>,
) -> Vec<EncodedQuad> {
    let mut prefixes: Vec<Vec<EncodedQuad>> = Vec::new();
    for yeses in 0.. {
        let (asked, allowed, last) = (Cell::new(0u64), Cell::new(0u64), Cell::new(read_work()));
        let gap = |at: &str| {
            let now = read_work();
            let done = now - last.replace(now);
            assert!(
                done <= allowed.get(),
                "{case}, refused after {yeses} yeses: {done} units of work {at}, allowed {}",
                allowed.get()
            );
        };
        let answers = read(&|n| {
            gap(&format!("before visit {}", asked.get() + 1));
            asked.set(asked.get() + 1);
            let yes = asked.get() <= yeses;
            allowed.set(if yes { n } else { 0 });
            yes
        });
        gap("after the last visit");
        if asked.get() <= yeses {
            let whole: Vec<_> = answers
                .into_iter()
                .map(|answer| answer.unwrap_or_else(|Stopped| panic!("{case}: never refused")))
                .collect();
            for (yeses, prefix) in prefixes.iter().enumerate() {
                assert!(
                    whole.starts_with(prefix),
                    "{case}, refused after {yeses} yeses: {prefix:?} not a prefix of {whole:?}"
                );
            }
            return whole;
        }
        let (last, delivered) = answers.split_last().expect("a refused read says so");
        assert_eq!(*last, Err(Stopped), "{case}, refused after {yeses} yeses");
        prefixes.push(
            delivered
                .iter()
                .map(|answer| answer.expect("quads before the refusal"))
                .collect::<Vec<_>>(),
        );
    }
    unreachable!("a read asks its visitor finitely often")
}

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

fn engine(fanout: u64) -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        compaction: CompactionPolicy {
            fanout,
            ..CompactionPolicy::default()
        },
        ..EngineConfig::default()
    })
    .unwrap()
}

/// The terms of a statement `[s, p, o]`, a missing object `o2`, and named graphs `g0`, `g1`, ...
struct Terms {
    point: [TermId; 3],
    miss: [TermId; 3],
    graphs: Vec<TermId>,
}

fn terms(engine: &Engine, graphs: usize) -> Terms {
    let tx = engine.transaction();
    let id = |name: &str| tx.intern(node(name).as_ref().into());
    let [s, p, o, o2] = ["s", "p", "o", "o2"].map(id);
    let graphs = (0..graphs).map(|g| id(&format!("g{g}"))).collect();
    tx.commit().unwrap();
    Terms {
        point: [s, p, o],
        miss: [s, p, o2],
        graphs,
    }
}

fn quad([s, p, o]: [TermId; 3], graph: TermId) -> EncodedQuad {
    EncodedQuad::new(s, p, o, graph)
}

/// Many runs (a commit's each, a policy that merges no window): the statement asserted in 8 new
/// graphs a commit, 7 of the last commit's copies deleted, and inferred in every other commit,
/// deleted between. So its point read passes many deleted copies in each stack, and in the
/// Materialised model meets the inferred stack after the asserted one runs out.
fn many_runs() -> (Engine, Terms) {
    let engine = engine(0);
    let terms = terms(&engine, 8 * 25);
    let graphs: Vec<_> = terms.graphs.chunks(8).collect();
    for (commit, ours) in graphs.iter().enumerate() {
        let mut tx = engine.transaction();
        for &graph in *ours {
            tx.insert_encoded(quad(terms.point, graph));
        }
        if let Some(last) = commit.checked_sub(1) {
            for &graph in &graphs[last][1..] {
                tx.remove_encoded(quad(terms.point, graph));
            }
        }
        let [s, p, o] = terms.point;
        match commit % 2 {
            0 => tx.insert_inferred(EncodedTriple::new(s, p, o)),
            _ => tx.remove_inferred(EncodedTriple::new(s, p, o)),
        };
        tx.commit().unwrap();
    }
    (engine, terms)
}

/// One run: the statement asserted in 128 graphs, one block of keys, and inferred.
fn one_block() -> (Engine, Terms) {
    let engine = engine(0);
    let terms = terms(&engine, 128);
    let mut tx = engine.transaction();
    for &graph in &terms.graphs {
        tx.insert_encoded(quad(terms.point, graph));
    }
    let [s, p, o] = terms.point;
    tx.insert_inferred(EncodedTriple::new(s, p, o));
    tx.commit().unwrap();
    (engine, terms)
}

/// More runs than making a point read's scans fits in one visit: 200 commits, the statement
/// asserted in a new graph each.
fn over_the_cap() -> (Engine, Terms) {
    let engine = engine(0);
    let terms = terms(&engine, 200);
    for &graph in &terms.graphs {
        let mut tx = engine.transaction();
        tx.insert_encoded(quad(terms.point, graph));
        tx.commit().unwrap();
    }
    (engine, terms)
}

/// The statement in the default graph and one named graph, its subject the same as an alias:
/// equality applies to its default-graph reads in the models with inferred statements.
fn with_equality() -> (Engine, Terms, TermId) {
    let engine = engine(4);
    let terms = terms(&engine, 1);
    let tx = engine.transaction();
    let same_as = tx.intern(node("sameAs").as_ref().into());
    let alias = tx.intern(node("alias").as_ref().into());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    let [s, _, _] = terms.point;
    tx.insert_encoded(quad(terms.point, TermId::DEFAULT_GRAPH));
    tx.insert_encoded(quad(terms.point, terms.graphs[0]));
    tx.insert_encoded(EncodedQuad::new(s, same_as, alias, TermId::DEFAULT_GRAPH));
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    (engine, terms, alias)
}

/// Its point read of `point` in `model`, refused at every visit in turn ([`refusal_sweep`]),
/// answers as its pattern read does, on its own path or not as `own` says. Returns the answer.
fn sweep_case(
    counted: &CountedSnapshot,
    case: &str,
    model: ReadModel,
    point: [TermId; 3],
    own: bool,
) -> Vec<EncodedQuad> {
    let case = format!("{case}, {model:?}, {point:?}");
    let reads = witness::get(Site::PointReads);
    let answer = refusal_sweep(&case, |visit| point_answers(counted, model, point, visit));
    let runs = witness::get(Site::PointReads) - reads;
    let [s, p, o] = point.map(Some);
    let pattern = QuadPattern {
        subject: s,
        predicate: p,
        object: o,
        graph: GraphSelector::Any,
    };
    let read: Vec<_> = counted
        .quads(model, &pattern, &|_| true)
        .map(|quad| quad.expect("never refused"))
        .collect();
    assert_eq!(answer, read, "{case}: as the pattern read answers");
    match own {
        true => assert!(runs > 0, "{case}: its own path"),
        false => assert_eq!(runs, 0, "{case}: the expanded read's path"),
    }
    answer
}

/// Test 2 (r3, E1). Spec ([`crate::visit`]: work is counted before it is done, after a yes at
/// most the units it allowed follow, after a no none; the point read: every refusal answers
/// [`Stopped`]): a point read, refused at each of its visits in turn, does no witnessed work a
/// visit did not allow, its scans' making and its last steps included, and says it was stopped.
/// On many runs with the statement deleted in many graphs, and on one block holding it in 128
/// graphs: the statement and a missing one (a read stopped before it starts is the sweep's first
/// case), in each model. Over the cap of one visit's units it takes its own path too (D16-574:
/// each run is counted before it is searched, so nothing falls back); where equality applies, the
/// expanded path, swept alike.
#[test]
fn a_point_read_does_no_work_its_visits_did_not_allow() {
    for (state, (engine, terms)) in [("many runs", many_runs()), ("one block", one_block())] {
        let counted = engine.snapshot().counted();
        for model in MODELS {
            sweep_case(&counted, state, model, terms.point, true);
            sweep_case(&counted, state, model, terms.miss, true);
        }
    }
    let (engine, terms) = over_the_cap();
    let counted = engine.snapshot().counted();
    for model in [ReadModel::Materialised, ReadModel::Asserted] {
        sweep_case(&counted, "over the cap", model, terms.point, true);
    }
    let (engine, terms, _) = with_equality();
    let counted = engine.snapshot().counted();
    sweep_case(
        &counted,
        "equality",
        ReadModel::Materialised,
        terms.point,
        false,
    );
    sweep_case(&counted, "equality", ReadModel::Asserted, terms.point, true);
}

/// Test 3. Spec (the point read: the expanded read where the read expands equality classes,
/// Asserted reads never do): with a `sameAs` class over the statement's subject, read plainly
/// and canonically, Asserted first and Materialised first (the first read in a model with
/// inferred statements builds the classes), a point read of the statement and of its alias's
/// copy takes the expanded path in the Materialised model (equality copies made) and its own
/// path in the Asserted model (none made), and hands on the graphs of the pattern read's quads.
#[test]
fn a_point_read_expands_equality_in_either_first_read_order() {
    for canonical in [false, true] {
        for order in [
            [ReadModel::Asserted, ReadModel::Materialised],
            [ReadModel::Materialised, ReadModel::Asserted],
        ] {
            let (engine, terms, alias) = with_equality();
            let snapshot = match canonical {
                true => engine.snapshot().with_canonical_equality(),
                false => engine.snapshot(),
            };
            let counted = snapshot.counted();
            let [s, p, o] = terms.point;
            for model in order {
                for point in [[s, p, o], [alias, p, o]] {
                    let case = format!("canonical {canonical}, {order:?}, {model:?}, {point:?}");
                    let (reads, copies) = (
                        witness::get(Site::PointReads),
                        witness::get(Site::ExpandCopies),
                    );
                    let mut graphs = Vec::new();
                    let answer = counted.point_graphs(model, point, &|_| true, |graph| {
                        graphs.push(graph);
                        Ok(ControlFlow::Continue(()))
                    });
                    let own = witness::get(Site::PointReads) - reads;
                    let copied = witness::get(Site::ExpandCopies) - copies;
                    assert_eq!(
                        answer,
                        Ok(ControlFlow::Continue(())),
                        "{case}: read to its end"
                    );
                    let [s, p, o] = point.map(Some);
                    let pattern = QuadPattern {
                        subject: s,
                        predicate: p,
                        object: o,
                        graph: GraphSelector::Any,
                    };
                    let read: Vec<_> = counted
                        .quads(model, &pattern, &|_| true)
                        .map(|quad| quad.expect("never refused").graph)
                        .collect();
                    assert_eq!(graphs, read, "{case}: the pattern read's graphs");
                    match model {
                        ReadModel::Asserted => {
                            assert_eq!((own, copied), (1, 0), "{case}: its own path, no copies");
                        }
                        _ => {
                            assert_eq!(own, 0, "{case}: the expanded path");
                            assert!(copied > 0, "{case}: equality copies made");
                        }
                    }
                }
            }
        }
    }
}

/// The layouts of `snapshot`'s asserted and inferred stacks.
fn layouts(snapshot: &Snapshot) -> [Layout; 2] {
    let data = snapshot.data(ReadModel::Materialised);
    [data.asserted.layout(), data.inferred.layout()]
}

/// The graphs of `quads`, in order.
fn graphs_of(quads: &[EncodedQuad]) -> Vec<TermId> {
    quads.iter().map(|quad| quad.graph).collect()
}

/// A commit's run each (a policy that merges no window): the statement asserted in the default
/// graph, deleted, then asserted again. The snapshots after the deletion and after the
/// re-insert.
fn deleted_and_reinserted() -> (Engine, Terms, [Snapshot; 2]) {
    let engine = engine(0);
    let terms = terms(&engine, 0);
    let copy = quad(terms.point, TermId::DEFAULT_GRAPH);
    let commit = |insert: bool| {
        let mut tx = engine.transaction();
        match insert {
            true => assert!(tx.insert_encoded(copy)),
            false => assert!(tx.remove_encoded(copy)),
        }
        tx.commit().unwrap();
        engine.snapshot()
    };
    commit(true);
    let deleted = commit(false);
    let reinserted = commit(true);
    (engine, terms, [deleted, reinserted])
}

/// The default graph's sum (D16-574 r3: a default-graph stack sums its runs' signs for the
/// statement in one number, each run holding at most one key of it). Spec (`index::merge`: a
/// quad is visible where its signs over the runs sum to 1): the statement asserted, deleted in a
/// run of its own and asserted again, all in the default graph, is answered after the re-insert
/// (signs 1, -1, 1) and not after the deletion (1, -1), in the models that read the asserted
/// stack; the read swept as test 2 sweeps it.
#[test]
fn a_default_graph_read_sums_a_statements_signs_over_its_runs() {
    let (_engine, terms, [deleted, reinserted]) = deleted_and_reinserted();
    for (snapshot, runs, wanted) in [
        (&deleted, 2, vec![]),
        (&reinserted, 3, vec![TermId::DEFAULT_GRAPH]),
    ] {
        assert_eq!(
            layouts(snapshot)[0],
            Layout::DefaultGraph,
            "a default-graph stack"
        );
        let held = snapshot.data(ReadModel::Asserted).asserted.runs().len();
        assert!(held >= runs, "a run per commit: {held}");
        let counted = snapshot.counted();
        for model in [ReadModel::Materialised, ReadModel::Asserted] {
            let answer = sweep_case(&counted, "re-inserted", model, terms.point, true);
            assert_eq!(
                graphs_of(&answer),
                wanted,
                "{model:?}, after {runs} commits"
            );
        }
    }
}

/// Each graph's sum in a quads stack. Spec (`index::merge`: each quad's signs over the runs sum
/// to 1 where it is visible, to 0 where not): the statement asserted in three graphs, then
/// deleted in two of them, then asserted again in one of those, a commit's run each, is answered
/// in the graph never deleted and the one re-inserted (signs 1 and 1, -1, 1), not in the one left
/// deleted (1, -1); the read swept as test 2 sweeps it.
#[test]
fn a_quads_read_sums_each_graphs_signs_over_its_runs() {
    let engine = engine(0);
    let terms = terms(&engine, 3);
    let [g0, g1, g2] = [0, 1, 2].map(|g| terms.graphs[g]);
    for (inserts, removes) in [
        (vec![g0, g1, g2], vec![]),
        (vec![], vec![g1, g2]),
        (vec![g1], vec![]),
    ] {
        let mut tx = engine.transaction();
        for graph in inserts {
            assert!(tx.insert_encoded(quad(terms.point, graph)));
        }
        for graph in removes {
            assert!(tx.remove_encoded(quad(terms.point, graph)));
        }
        tx.commit().unwrap();
    }
    let snapshot = engine.snapshot();
    assert_eq!(layouts(&snapshot)[0], Layout::Quads, "a quads stack");
    let counted = snapshot.counted();
    let mut wanted = vec![g0, g1];
    wanted.sort();
    for model in [ReadModel::Materialised, ReadModel::Asserted] {
        let answer = sweep_case(&counted, "three graphs", model, terms.point, true);
        assert_eq!(graphs_of(&answer), wanted, "{model:?}");
    }
}

/// A quads asserted stack and a default-graph inferred stack, a commit's run each: the statement
/// asserted in two named graphs and inferred, then deleted in one graph and no longer inferred,
/// then inferred again.
fn mixed_layouts() -> (Engine, Terms) {
    let engine = engine(0);
    let terms = terms(&engine, 2);
    let [s, p, o] = terms.point;
    let triple = EncodedTriple::new(s, p, o);
    let mut tx = engine.transaction();
    for &graph in &terms.graphs {
        assert!(tx.insert_encoded(quad(terms.point, graph)));
    }
    assert!(tx.insert_inferred(triple));
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    assert!(tx.remove_encoded(quad(terms.point, terms.graphs[1])));
    assert!(tx.remove_inferred(triple));
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    assert!(tx.insert_inferred(triple));
    tx.commit().unwrap();
    (engine, terms)
}

/// Each stack by its own layout (D16-574 r3, M1: a Materialised read can span a quads asserted
/// stack and a default-graph inferred one). Spec (the point read answers what the pattern read
/// answers; every refusal answers [`Stopped`] after a prefix of the answer): over such stacks the
/// read answers the asserted stack's live graph, then the inferred default graph, in each model
/// as the pattern read does, swept as test 2 sweeps it; and a refusal in either stack's walk
/// stops it there, in the inferred stack's after the asserted graph was handed on.
#[test]
fn a_read_over_both_layouts_sums_each_stack_by_its_own() {
    let (engine, terms) = mixed_layouts();
    let snapshot = engine.snapshot();
    assert_eq!(layouts(&snapshot), [Layout::Quads, Layout::DefaultGraph]);
    let counted = snapshot.counted();
    let g0 = terms.graphs[0];
    for (model, wanted) in [
        (ReadModel::Materialised, vec![g0, TermId::DEFAULT_GRAPH]),
        (ReadModel::Asserted, vec![g0]),
        (ReadModel::Inferred, vec![TermId::DEFAULT_GRAPH]),
    ] {
        let answer = sweep_case(&counted, "both layouts", model, terms.point, true);
        assert_eq!(graphs_of(&answer), wanted, "{model:?}");
    }
    let mut refused = Vec::new();
    for yeses in 0.. {
        let asked = Cell::new(0);
        let answers = point_answers(&counted, ReadModel::Materialised, terms.point, &|_| {
            asked.set(asked.get() + 1);
            asked.get() <= yeses
        });
        let Some((Err(Stopped), delivered)) = answers.split_last() else {
            break;
        };
        let delivered: Vec<_> = delivered
            .iter()
            .map(|answer| answer.expect("quads before the refusal").graph)
            .collect();
        refused.push(delivered);
    }
    let walk = |answered: &[TermId]| refused.iter().any(|prefix| prefix == answered);
    assert!(walk(&[]), "a refusal in the asserted walk: {refused:?}");
    assert!(walk(&[g0]), "a refusal in the inferred walk: {refused:?}");
}

/// The statement asserted in the default graph and its missing object's statement inferred:
/// two default-graph stacks, each with a run.
fn default_graph_stacks() -> (Engine, Terms) {
    let engine = engine(0);
    let terms = terms(&engine, 0);
    let [s, p, o2] = terms.miss;
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(quad(terms.point, TermId::DEFAULT_GRAPH)));
    assert!(tx.insert_inferred(EncodedTriple::new(s, p, o2)));
    tx.commit().unwrap();
    let layouts = layouts(&engine.snapshot());
    assert_eq!(layouts, [Layout::DefaultGraph, Layout::DefaultGraph]);
    (engine, terms)
}

/// The states a caller ends a Materialised read in: a default-graph statement with a later
/// stack still to read, and one block holding the statement in 128 graphs, inferred too.
fn caller_end_states() -> [(&'static str, Engine, [TermId; 3]); 2] {
    let (default, terms) = default_graph_stacks();
    let (block, block_terms) = one_block();
    [
        ("default graph", default, terms.point),
        ("one block", block, block_terms.point),
    ]
}

/// A decisive graph ends the read (D16-574 r2, L2). Spec (the point read: on `Break` it answers
/// at once, with no further charge or search): a caller that breaks at the first graph gets
/// `Break`, though its visitor would refuse the next ask the read made going on (there is one),
/// and the read asks nothing and does no witnessed work after that graph.
#[test]
fn a_decisive_graph_answers_before_a_later_refusal() {
    for (state, engine, point) in caller_end_states() {
        let counted = engine.snapshot().counted();
        let model = ReadModel::Materialised;
        let asked = Cell::new(0u64);
        let mut first = None;
        let going = counted.point_graphs(
            model,
            point,
            &|_| {
                asked.set(asked.get() + 1);
                true
            },
            |_| {
                if first.is_none() {
                    first = Some(asked.get());
                }
                Ok(ControlFlow::Continue(()))
            },
        );
        assert_eq!(going, Ok(ControlFlow::Continue(())), "{state}");
        let (first, all) = (first.expect("a graph"), asked.get());
        assert!(all > first, "{state}: an ask after the first graph");
        asked.set(0);
        let handed = Cell::new(None);
        let answer = counted.point_graphs(
            model,
            point,
            &|_| {
                asked.set(asked.get() + 1);
                asked.get() <= first
            },
            |_| {
                handed.set(Some(read_work()));
                Ok(ControlFlow::Break(()))
            },
        );
        assert_eq!(
            answer,
            Ok(ControlFlow::Break(())),
            "{state}: the decisive graph's answer"
        );
        assert_eq!(
            asked.get(),
            first,
            "{state}: no ask after the decisive graph"
        );
        let handed = handed.get().expect("a graph handed on");
        assert_eq!(
            read_work(),
            handed,
            "{state}: no work after the decisive graph"
        );
    }
}

/// A refusing caller stops the read (D16-574 r2, L2). Spec (the point read: on `Err` it returns
/// the error at once): a caller refusing the first graph gets [`Stopped`], and the read asks
/// nothing and does no witnessed work after that graph.
#[test]
fn a_refusing_caller_stops_the_read_at_once() {
    for (state, engine, point) in caller_end_states() {
        let counted = engine.snapshot().counted();
        let asked = Cell::new(0u64);
        let handed = Cell::new(None);
        let answer = counted.point_graphs(
            ReadModel::Materialised,
            point,
            &|_| {
                asked.set(asked.get() + 1);
                true
            },
            |_| {
                handed.set(Some((asked.get(), read_work())));
                Err(Stopped)
            },
        );
        assert_eq!(answer, Err(Stopped), "{state}: the caller's refusal");
        let handed = handed.get().expect("a graph handed on");
        assert_eq!(
            (asked.get(), read_work()),
            handed,
            "{state}: nothing read after it"
        );
    }
}

/// No credit across a caller's work (D16-574 r3, charging: the read's remainder is dropped at
/// every graph it hands on, so no units one visit allowed reach past the caller's own asks).
/// Spec: after each graph handed on, the read does no witnessed work before it asks its visitor
/// again: not before its next ask, not before its next graph, not before its end. Over one
/// block's 128 graphs in a quads stack, many runs, and both layouts, in models that read more
/// than one graph.
#[test]
fn a_point_read_holds_no_credit_across_a_graph_it_hands_on() {
    let (block, block_terms) = one_block();
    let (runs, runs_terms) = many_runs();
    let (mixed, mixed_terms) = mixed_layouts();
    for (state, engine, point, models) in [
        ("one block", &block, block_terms.point, &MODELS[..2]),
        ("many runs", &runs, runs_terms.point, &MODELS[..2]),
        ("both layouts", &mixed, mixed_terms.point, &MODELS[..1]),
    ] {
        let counted = engine.snapshot().counted();
        for &model in models {
            let case = format!("{state}, {model:?}");
            // The work witnessed when the last graph was handed on, until the next ask.
            let handed = Cell::new(None);
            let unasked = |until: &str| {
                if let Some(at) = handed.take() {
                    let done = read_work() - at;
                    assert_eq!(
                        done, 0,
                        "{case}: work {until}, with no ask since the last graph"
                    );
                }
            };
            let mut graphs = 0;
            let answer = counted.point_graphs(
                model,
                point,
                &|_| {
                    unasked("before the next ask");
                    true
                },
                |_| {
                    unasked("before the next graph");
                    graphs += 1;
                    handed.set(Some(read_work()));
                    Ok(ControlFlow::Continue(()))
                },
            );
            unasked("before the read's end");
            assert_eq!(answer, Ok(ControlFlow::Continue(())), "{case}");
            assert!(graphs > 1, "{case}: more than one graph handed on");
        }
    }
}

/// Equality over a default-graph stack (D16-574 r3: the equality path is chosen once per read
/// and stays the expanded read). Spec ([`Snapshot::quads_for_pattern_in`]: in a model with
/// inferred statements an alias's copy of a stored statement is answered; the asserted model
/// reads as stored): with the statement and a `sameAs` to an alias of its subject in the default
/// graph alone, a point read of the alias's copy answers the default graph in the Materialised
/// model, on the expanded path, and nothing in the Asserted model, on its own; the statement
/// itself is answered in both. Each read swept as test 2 sweeps it.
#[test]
fn a_read_where_equality_applies_expands_over_a_default_graph_stack() {
    let engine = engine(4);
    let terms = terms(&engine, 0);
    let tx = engine.transaction();
    let same_as = tx.intern(node("sameAs").as_ref().into());
    let alias = tx.intern(node("alias").as_ref().into());
    tx.commit().unwrap();
    let [s, p, o] = terms.point;
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(quad(terms.point, TermId::DEFAULT_GRAPH)));
    assert!(tx.insert_encoded(EncodedQuad::new(s, same_as, alias, TermId::DEFAULT_GRAPH)));
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    assert_eq!(
        layouts(&snapshot)[0],
        Layout::DefaultGraph,
        "a default-graph stack"
    );
    let counted = snapshot.counted();
    let default = vec![TermId::DEFAULT_GRAPH];
    for (model, point, own, wanted) in [
        (
            ReadModel::Materialised,
            [alias, p, o],
            false,
            default.clone(),
        ),
        (ReadModel::Materialised, [s, p, o], false, default.clone()),
        (ReadModel::Asserted, [alias, p, o], true, vec![]),
        (ReadModel::Asserted, [s, p, o], true, default),
    ] {
        let answer = sweep_case(&counted, "default-graph equality", model, point, own);
        assert_eq!(graphs_of(&answer), wanted, "{model:?}, {point:?}");
    }
}

/// Spec ([`crate::visit`]: once its visitor has stopped a read answers [`Stopped`], never the
/// end of the data): a point read over an empty dataset, its visitor stopped, answers
/// [`Stopped`] in each model.
#[test]
fn a_point_read_over_nothing_tells_a_stopped_visitor() {
    let engine = engine(0);
    let terms = terms(&engine, 0);
    let counted = engine.snapshot().counted();
    for model in MODELS {
        let answer = point_answers(&counted, model, terms.point, &|_| false);
        assert_eq!(answer, [Err(Stopped)], "{model:?}");
    }
}

/// The default-graph loop checks its layout (D16-574 r3: a debug assertion of the layout's
/// invariant on each key it finds). Spec ([`Layout::DefaultGraph`]: such a run holds the
/// default graph alone, so at most one key with a statement's subject, predicate and object):
/// a quads run holding the statement in two graphs, forced through the default-graph loop,
/// fails that check.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "at most one key")]
fn the_default_graph_loop_checks_its_layout() {
    use crate::index::IndexVersion;
    use crate::quad::AccessPlan;
    use crate::visit::Ledger;

    let [s, p, o, g1, g2] = [1, 2, 3, 4, 5].map(TermId::from_raw);
    let quads = vec![quad([s, p, o], g1), quad([s, p, o], g2)];
    let index = IndexVersion::from_quads(Layout::Quads, quads);
    let plan = AccessPlan::for_pattern(&QuadPattern {
        subject: Some(s),
        predicate: Some(p),
        object: Some(o),
        graph: GraphSelector::Any,
    });
    let _ = index.point_sum(&plan, &mut Ledger::new(&|_| true));
}
