//! The join orderer's plans step by step: the order, and each step's estimated rows to the
//! bit, for planning problems of each shape its searches meet. Identity tests: the tables
//! were captured from the orderer before its state lifecycle change (engine-v2 38202f73d),
//! which keeps every comparison, so they hold on both trees by design. A change to the
//! cost model changes them on purpose; the failure prints the new table.

use super::plan::{self, Input};
use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// A plan's order, and each step's rows as `f64::to_bits`.
type Steps = (Vec<usize>, Vec<u64>);

type Table = [(&'static str, &'static [usize], &'static [u64])];

fn steps(plan: &plan::Plan) -> Steps {
    let rows = plan.rows.iter().map(|r| r.to_bits()).collect();
    (plan.order.clone(), rows)
}

/// `actual` is `expected`, case by case; otherwise the cases that differ, and the whole
/// actual table as source.
fn assert_table(expected: &Table, actual: &[(&str, Steps)]) {
    let found = |name: &str, (order, rows): &Steps| {
        expected
            .iter()
            .any(|e| e.0 == name && e.1 == order.as_slice() && e.2 == rows.as_slice())
    };
    let differ: Vec<&str> = actual
        .iter()
        .filter(|(name, steps)| !found(name, steps))
        .map(|(name, _)| *name)
        .collect();
    if differ.is_empty() && expected.len() == actual.len() {
        return;
    }
    let mut table = String::new();
    for (name, (order, rows)) in actual {
        let bits: Vec<String> = rows.iter().map(|r| format!("{r:#018x}")).collect();
        let values: Vec<f64> = rows.iter().map(|&r| f64::from_bits(r)).collect();
        table += &format!(
            "    // {values:?}\n    (\"{name}\", &{order:?}, &[{}]),\n",
            bits.join(", ")
        );
    }
    panic!("plans differ: {differ:?}\n--- actual ---\n{table}--- end ---");
}

fn input(count: u64, vars: &[(usize, u64)]) -> Input {
    Input {
        count,
        vars: vars.to_vec(),
        star: None,
        edge_object: None,
        overlaps: Vec::new(),
    }
}

/// `n` patterns: on even `i` a star pattern on variable 0 with a variable of its own, on
/// odd `i` an edge between variables `i` and `i + 1`; counts spread over 50..5,050.
fn branches(n: usize) -> Vec<Input> {
    (0..n)
        .map(|i| {
            let count = 50 + (i as u64 * 7_919) % 5_000;
            if i % 2 == 0 {
                input(
                    count,
                    &[(0, (count / 3).max(1)), (i + 1, (count / 2).max(1))],
                )
            } else {
                input(
                    count,
                    &[(i, (count / 5).max(1)), (i + 1, (count / 4).max(1))],
                )
            }
        })
        .collect()
}

#[rustfmt::skip]
const HAND_BUILT: &Table = &[
    // [7.0]
    ("one", &[0], &[0x401c000000000000]),
    // [10.0, 12.5]
    ("two", &[1, 0], &[0x4024000000000000, 0x4029000000000000]),
    // [10.0, 12.5]
    ("two-fixed", &[1, 0], &[0x4024000000000000, 0x4029000000000000]),
    // [1000.0, 100000.0, 555.5555555555555, 2.2222222222222223]
    ("fixed-chain", &[2, 0, 3, 1], &[0x408f400000000000, 0x40f86a0000000000, 0x40815c71c71c71c7, 0x4001c71c71c71c72]),
    // [50.0, 50.0, 50.0, 60.0, 180.0]
    ("star", &[0, 2, 4, 3, 1], &[0x4049000000000000, 0x4049000000000000, 0x4049000000000000, 0x404e000000000000, 0x4066800000000000]),
    // [9.0, 9.183673469387756, 1.3775510204081634, 1.4835164835164836, 1.0, 33.333333333333336]
    ("chain", &[5, 4, 3, 2, 1, 0], &[0x4022000000000000, 0x40225e0a72f05398, 0x3ff60a72f0539783, 0x3ff7bc7bc7bc7bc8, 0x3ff0000000000000, 0x4040aaaaaaaaaaab]),
    // [60.0, 180.0, 1200.0, 13.333333333333334]
    ("cycle", &[1, 2, 0, 3], &[0x404e000000000000, 0x4066800000000000, 0x4092c00000000000, 0x402aaaaaaaaaaaab]),
    // [7.0, 7.0, 21.0, 42.0]
    ("cross-product", &[3, 2, 1, 0], &[0x401c000000000000, 0x401c000000000000, 0x4035000000000000, 0x4045000000000000]),
    // [0.0, 0.0, 0.0]
    ("empty", &[1, 0, 2], &[0x0000000000000000, 0x0000000000000000, 0x0000000000000000]),
    // [100.0, 100.0, 100.0]
    ("tie-same-start", &[0, 1, 2], &[0x4059000000000000, 0x4059000000000000, 0x4059000000000000]),
    // [10.0, 200.0]
    ("tie-different-start", &[1, 0], &[0x4024000000000000, 0x4069000000000000]),
    // [189.0, 15.0, 75.0]
    ("overlaps", &[0, 1, 2], &[0x4067a00000000000, 0x402e000000000000, 0x4052c00000000000]),
    // [50.0, 150.11709601873537, 56.557377049180324, 169.67213114754097, 131.7677161939457, 395.3964024503395, 402.6702041935297, 1208.010612580589, 3626.1327257636462, 18130.663628818234, 90700.96771998818, 454116.6495120488]
    ("exhaustive-12", &[0, 6, 7, 8, 9, 10, 11, 2, 4, 5, 3, 1], &[0x4049000000000000, 0x4062c3bf40265eba, 0x404c47582192e29f, 0x40653582192e29f7, 0x40607891218d3367, 0x4078b657aa188433, 0x40792ab928084da2, 0x4092e00ade063a39, 0x40ac5443f4a19c65, 0x40d1b4aa78e501c0, 0x40f624cf7bc7f44f, 0x411bb7929919afc0]),
    // [2564.0, 150.11709601873537, 450.3512880562061, 169.672131147541, 509.016393442623, 1527.9344262295083, 4583.803278688524, 3559.7907884465258, 10681.891679414912, 10878.398177290941, 54465.3696262678, 272469.9897072293, 1362349.9485361467]
    ("greedy-13", &[6, 0, 12, 7, 2, 4, 8, 9, 10, 11, 1, 3, 5], &[0x40a4080000000000, 0x4062c3bf40265eba, 0x407c259ee0398e17, 0x40653582192e29f8, 0x407fd04325c53ef4, 0x4097dfbcda3ac10d, 0x40b1e7cda3ac10c9, 0x40abcf94e23927c4, 0x40c4dcf2228d12c8, 0x40c53f32f779354d, 0x40ea982bd3fa777e, 0x4110a157f575cfda, 0x4134c9adf2d343d1]),
];

/// Spec: the q02 planner design (12 Nov 2026) as the lead reconciled it: the orderer keeps
/// every comparison, in the same order, on the same values, so each planning problem gets
/// the same order and the same rows per step, the last step's included. Stars, a chain, a
/// cycle, a cross product, an empty pattern, equal-cost ties with equal and with different
/// starting rows, counted common values, one and two inputs, a fixed order, and 12
/// (exhaustive) and 13 (greedy) inputs.
#[test]
fn hand_built_inputs_plan_as_before() {
    let mut overlaps = vec![
        input(189, &[(0, 189)]),
        input(239, &[(0, 239)]),
        input(5_000, &[(0, 1_000), (1, 300)]),
    ];
    overlaps[0].overlaps = vec![(1, 15)];
    overlaps[1].overlaps = vec![(0, 15)];
    let two = || vec![input(50, &[(0, 50), (1, 40)]), input(10, &[(1, 10)])];
    // Name, inputs, variables, and the order for `plan::along` (none: the search's).
    type Case = (&'static str, Vec<Input>, usize, Option<Vec<usize>>);
    let cases: Vec<Case> = vec![
        ("one", vec![input(7, &[(0, 7)])], 1, None),
        ("two", two(), 2, None),
        ("two-fixed", two(), 2, Some(vec![1, 0])),
        (
            "fixed-chain",
            vec![
                input(100, &[(0, 100), (1, 50)]),
                input(40, &[(1, 40), (2, 40)]),
                input(1_000, &[(2, 200), (3, 900)]),
                input(5, &[(3, 5)]),
            ],
            4,
            Some(vec![2, 0, 3, 1]),
        ),
        (
            "star",
            vec![
                input(50, &[(0, 50)]),
                input(300_000, &[(0, 100_000), (1, 2_000)]),
                input(5_000, &[(0, 5_000)]),
                input(1_200, &[(0, 1_000), (2, 30)]),
                input(80, &[(0, 80), (3, 80)]),
            ],
            4,
            None,
        ),
        (
            "chain",
            vec![
                input(1_000, &[(0, 900), (1, 30)]),
                input(20, &[(1, 20), (2, 4)]),
                input(7_000, &[(2, 7_000), (3, 6_500)]),
                input(300, &[(3, 300), (4, 100)]),
                input(50_000, &[(4, 2_000), (5, 49_000)]),
                input(9, &[(5, 9), (6, 9)]),
            ],
            7,
            None,
        ),
        (
            "cycle",
            vec![
                input(400, &[(0, 400), (1, 20)]),
                input(60, &[(1, 60), (2, 60)]),
                input(9_000, &[(2, 3_000), (3, 90)]),
                input(700, &[(3, 70), (0, 700)]),
            ],
            4,
            None,
        ),
        (
            "cross-product",
            vec![
                input(10, &[(0, 10), (1, 5)]),
                input(3, &[(1, 3)]),
                input(40, &[(2, 40)]),
                input(7, &[(2, 7), (3, 7)]),
            ],
            4,
            None,
        ),
        (
            "empty",
            vec![
                input(100, &[(0, 100), (1, 20)]),
                input(0, &[(1, 0), (2, 0)]),
                input(30, &[(2, 30)]),
            ],
            3,
            None,
        ),
        (
            "tie-same-start",
            vec![
                input(100, &[(0, 10)]),
                input(100, &[(0, 10)]),
                input(100, &[(0, 10)]),
            ],
            1,
            None,
        ),
        (
            "tie-different-start",
            vec![input(20, &[(0, 20)]), input(10, &[(1, 10)])],
            2,
            None,
        ),
        ("overlaps", overlaps, 2, None),
        ("exhaustive-12", branches(12), 13, None),
        ("greedy-13", branches(13), 14, None),
    ];
    let actual: Vec<(&str, Steps)> = cases
        .iter()
        .map(|(name, inputs, variables, fixed)| {
            let plan = match fixed {
                Some(order) => plan::along(inputs, *variables, PROBE_FACTOR, None, order),
                None => plan::order(inputs, *variables, PROBE_FACTOR, None, &|| false).unwrap(),
            };
            (*name, steps(&plan))
        })
        .collect();
    assert_table(HAND_BUILT, &actual);
}

/// People `n0`..`n59` who know one or two others, with labels, types, some cities and
/// ages, and up to 13 predicates `p0`..`p12` each; five cities, three with a country; and
/// `a anchor n0`. All IRIs `http://e/…`, in the default graph.
fn people() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let e = |name: &str| NamedNode::new_unchecked(format!("http://e/{name}"));
    let mut tx = engine.transaction();
    let mut add = |s: &str, p: &str, o: &str| {
        tx.insert(Quad::new(e(s), e(p), e(o), GraphName::DefaultGraph).as_ref());
    };
    add("a", "anchor", "n0");
    let n = |k: u32| format!("n{}", k % 60);
    for i in 0..60u32 {
        let s = &n(i);
        add(s, "knows", &n(i * 7 + 1));
        if i % 2 == 0 {
            add(s, "knows", &n(i * 3 + 2));
        }
        add(s, "label", &format!("l{i}"));
        add(s, "type", "Person");
        if i % 4 == 0 {
            add(s, "type", "Student");
        }
        if i % 3 != 0 {
            add(s, "city", &format!("c{}", i % 5));
        }
        if i % 2 == 1 {
            add(s, "age", &format!("y{}", i % 7));
        }
        for p in 0..13 {
            if (i * 7 + p * 3) % 5 < 3 {
                add(s, &format!("p{p}"), &format!("o{}", i % 17));
            }
        }
    }
    for c in 0..5 {
        add(&format!("c{c}"), "name", &format!("m{c}"));
        if c % 2 == 0 {
            add(&format!("c{c}"), "country", &format!("k{}", c % 2));
        }
    }
    tx.commit().unwrap();
    engine
}

/// `text`, `s p o` with `?name` for a variable and `name` for `<http://e/name>`, as a
/// default-graph pattern of `snapshot`.
fn scan(snapshot: &Snapshot, text: &str) -> ScanPattern {
    let slot = |term: &str| match term.strip_prefix('?') {
        Some(name) => Slot::Var(Variable::new_unchecked(name)),
        None => {
            let iri = NamedNode::new_unchecked(format!("http://e/{term}"));
            Slot::Const(snapshot.lookup(iri.as_ref().into()).expect(term))
        }
    };
    let [s, p, o] = text.split_whitespace().collect::<Vec<_>>()[..] else {
        panic!("{text}");
    };
    ScanPattern {
        slots: [
            slot(s),
            slot(p),
            slot(o),
            Slot::Const(TermId::DEFAULT_GRAPH),
        ],
    }
}

/// A BGP's links: per variable, the variables its other side reads.
type Links = [(Variable, Vec<Variable>)];

/// The plan of `patterns` as a BGP's (with `links`) by `ctx`, with EXPLAIN's estimates.
fn planned(ctx: &Context<'_>, patterns: &[&str], links: &Links) -> Steps {
    let scans: Vec<ScanPattern> = patterns.iter().map(|p| scan(&ctx.snapshot, p)).collect();
    let counts: Vec<u64> = scans
        .iter()
        .map(|s| ctx.snapshot.estimate_in(ctx.model, &s.quad_pattern()))
        .collect();
    let plan = ctx.join_order(&scans, &counts, links, true);
    steps(&plan.map_err(QueryEvaluationError::from).unwrap())
}

#[rustfmt::skip]
const FROM_THE_STORE: &Table = &[
    // [1.0, 1.5, 1.5]
    ("q02", &[0, 1, 2], &[0x3ff0000000000000, 0x3ff8000000000000, 0x3ff8000000000000]),
    // [40.0]
    ("one", &[0], &[0x4044000000000000]),
    // [40.0, 70.0]
    ("two", &[1, 0], &[0x4044000000000000, 0x4051800000000000]),
    // [30.0, 20.0, 20.0, 20.0]
    ("star", &[3, 2, 0, 1], &[0x403e000000000000, 0x4034000000000000, 0x4034000000000000, 0x4034000000000000]),
    // [3.0, 24.0, 42.0, 70.0]
    ("chain", &[3, 2, 1, 0], &[0x4008000000000000, 0x4038000000000000, 0x4045000000000000, 0x4051800000000000]),
    // [5.0, 40.0, 40.0]
    ("edge-after-star", &[0, 1, 2], &[0x4014000000000000, 0x4044000000000000, 0x4044000000000000]),
    // [8.0, 2.0, 3.0]
    ("common-values", &[1, 0, 2], &[0x4020000000000000, 0x4000000000000000, 0x4008000000000000]),
    // [90.0, 150.0, 4.166666666666667]
    ("cycle", &[0, 1, 2], &[0x4056800000000000, 0x4062c00000000000, 0x4010aaaaaaaaaaab]),
    // [15.0, 22.5, 22.5]
    ("constant-objects", &[0, 1, 2], &[0x402e000000000000, 0x4036800000000000, 0x4036800000000000]),
    // [40.0, 40.0, 40.0]
    ("link", &[2, 0, 1], &[0x4044000000000000, 0x4044000000000000, 0x4044000000000000]),
    // [36.0, 12.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0]
    ("star-12", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], &[0x4042000000000000, 0x4028000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000]),
    // [36.0, 12.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0, -0.0]
    ("star-13", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], &[0x4042000000000000, 0x4028000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000, 0x8000000000000000]),
    // [15.0, 22.5, 1662.375]
    ("path", &[0, 1, 2], &[0x402e000000000000, 0x4036800000000000, 0x4099f98000000000]),
];

