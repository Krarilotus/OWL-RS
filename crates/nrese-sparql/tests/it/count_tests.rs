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
    let counted = match rng.below(3) {
        0 => "?v0",
        _ => "*",
    };
    format!(
        "SELECT (COUNT({counted}) AS ?n) WHERE {{ {} }}",
        body.join(" ")
    )
}

fn count(results: QueryResults<'_>) -> String {
    let QueryResults::Solutions(solutions) = results else {
        panic!("a SELECT")
    };
    let rows: Vec<_> = solutions.map(|s| s.unwrap()).collect();
    assert_eq!(rows.len(), 1);
    rows[0]
        .get("n")
        .map_or_else(|| "unbound".to_owned(), ToString::to_string)
}

#[test]
fn counted_joins_equal_the_reference() {
    let mut rng = Rng::seeded(20_261_013);
    let (mut checked, mut counted, mut nonzero) = (0usize, 0usize, 0usize);
    let covered = |checked: usize, counted: usize, nonzero: usize| {
        counted * 2 > checked && nonzero * 3 > checked
    };
    for case in 0.. {
        if !draws_on(case, 60, covered(checked, counted, nonzero)) {
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
            let by_messages = explained
                .steps
                .iter()
                .any(|step| step.operator == "counted join");
            counted += usize::from(by_messages);
            nonzero += usize::from(!expected.starts_with("\"0\""));
            checked += 1;
        }
    }
    assert!(
        covered(checked, counted, nonzero),
        "{checked} checked, {counted} counted by message passing, {nonzero} not 0"
    );
}
