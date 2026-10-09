use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryOptions, TypedResults, evaluate_query_typed};
use std::sync::Arc;

fn solutions(engine: &Engine, values: &str) -> nrese_sparql::SolutionTable {
    let query = nrese_sparql_syntax::SparqlParser::new()
        .parse_query(&format!(
            "SELECT ?x ?y WHERE {{ VALUES (?x ?y) {{ {values} }} }}"
        ))
        .unwrap();
    let TypedResults::Solutions(rows) =
        evaluate_query_typed(&engine.snapshot(), &query, &QueryOptions::default()).unwrap()
    else {
        panic!()
    };
    rows
}

#[test]
fn bounded_id_gap_matches_term_oracle_priority_and_preserves_lower_bags() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let lower_values = "(2 UNDEF) (1 \"a\") (2 UNDEF)";
    let upper_values = "(3 \"z\") (1 \"a\") (2 UNDEF) (3 \"z\") (<urn:nrese:u1:hidden> 4) (UNDEF 5) (\"a\\n\" \"x\") (\"a\\\"\" \"y\") (<urn:x> 2)";
    for limit in 0..10 {
        let lower = solutions(&engine, lower_values);
        let upper = solutions(&engine, upper_values);
        let lower_rows: Vec<_> = (0..lower.len()).map(|i| lower.row(i)).collect();
        let known: std::collections::HashSet<_> = lower_rows.iter().cloned().collect();
        let mut expected: Vec<_> = (0..upper.len()).map(|i| upper.row(i)).filter(|r| !known.contains(r)
            && !r.iter().any(|t| matches!(t, Some(nrese_rdf::Term::NamedNode(n)) if n.as_str().starts_with("urn:nrese:u1:")))).collect();
        expected.sort_by_key(|r| format!("{r:?}"));
        expected.dedup();
        let total = expected.len();
        expected.truncate(limit);
        let budget = Arc::new(Budget::unlimited());
        let mut pair = lower.align(upper, Arc::clone(&budget)).unwrap();
        let gap = select(&mut pair, limit, &budget, &CancellationToken::new()).unwrap();
        assert_eq!(gap.total as usize, total);
        assert_eq!(gap.lower, 2);
        assert_eq!(
            gap.rows
                .iter()
                .map(|&i| pair.upper_row(i))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(gap.rows.len() <= limit);
        for &row in &gap.rows {
            pair.append_upper(row).unwrap();
        }
        let result = pair.into_lower();
        assert_eq!(
            (0..lower_rows.len())
                .map(|i| result.row(i))
                .collect::<Vec<_>>(),
            lower_rows
        );
    }
}

#[test]
fn cancellation_and_memory_failure_release_gap_scratch() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let budget = Arc::new(Budget::new(1024));
    let mut pair = solutions(&engine, "(1 2)")
        .align(solutions(&engine, "(3 4) (5 6)"), Arc::clone(&budget))
        .unwrap();
    let before = budget.used();
    let token = CancellationToken::new();
    token.cancel();
    assert!(select(&mut pair, 10, &budget, &token).is_err());
    assert_eq!(budget.used(), before);
    assert!(select(&mut pair, 10, &budget, &CancellationToken::new()).is_err());
    assert_eq!(budget.used(), before);
}

#[test]
fn large_gap_keeps_only_the_admitted_prefix_and_its_reservation() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let values: String = (0..4096).rev().map(|i| format!("({i} UNDEF) ")).collect();
    let lower = solutions(&engine, "");
    let upper = solutions(&engine, &values);
    let mut expected: Vec<_> = (0..upper.len()).map(|i| upper.row(i)).collect();
    expected.sort_by_key(|r| format!("{r:?}"));
    expected.truncate(7);
    let budget = Arc::new(Budget::unlimited());
    let mut pair = lower.align(upper, Arc::clone(&budget)).unwrap();
    let before = budget.used();
    let gap = select(&mut pair, 7, &budget, &CancellationToken::new()).unwrap();
    assert_eq!(gap.total, 4096);
    assert_eq!(gap.rows.len(), 7);
    assert_eq!(
        gap.rows
            .iter()
            .map(|&i| pair.upper_row(i))
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        budget.used() - before,
        (gap.rows.capacity() + gap.bytes.capacity()) * std::mem::size_of::<usize>()
    );
    drop(gap);
    assert_eq!(budget.used(), before);
}
