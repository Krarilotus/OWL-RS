use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::Literal;

fn table(snapshot: &Snapshot, rows: &[&[u64]], computed: Vec<Term>) -> SolutionTable {
    let budget = Arc::new(Budget::unlimited());
    let width = rows.first().map_or(1, |r| r.len());
    // The table holds its own charge; the result holds what the query charged directly.
    let table = IdTable::from_rows(width, rows.iter().copied(), &budget).unwrap();
    SolutionTable {
        snapshot: snapshot.clone(),
        variables: vec![Variable::new_unchecked("x")].into(),
        held: Reservation::new(budget),
        table,
        computed: Arc::new(computed),
        aligned: None,
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
        resources: crate::Resources {
            memory: nrese_exec::Envelope::new(None, Some(Arc::clone(&shared))),
            ..Default::default()
        },
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

/// C1's real public composition: unbound SELECT projection, identity alignment,
/// explicit reserve and appended bag rows. Assert capacity before any append on old code.
#[test]
fn projected_unbound_column_is_safe_to_append() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let parser = nrese_sparql_syntax::SparqlParser::new();
    let lower_query = parser.parse_query("SELECT ?x WHERE {}").unwrap();
    let upper_query = parser
        .parse_query("SELECT ?x WHERE { VALUES ?x { 7 7 UNDEF } }")
        .unwrap();
    let TypedResults::Solutions(lower) = evaluate(
        engine.snapshot(),
        &lower_query,
        &crate::QueryOptions::default(),
    )
    .unwrap() else {
        panic!("SELECT")
    };
    let TypedResults::Solutions(upper) = evaluate(
        engine.snapshot(),
        &upper_query,
        &crate::QueryOptions::default(),
    )
    .unwrap() else {
        panic!("SELECT")
    };
    assert_eq!((lower.len(), lower.table.width()), (1, 1));
    assert_eq!(
        lower.table.room_left(),
        0,
        "unbound projection has finite room before any append"
    );
    assert_eq!(lower.row(0), vec![None]);
    let value = upper.row(0);
    assert_eq!(value, upper.row(1));
    assert_eq!(upper.row(2), vec![None]);
    let actual = |table: &IdTable| table.memory_bytes();
    assert_eq!(
        lower.table.memory_bytes(),
        4 * size_of::<Vec<u64>>() + lower.table.columns()[0].capacity() * 8,
        "first-column header participates in recharge"
    );
    // Existing projection charge_ahead may retain a freed/unused header bound until
    // alignment settles it. It must still cover the actual first-column allocation.
    assert!(lower.table.charge() >= actual(&lower.table));
    let budget = Arc::new(Budget::unlimited());
    let mut unrelated = Reservation::new(Arc::clone(&budget));
    unrelated.charge(13).unwrap();
    let mut pair = lower.align(upper, Arc::clone(&budget)).unwrap();
    assert_eq!(pair.lower().room_left(), 0);
    assert_eq!(pair.lower().charge(), actual(pair.lower()));
    assert_eq!(pair.upper().charge(), actual(pair.upper()));
    assert_eq!(
        budget.used(),
        13 + pair.held.bytes() + pair.lower().charge() + pair.upper().charge()
    );
    pair.reserve_appends(3).unwrap();
    assert_eq!(pair.lower().room_left(), 3);
    for row in 0..3 {
        pair.append_upper(row).unwrap();
    }
    assert_eq!((pair.lower().len(), pair.upper().len()), (4, 3));
    assert_eq!(pair.lower().charge(), actual(pair.lower()));
    assert_eq!(
        budget.used(),
        13 + pair.held.bytes() + pair.lower().charge() + pair.upper().charge()
    );
    let result = pair.into_lower();
    assert_eq!(
        budget.used(),
        13 + result.table.charge() + result.aligned.as_ref().unwrap().bytes()
    );
    let QueryResults::Solutions(rows) = TypedResults::Solutions(result).into_results() else {
        panic!("solutions")
    };
    let rows: Vec<_> = rows.map(|row| row.unwrap().values().to_vec()).collect();
    assert_eq!(rows, vec![vec![None], value.clone(), value, vec![None]]);
    assert_eq!(
        budget.used(),
        13,
        "result and alignment release only their actual owners"
    );
    drop(unrelated);
    assert_eq!(budget.used(), 0);
}