/// Spec: as for [`hand_built_inputs_plan_as_before`], through the planner's inputs from a
/// store (counts, distinct values, stars, edges, counted common values, a link's
/// variable) and the store's characteristic sets and pairs: q02's shape, one and two
/// patterns, a star, a chain, an edge after a star on its object, two small patterns on
/// one variable, a cycle (EXPLAIN plans it), constant objects, stars of 12 (exhaustive)
/// and 13 (greedy) patterns, and two patterns with a path after them, the path the last
/// input: its estimate needs what the second pattern binds.
#[test]
fn store_patterns_plan_as_before() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    // What the cases are for: the store has characteristic pairs, and two of its small
    // patterns on one variable get their common values counted.
    let sets = snapshot.characteristic_sets_in(ctx.model);
    assert!(
        sets.is_some_and(|s| s.has_pairs()),
        "no characteristic pairs"
    );
    let small = [
        scan(&snapshot, "?x type Student"),
        scan(&snapshot, "?x city c1"),
    ];
    let counts: Vec<u64> = small
        .iter()
        .map(|s| snapshot.estimate_in(ctx.model, &s.quad_pattern()))
        .collect();
    let inputs = ctx.plan_inputs(&small, &counts, &[], &mut Vec::new());
    assert!(
        inputs.iter().all(|i| !i.overlaps.is_empty()),
        "no common values"
    );
    let star = |n: usize| -> Vec<String> { (0..n).map(|i| format!("?x p{i} ?o{i}")).collect() };
    let (star12, star13) = (star(12), star(13));
    let var = Variable::new_unchecked;
    let none: &Links = &[];
    let link: &Links = &[(var("t"), vec![var("x"), var("y")])];
    let cases: Vec<(&str, Vec<&str>, &Links)> = vec![
        (
            "q02",
            vec!["a anchor ?b", "?b knows ?c", "?c label ?l"],
            none,
        ),
        ("one", vec!["?x city ?c"], none),
        ("two", vec!["?x knows ?y", "?y city ?c"], none),
        (
            "star",
            vec!["?x type Person", "?x knows ?y", "?x city ?c", "?x age ?a"],
            none,
        ),
        (
            "chain",
            vec!["?x knows ?y", "?y knows ?z", "?z city ?c", "?c country ?k"],
            none,
        ),
        (
            "edge-after-star",
            vec!["?c name ?m", "?x city ?c", "?x label ?l"],
            none,
        ),
        (
            "common-values",
            vec!["?x type Student", "?x city c1", "?x knows ?y"],
            none,
        ),
        (
            "cycle",
            vec!["?x knows ?y", "?y knows ?z", "?z knows ?x"],
            none,
        ),
        (
            "constant-objects",
            vec!["?x type Student", "?x knows ?y", "?y type Person"],
            none,
        ),
        (
            "link",
            vec!["?x knows ?y", "?y label ?l", "?t city ?c"],
            link,
        ),
        ("star-12", star12.iter().map(String::as_str).collect(), none),
        ("star-13", star13.iter().map(String::as_str).collect(), none),
    ];
    let mut actual: Vec<(&str, Steps)> = cases
        .iter()
        .map(|(name, patterns, links)| (*name, planned(&ctx, patterns, links)))
        .collect();
    // Two triple patterns, then a path from the second one's object: ordered as one join,
    // the path an input after the patterns' ([`Context::path_join_order`]).
    let text = "SELECT * WHERE { ?x <http://e/type> <http://e/Student> . \
                ?x <http://e/knows> ?y . ?y <http://e/knows>+ ?z }";
    let query = Query::parse(text, None).unwrap();
    let join = path_join(pattern_and_form(&query).0).expect("triple patterns and a path");
    let plan = ctx.path_join_order(&join);
    let plan = plan.map_err(QueryEvaluationError::from).unwrap();
    actual.push(("path", steps(&plan.expect("its terms are in the store"))));
    assert_table(FROM_THE_STORE, &actual);
}

