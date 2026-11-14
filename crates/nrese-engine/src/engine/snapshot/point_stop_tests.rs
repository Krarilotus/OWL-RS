//! A point read's stops and paths (D16-543, design note `2026-11-14-d16-543-point-reads-design.md`
//! r3, tests 2 and 3): [`CountedSnapshot::point_quads`] does no work its visits did not allow, on
//! each of its paths, and takes the expanded path where equality applies and its own elsewhere.

use std::cell::Cell;

use nrese_rdf::NamedNode;

use crate::quad::{GraphSelector, QuadPattern};
use crate::visit::{Stopped, Visitor};
use crate::witness::{self, Site};
use crate::{
    CompactionPolicy, CountedSnapshot, EncodedQuad, EncodedTriple, Engine, EngineConfig, ReadModel,
    TermId,
};

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
/// answers as its pattern read does, on its own path or not as `own` says.
fn sweep_case(
    counted: &CountedSnapshot,
    case: &str,
    model: ReadModel,
    point: [TermId; 3],
    own: bool,
) {
    let case = format!("{case}, {model:?}, {point:?}");
    let reads = witness::get(Site::PointReads);
    let answer = refusal_sweep(&case, |visit| {
        counted.point_quads(model, point, visit).collect()
    });
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
        false => assert_eq!(runs, 0, "{case}: the counted read's path"),
    }
}

/// Test 2 (r3, E1). Spec ([`crate::visit`]: work is counted before it is done, after a yes at
/// most the units it allowed follow, after a no none; [`CountedSnapshot::point_quads`]: every
/// refusal answers [`Stopped`]): a point read, refused at each of its visits in turn, does no
/// witnessed work a visit did not allow, its scans' making and its last steps included, and
/// says it was stopped. On many runs with the statement deleted in many graphs, and on one block
/// holding it in 128 graphs: the statement and a missing one (a read stopped before it starts is
/// the sweep's first case), in each model. Over the cap, and where equality applies, the read
/// takes the counted read's path, swept alike.
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
        sweep_case(&counted, "over the cap", model, terms.point, false);
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

/// Test 3. Spec ([`CountedSnapshot::point_quads`]: the expanded read where the read expands
/// equality classes, Asserted reads never do): with a `sameAs` class over the statement's
/// subject, read plainly and canonically, Asserted first and Materialised first (the first read
/// in a model with inferred statements builds the classes), a point read of the statement and
/// of its alias's copy takes the expanded path in the Materialised model (equality copies made)
/// and its own path in the Asserted model (none made), and answers as the pattern read does.
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
                    let answer: Vec<_> = counted.point_quads(model, point, &|_| true).collect();
                    let own = witness::get(Site::PointReads) - reads;
                    let copied = witness::get(Site::ExpandCopies) - copies;
                    let [s, p, o] = point.map(Some);
                    let pattern = QuadPattern {
                        subject: s,
                        predicate: p,
                        object: o,
                        graph: GraphSelector::Any,
                    };
                    let read: Vec<_> = counted.quads(model, &pattern, &|_| true).collect();
                    assert_eq!(answer, read, "{case}: as the pattern read answers");
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
