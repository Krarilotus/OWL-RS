//! `COUNT` of join trees by message passing (`native::count`) against the reference
//! evaluator: random trees of patterns with OPTIONAL, MINUS, (NOT) EXISTS and UNION parts
//! hanging off one of their variables (and some that share none, two, or one only a part
//! binds, which the count must refuse), over random data whose objects are mostly
//! entities, so chains join; a third of it inferred, a little of it in a named graph, every
//! query under a random read model.

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

use crate::native_differential_tests::{EX, Rng, draws_on, ex};

fn object(rng: &mut Rng) -> Term {
    match rng.below(8) {
        0 => Literal::new_typed_literal(rng.below(4).to_string(), xsd::INTEGER).into(),
        1 => Literal::new_simple_literal(format!("s{}", rng.below(3))).into(),
        _ => ex(&format!("e{}", rng.below(8))).into(),
    }
}

fn dataset(rng: &mut Rng) -> Vec<Quad> {
    (0..20 + rng.below(80))
        .map(|_| {
            let graph = match rng.below(12) {
                0 => GraphName::NamedNode(ex("g")),
                _ => GraphName::DefaultGraph,
            };
            Quad::new(
                ex(&format!("e{}", rng.below(8))),
                ex(&format!("p{}", rng.below(4))),
                object(rng),
                graph,
            )
        })
        .collect()
}

/// A random tree: patterns from `?v0`, each joining a variable already bound to a new one
/// (now and then to a bound one or a constant: cycles, two patterns over a pair).
struct Tree {
    next: usize,
    bound: Vec<String>,
}

impl Tree {
    fn fresh(&mut self) -> String {
        self.next += 1;
        format!("?v{}", self.next - 1)
    }

    fn pattern(&mut self, rng: &mut Rng, from: &str) -> String {
        // Now and then a closure to a new variable, plain or after a step (`p1/p2+`), either
        // way round.
        if rng.below(24) == 0 {
            let path = match rng.below(2) {
                0 => format!("<{EX}p{}>+", rng.below(4)),
                _ => format!("<{EX}p{}>/<{EX}p{}>+", rng.below(4), rng.below(4)),
            };
            let end = self.fresh();
            return match rng.below(2) {
                0 => format!("{from} {path} {end} ."),
                _ => format!("{end} {path} {from} ."),
            };
        }
        let p = format!("<{EX}p{}>", rng.below(4));
        let others: Vec<String> = self.bound.iter().filter(|v| *v != from).cloned().collect();
        // A constant; a bound variable (two patterns over a pair, a cycle); the same
        // variable (refused); mostly a new one.
        let other = match rng.below(24) {
            0 | 1 => format!("<{EX}e{}>", rng.below(9)), // e8 is unknown
            2..=4 if !others.is_empty() => rng.pick(&others).clone(),
            5 => from.to_owned(),
            _ => {
                let v = self.fresh();
                self.bound.push(v.clone());
                v
            }
        };
        match rng.below(2) {
            0 => format!("{from} {p} {other} ."),
            _ => format!("{other} {p} {from} ."),
        }
    }

    /// One to three patterns from `from`, their new variables private to the group.
    fn group(&mut self, rng: &mut Rng, from: &str) -> String {
        let outer = self.bound.clone();
        self.bound = vec![from.to_owned()];
        let mut patterns = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let from = rng.pick(&self.bound).clone();
            patterns.push(self.pattern(rng, &from));
        }
        self.bound = outer;
        patterns.join(" ")
    }
}

fn count_query(rng: &mut Rng) -> String {
    let mut tree = Tree {
        next: 1,
        bound: vec!["?v0".to_owned()],
    };
    let mut body = Vec::new();
    for _ in 0..1 + rng.below(4) {
        let from = rng.pick(&tree.bound).clone();
        body.push(tree.pattern(rng, &from));
    }
    for _ in 0..rng.below(4) {
        // The variable the part hangs off: usually one of the tree's, else a fresh one
        // (it shares none) or two (refused).
        let at = match rng.below(8) {
            0 => tree.fresh(),
            _ => rng.pick(&tree.bound).clone(),
        };
        let mut group = tree.group(rng, &at);
        if rng.below(8) == 0 {
            let second = rng.pick(&tree.bound).clone();
            group.push_str(&format!(" {at} <{EX}p{}> {second} .", rng.below(4)));
        }
        body.push(match rng.below(5) {
            0 => format!("OPTIONAL {{ {group} }}"),
            1 => format!("MINUS {{ {group} }}"),
            2 => format!("FILTER EXISTS {{ {group} }}"),
            3 => format!("FILTER NOT EXISTS {{ {group} }}"),
            _ => {
                let other = tree.group(rng, &at);
                format!("{{ {group} }} UNION {{ {other} }}")
            }
        });
    }
    // COUNT(*), COUNT of a variable every solution binds, counts per value of one, and the
    // number of its values.
    let body = body.join(" ");
    let key = rng.pick(&tree.bound).clone();
    match rng.below(6) {
        0 => format!(
            "SELECT {key} (COUNT(*) AS ?n) (COUNT({key}) AS ?m) WHERE {{ {body} }} GROUP BY {key}"
        ),
        1 => format!("SELECT (COUNT(DISTINCT {key}) AS ?n) WHERE {{ {body} }}"),
        2 => format!("SELECT (COUNT(?v0) AS ?n) WHERE {{ {body} }}"),
        _ => format!("SELECT (COUNT(*) AS ?n) WHERE {{ {body} }}"),
    }
}