/// The join of triple patterns and paths that `pattern` is, under its projection.
fn path_join(pattern: &GraphPattern) -> Option<super::path_joins::PathJoin<'_>> {
    match pattern {
        GraphPattern::Project { inner, .. } => path_join(inner),
        other => super::path_joins::PathJoin::of(other),
    }
}

/// Spec: `Context::distinct`: a variable's distinct values from the statistics where an
/// index order puts it right after the pattern's constants, else the pattern's matches,
/// as the caller counts them. From the statistics: a predicate variable beside others
/// (the store's 21 predicates). The matches: a variable alone, a variable at two
/// positions beside another, a pattern in the named graphs. Green before and after the
/// allocation cleanup of `distinct` by design.
#[test]
fn distinct_values_come_from_the_statistics_where_an_order_fits() {
    let engine = people();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let ctx = Context::new(&snapshot, &options, None, None);
    let var = Variable::new_unchecked;
    let mut named = scan(&snapshot, "?x knows ?y");
    named.slots[3] = Slot::Var(var("g"));
    for (what, pattern, variable, expected) in [
        ("a predicate variable", scan(&snapshot, "?x ?p ?o"), "p", 21),
        (
            "a variable alone",
            scan(&snapshot, "?x type Person"),
            "x",
            1_234,
        ),
        ("at two positions", scan(&snapshot, "?x ?p ?x"), "x", 1_234),
        ("in the named graphs", named, "x", 1_234),
    ] {
        let distinct = ctx.distinct(&pattern, &var(variable), 1_234);
        assert_eq!(distinct, expected, "{what}");
    }
}

