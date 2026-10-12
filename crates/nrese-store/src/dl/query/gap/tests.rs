use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryEvaluationError, QueryOptions, TypedResults, evaluate_query_typed};
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

fn ids(table: &IdTable, row: usize) -> Vec<u64> {
    table.columns().iter().map(|c| c[row]).collect()
}

/// The gap's admitted rows, by their ids.
fn admitted(gap: &Gap<'_>, pair: &AlignedSolutions) -> Vec<Vec<u64>> {
    gap.rows.iter().map(|&i| ids(pair.upper(), i)).collect()
}

#[test]
fn bounded_id_gap_matches_its_oracle_in_id_order_and_preserves_lower_bags() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let lower_values = "(2 UNDEF) (1 \"a\") (2 UNDEF)";
    let upper_values = "(3 \"z\") (1 \"a\") (2 UNDEF) (3 \"z\") (<urn:nrese:u1:hidden> 4) (UNDEF 5) (\"a\\n\" \"x\") (\"a\\\"\" \"y\") (<urn:x> 2)";
    for limit in 0..10 {
        let lower = solutions(&engine, lower_values);
        let upper = solutions(&engine, upper_values);
        let lower_rows: Vec<_> = (0..lower.len()).map(|i| lower.row(i)).collect();
        let budget = Arc::new(Budget::unlimited());
        let mut pair = lower.align(upper, Arc::clone(&budget)).unwrap();
        let known: std::collections::HashSet<_> = (0..pair.lower().len())
            .map(|i| ids(pair.lower(), i))
            .collect();
        let reserved = |id: u64| matches!(pair.term(id), Some(nrese_rdf::Term::NamedNode(n)) if n.as_str().starts_with("urn:nrese:u1:"));
        let mut expected: Vec<_> = (0..pair.upper().len())
            .map(|i| ids(pair.upper(), i))
            .filter(|r| !known.contains(r) && !r.iter().any(|&id| reserved(id)))
            .collect();
        expected.sort();
        expected.dedup();
        let total = expected.len();
        expected.truncate(limit);
        let gap = select(&mut pair, limit, &budget, &CancellationToken::new()).unwrap();
        assert_eq!(gap.total as usize, total);
        assert_eq!(gap.lower, 2);
        assert_eq!(admitted(&gap, &pair), expected);
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
    // Room for the lower rows' scratch, not for the gap's distinct terms.
    budget.charge(1024 - budget.used() - 64).unwrap();
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
    let budget = Arc::new(Budget::unlimited());
    let mut pair = lower.align(upper, Arc::clone(&budget)).unwrap();
    let mut expected: Vec<_> = (0..pair.upper().len())
        .map(|i| ids(pair.upper(), i))
        .collect();
    expected.sort();
    expected.truncate(7);
    let before = budget.used();
    let gap = select(&mut pair, 7, &budget, &CancellationToken::new()).unwrap();
    assert_eq!(gap.total, 4096);
    assert_eq!(gap.rows.len(), 7);
    assert_eq!(admitted(&gap, &pair), expected);
    assert_eq!(
        budget.used() - before,
        (gap.rows.capacity() + gap.bytes.capacity()) * std::mem::size_of::<usize>()
    );
    drop(gap);
    assert_eq!(budget.used(), before);
}

type Checkpoint = Box<dyn Fn(bool)>;

thread_local! {
    static CHECKPOINT: std::cell::RefCell<Option<Checkpoint>> = std::cell::RefCell::new(None);
}

pub(super) fn checkpoint(finished: bool) {
    CHECKPOINT.with(|hook| {
        if let Some(hook) = &*hook.borrow() {
            hook(finished);
        }
    });
}

#[test]
fn gap_releases_scratch_on_cancel_or_drop() {
    use nrese_exec::SharedBudget;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let engine = Engine::new(EngineConfig::default()).unwrap();
    // Unsorted 2^16 rows enter IdTable's parallel sort/gather/filter branches.
    // Only two distinct rows keep the decoded oracle and admission scratch small.
    let values = "(2 UNDEF) (1 UNDEF) ".repeat(1 << 15);
    for cancel_at in [None, Some(false), Some(true)] {
        let shared = SharedBudget::new(8 << 20);
        let budget = Arc::new(Budget::unlimited().within(Some(Arc::clone(&shared))));
        let mut pair = solutions(&engine, "(0 UNDEF) (0 UNDEF)")
            .align(solutions(&engine, &values), Arc::clone(&budget))
            .unwrap();
        assert_eq!(pair.upper().len(), 1 << 16);
        assert!(!pair.upper().is_sorted_on(&[0, 1]));
        let sort_scratch = pair.upper().memory_bytes() + pair.upper().len() * 24;
        let mut expected = vec![ids(pair.upper(), 0), ids(pair.upper(), 1)];
        expected.sort();
        let before = budget.used();
        let token = CancellationToken::new();
        let observed = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&observed);
        let flag = token.clone();
        CHECKPOINT.set(Some(Box::new(move |finished| {
            count.fetch_add(1, Ordering::Relaxed);
            if cancel_at == Some(finished) {
                flag.cancel();
            }
        })));
        let result = select(&mut pair, 2, &budget, &token);
        CHECKPOINT.set(None);
        assert_eq!(
            observed.load(Ordering::Relaxed),
            if cancel_at == Some(false) { 1 } else { 2 }
        );
        assert!(budget.peak() >= before + sort_scratch);
        if cancel_at.is_some() {
            assert!(matches!(
                result,
                Err(crate::StoreError::SparqlEvaluation(
                    QueryEvaluationError::Cancelled
                ))
            ));
            drop(result);
        } else {
            let gap = result.unwrap();
            assert_eq!((gap.lower, gap.total), (1, 2));
            assert_eq!(admitted(&gap, &pair), expected);
            assert_eq!(pair.lower().len(), 2, "lower bags stay untouched");
            assert_eq!(
                budget.used() - before,
                (gap.rows.capacity() + gap.bytes.capacity()) * std::mem::size_of::<usize>()
            );
            assert_eq!(shared.used(), budget.used());
            drop(gap);
        }
        assert_eq!(budget.used(), before);
        drop(pair);
        drop(budget);
        assert_eq!(shared.used(), 0);
    }
}
