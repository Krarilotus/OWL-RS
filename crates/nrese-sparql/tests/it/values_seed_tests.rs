//! A small `VALUES` seeds the pattern it is joined to: the pattern is evaluated from the
//! values (index probes per row), not alone and joined afterwards. Found on the
//! Zebratlas release (4 October 2026): one edge's provenance chain, a one-row `VALUES` and
//! a 12-pattern star, evaluated all 673,884 chains before joining the one edge (164 ms;
//! 0.85 s on the readiness check's machine, against 2.2 ms for Oxigraph).

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{QueryOptions, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// `chains` provenance chains: edge → derivation → record (with properties) → source,
/// and the derivation's activity → agent.
fn engine(chains: usize) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let mut add = |s: NamedNode, p: &str, o: nrese_rdf::Term| {
        tx.insert(Quad::new(s, ex(p), o, GraphName::DefaultGraph).as_ref());
    };
    for i in 0..chains {
        let (edge, derivation, record) = (
            ex(&format!("edge{i}")),
            ex(&format!("d{i}")),
            ex(&format!("r{i}")),
        );
        add(edge, "derivation", derivation.clone().into());
        add(derivation.clone(), "entity", record.clone().into());
        add(derivation, "activity", ex(&format!("run{}", i % 7)).into());
        add(record.clone(), "locator", Literal::from(i as i64).into());
        add(
            record.clone(),
            "hash",
            Literal::new_simple_literal(format!("h{i}")).into(),
        );
        add(record, "source", ex(&format!("file{}", i % 11)).into());
    }
    for k in 0..11 {
        add(
            ex(&format!("file{k}")),
            "hash",
            Literal::new_simple_literal(format!("f{k}")).into(),
        );
    }
    for k in 0..7 {
        add(ex(&format!("run{k}")), "agent", ex("tool").into());
    }
    tx.commit().unwrap();
    engine
}

#[test]
fn a_one_row_values_seeds_the_star_it_joins() {
    let engine = engine(2_000);
    let text = format!(
        "PREFIX : <{EX}> SELECT DISTINCT ?record ?locator ?hash ?sourceHash ?agent WHERE {{
           VALUES ?edge {{ :edge42 }}
           ?edge :derivation ?d . ?d :entity ?record ; :activity ?activity .
           ?record :locator ?locator ; :hash ?hash ; :source ?source .
           ?source :hash ?sourceHash . ?activity :agent ?agent .
         }} ORDER BY ?record"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let snapshot = engine.snapshot();
    let explanation = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    assert_eq!(explanation.rows, 1);
    let widest = explanation
        .steps
        .iter()
        .filter(|step| step.operator != "values")
        .map(|step| step.rows)
        .max()
        .unwrap_or(0);
    assert!(
        widest <= 10,
        "an operator produced {widest} rows; the star wasn't seeded by the one edge: {:#?}",
        explanation.steps
    );
}