/// Spec: the q02 design review (12 Nov 2026, BLOCK 1) and the lead's reconciliation:
/// planning looks up the distinct values of every pattern's variables, also of one that
/// no other pattern has, whose value no estimate reads: a lookup fills the engine's
/// statistics cache, which later revisions read while a pattern's matches drift by at most
/// a quarter. Here `?o`'s values are looked up on one revision and read on the next. There
/// is no seam that records the lookups and their order; this observes the lookup only.
#[test]
fn planning_looks_up_a_variable_only_one_pattern_has() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let e = |name: &str| NamedNode::new_unchecked(format!("http://e/{name}"));
    let quad = |s: &str, p: &str, o: &str| Quad::new(e(s), e(p), e(o), GraphName::DefaultGraph);
    let mut tx = engine.transaction();
    tx.insert(quad("a", "anchor", "s0").as_ref());
    for i in 0..20 {
        let s = format!("s{i}");
        tx.insert(quad(&s, "p", &format!("o{}", i % 2)).as_ref());
        tx.insert(quad(&s, "t", "k").as_ref());
    }
    tx.commit().unwrap();
    let options = QueryOptions::default();
    {
        // `?s p ?o`: 20 matches, 2 values of `?o`.
        let snapshot = engine.snapshot();
        let ctx = Context::new(&snapshot, &options, None, None);
        planned(&ctx, &["a anchor ?s", "?s p ?o", "?s t k"], &[]);
    }
    // The same 20 matches, now with 20 values of `?o`.
    let mut tx = engine.transaction();
    for i in 0..20 {
        let s = format!("s{i}");
        tx.remove(quad(&s, "p", &format!("o{}", i % 2)).as_ref());
        tx.insert(quad(&s, "p", &format!("q{i}")).as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let ctx = Context::new(&snapshot, &options, None, None);
    let pattern = scan(&snapshot, "?s p ?o");
    assert_eq!(
        ctx.distinct(&pattern, &Variable::new_unchecked("o"), 20),
        2,
        "the first revision's lookup is what the second reads (20 if it made none)"
    );
}