/// The rows, each as its bindings in variable order, sorted.
fn count(results: QueryResults<'_>) -> Vec<String> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("a SELECT")
    };
    let mut rows: Vec<String> = solutions
        .map(|s| {
            let s = s.unwrap();
            ["v0", "v1", "v2", "v3", "v4", "v5", "v6", "n", "m"]
                .iter()
                .filter_map(|v| s.get(*v).map(|t| format!("{v}={t}")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

/// A FILTER that bounds one variable: term-kind tests, comparisons with integers (some
/// past the inline integers on either side) and years, and their conjunctions.
fn bounding_filter(rng: &mut Rng, v: &str) -> String {
    let test = |rng: &mut Rng| -> String {
        match rng.below(6) {
            5 => format!("LANG({v}) = \"{}\"", rng.pick(&["en", "de"])),
            0 => format!("{}({v})", rng.pick(&["isLITERAL", "isIRI", "isBLANK"])),
            1 | 2 => format!(
                "{v} {} {}",
                rng.pick(&["<", "<=", "=", ">=", ">"]),
                rng.pick(&[
                    "-3",
                    "0",
                    "2",
                    "5",
                    "7",
                    "2305843009213693952",
                    "-2305843009213693952",
                    "9223372036854775807"
                ])
            ),
            3 => format!("YEAR({v}) {} 2001", rng.pick(&["<", "=", ">="])),
            _ => format!("2 < {v}"),
        }
    };
    let mut conjuncts = vec![test(rng)];
    if rng.below(3) == 0 {
        conjuncts.push(test(rng));
    }
    conjuncts.join(" && ")
}

/// COUNT of one pattern under a FILTER that bounds one variable, by range counts of the
/// index (`range count`), against the reference evaluator: over the general random data
/// (every literal kind, non-canonical and integer-derived forms, dates with timezones, a
/// third inferred), every read model.
#[test]
fn range_counts_equal_the_reference() {
    let mut rng = Rng::seeded(20_261_014);
    let (mut checked, mut ranged) = (0usize, 0usize);
    for case in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in crate::native_differential_tests::random_dataset(&mut rng) {
            if rng.below(3) == 0 {
                let triple = EncodedTriple::new(
                    tx.intern(quad.subject.as_ref().into()),
                    tx.intern(quad.predicate.as_ref().into()),
                    tx.intern(quad.object.as_ref()),
                );
                tx.insert_inferred(triple);
            } else {
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..25 {
            let options = QueryOptions {
                read_model: *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]),
                ..QueryOptions::default()
            };
            let pattern = match rng.below(3) {
                0 => "?s ?p ?o".to_owned(),
                _ => format!("?s <{EX}p{}> ?o", rng.below(4)),
            };
            let v = *rng.pick(&["?o", "?o", "?s"]);
            let counted = *rng.pick(&["*", "?o", "?s"]);
            let text = format!(
                "SELECT (COUNT({counted}) AS ?n) WHERE {{ {pattern} FILTER({}) }}",
                bounding_filter(&mut rng, v)
            );
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let native = count(evaluate_query(&snapshot, &query, &options).unwrap());
            let expected =
                count(nrese_sparql_reference::evaluate_query(&snapshot, &query, &options).unwrap());
            assert_eq!(
                native, expected,
                "case {case}, {:?}: {text}",
                options.read_model
            );
            let explained = explain_query(&snapshot, &query, &options).unwrap();
            ranged += usize::from(explained.steps.iter().any(|s| s.operator == "range count"));
            checked += 1;
        }
    }
    assert!(
        ranged * 2 > checked,
        "{checked} checked, {ranged} by range counts"
    );
}

/// `COUNT(?v)` and `COUNT(DISTINCT ?v)` of one pattern from the index (its count, its
/// groups counted as they are walked) against the reference evaluator: patterns with
/// constants in every position, over data committed in several revisions with deletions
/// (several runs, tombstones: the walk declines and the groups are listed), a third
/// inferred, every read model.
#[test]
fn single_pattern_counts_equal_the_reference() {
    let mut rng = Rng::seeded(20_261_015);
    let (mut checked, mut walked) = (0usize, 0usize);
    for case in 0..30 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        for _ in 0..1 + rng.below(3) {
            let mut tx = engine.transaction();
            for quad in dataset(&mut rng) {
                match rng.below(6) {
                    0 if quad.graph_name.is_default_graph() => {
                        let triple = EncodedTriple::new(
                            tx.intern(quad.subject.as_ref().into()),
                            tx.intern(quad.predicate.as_ref().into()),
                            tx.intern(quad.object.as_ref()),
                        );
                        tx.insert_inferred(triple);
                    }
                    1 => {
                        tx.remove(quad.as_ref());
                    }
                    _ => {
                        tx.insert(quad.as_ref());
                    }
                }
            }
            tx.commit().unwrap();
        }
        let snapshot = engine.snapshot();
        for _ in 0..30 {
            let options = QueryOptions {
                read_model: *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]),
                ..QueryOptions::default()
            };
            let term = |rng: &mut Rng, v: &str| match rng.below(3) {
                0 => format!("<{EX}e{}>", rng.below(9)),
                _ => v.to_owned(),
            };
            let (s, o) = (term(&mut rng, "?s"), term(&mut rng, "?o"));
            let p = match rng.below(2) {
                0 => "?p".to_owned(),
                _ => format!("<{EX}p{}>", rng.below(5)),
            };
            let vars: Vec<&str> = [s.as_str(), p.as_str(), o.as_str()]
                .into_iter()
                .filter(|t| t.starts_with('?'))
                .collect();
            let Some(&v) = vars.get(rng.below(vars.len().max(1) as u64) as usize) else {
                continue;
            };
            let distinct = if rng.below(2) == 0 { "DISTINCT " } else { "" };
            let text = format!("SELECT (COUNT({distinct}{v}) AS ?n) WHERE {{ {s} {p} {o} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let native = count(evaluate_query(&snapshot, &query, &options).unwrap());
            let expected =
                count(nrese_sparql_reference::evaluate_query(&snapshot, &query, &options).unwrap());
            assert_eq!(
                native, expected,
                "case {case}, {:?}: {text}",
                options.read_model
            );
            let explained = explain_query(&snapshot, &query, &options).unwrap();
            walked += usize::from(
                explained
                    .steps
                    .iter()
                    .any(|s| s.operator == "distinct count"),
            );
            checked += 1;
        }
    }
    assert!(
        walked * 4 > checked,
        "{checked} checked, {walked} distinct counts"
    );
}

#[test]
fn counted_joins_equal_the_reference() {
    let mut rng = Rng::seeded(20_261_013);
    let (mut checked, mut counted, mut nonzero) = (0usize, 0usize, 0usize);
    // The paths each in more than 20 counts: messages by pairs, messages from the parents'
    // side, stars streamed part by part.
    let mut paths = [0usize; 3];
    let covered = |checked: usize, counted: usize, nonzero: usize, paths: [usize; 3]| {
        counted * 2 > checked && nonzero * 3 > checked && paths.iter().all(|&n| n > 20)
    };
    for case in 0.. {
        if !draws_on(case, 30, covered(checked, counted, nonzero, paths)) {
            break;
        }
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in dataset(&mut rng) {
            if quad.graph_name.is_default_graph() && rng.below(3) == 0 {
                let triple = EncodedTriple::new(
                    tx.intern(quad.subject.as_ref().into()),
                    tx.intern(quad.predicate.as_ref().into()),
                    tx.intern(quad.object.as_ref()),
                );
                tx.insert_inferred(triple);
            } else {
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..30 {
            let options = QueryOptions {
                read_model: *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]),
                ..QueryOptions::default()
            };
            let text = count_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let native = count(evaluate_query(&snapshot, &query, &options).unwrap());
            let expected =
                count(nrese_sparql_reference::evaluate_query(&snapshot, &query, &options).unwrap());
            assert_eq!(
                native, expected,
                "case {case}, {:?}: {text}",
                options.read_model
            );
            let explained = explain_query(&snapshot, &query, &options).unwrap();
            let step = explained
                .steps
                .iter()
                .find(|step| step.operator == "counted join");
            counted += usize::from(step.is_some());
            if let Some(step) = step {
                for (n, unused) in paths.iter_mut().zip([
                    " 0 pairs kept",
                    " 0 matches weighed",
                    " 0 groups streamed",
                ]) {
                    *n += usize::from(!step.detail.contains(unused));
                }
            }
            nonzero += usize::from(expected.iter().any(|row| !row.contains("n=\"0\"")));
            checked += 1;
        }
    }
    let [paired, gathered, streamed] = paths;
    assert!(
        covered(checked, counted, nonzero, paths),
        "{checked} checked, {counted} counted by message passing, {nonzero} not 0, {paired} with pairs, {gathered} gathered by parents, {streamed} stars streamed"
    );
}
