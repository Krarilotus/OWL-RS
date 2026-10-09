use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::Literal;

fn table(snapshot: &Snapshot, rows: &[&[u64]], computed: Vec<Term>) -> SolutionTable {
    let table = IdTable::from_rows(rows.first().map_or(1, |r| r.len()), rows.iter().copied());
    let budget = Arc::new(Budget::unlimited());
    budget.charge(table.memory_bytes()).unwrap();
    SolutionTable {
        snapshot: snapshot.clone(),
        variables: vec![Variable::new_unchecked("x")].into(),
        table,
        computed: Arc::new(computed),
        budget,
        extra_budgets: Vec::new(),
    }
}

#[test]
fn computed_ids_are_translated_by_term_and_dictionary_ids_by_identity() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let a: Term = Literal::new_simple_literal("a").into();
    let stored = tx.intern(a.as_ref()).raw();
    tx.insert(
        nrese_rdf::Quad::new(
            nrese_rdf::NamedNode::new_unchecked("urn:s"),
            nrese_rdf::NamedNode::new_unchecked("urn:p"),
            a.clone(),
            nrese_rdf::GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let b: Term = Literal::new_simple_literal("b").into();
    let lower = table(
        &snapshot,
        &[&[computed_id(0)], &[stored], &[UNDEF]],
        vec![a.clone()],
    );
    let upper = table(
        &snapshot,
        &[&[computed_id(0)], &[computed_id(1)], &[UNDEF]],
        vec![b.clone(), a.clone()],
    );
    let pair = lower.align(upper, Arc::new(Budget::unlimited())).unwrap();
    assert_eq!(pair.lower().get(0, 0), stored);
    assert_eq!(pair.lower().get(1, 0), stored);
    assert_ne!(pair.lower().get(0, 0), pair.upper().get(0, 0));
    assert_eq!(pair.lower().get(0, 0), pair.upper().get(1, 0));
    assert_eq!(pair.upper_row(0), vec![Some(b)]);
    assert_eq!(pair.upper_row(1), vec![Some(a)]);
    assert_eq!(pair.upper_row(2), vec![None]);
    let other = Engine::new(EngineConfig::default()).unwrap();
    assert!(
        table(&snapshot, &[&[stored]], vec![])
            .align(
                table(&other.snapshot(), &[&[stored]], vec![]),
                Arc::new(Budget::unlimited())
            )
            .is_err()
    );
}

#[test]
fn retained_results_keep_shared_reservations_and_original_bags_until_delivery() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let shared = nrese_exec::SharedBudget::new(1 << 20);
    let options = crate::QueryOptions {
        shared_memory: Some(Arc::clone(&shared)),
        ..Default::default()
    };
    let query = nrese_sparql_syntax::SparqlParser::new()
        .parse_query("SELECT ?x WHERE { VALUES ?x { 2 1 2 UNDEF } }")
        .unwrap();
    let result = evaluate(engine.snapshot(), &query, &options).unwrap();
    assert!(shared.used() > 0);
    let QueryResults::Solutions(rows) = result.into_results() else {
        panic!()
    };
    let rows: Vec<_> = rows.map(|r| r.unwrap().values().to_vec()).collect();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0], rows[2]);
    assert_ne!(rows[0], rows[1]);
    assert_eq!(rows[3], vec![None]);
    assert_eq!(shared.used(), 0);
}

#[test]
fn aligning_zero_column_results_keeps_their_cardinality() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut lower = table(
        &engine.snapshot(),
        &[&[], &[]],
        vec![Literal::from(1).into()],
    );
    let mut upper = table(&engine.snapshot(), &[&[]], vec![Literal::from(2).into()]);
    lower.variables = Arc::from([]);
    upper.variables = Arc::from([]);
    let pair = lower.align(upper, Arc::new(Budget::unlimited())).unwrap();
    assert_eq!(pair.lower().len(), 2);
    assert_eq!(pair.upper().len(), 1);
}
