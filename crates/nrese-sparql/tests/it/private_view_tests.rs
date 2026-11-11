//! SPARQL over a private transaction (`Engine::speculative_over`), read through
//! [`PendingRead`]: what it reads is the transaction's own state and vocabulary, or the query
//! fails with a typed error. It never falls back to the base, never answers empty for a
//! refusal, and adds nothing to the dictionary. Dictionary lifecycle V2
//! (`notes/2026-11-09-dict-v2-readview-design.md` §3–4).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_engine::{Engine, EngineConfig, PendingView, PrivateTransaction, Snapshot};
use nrese_exec::Budget;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{
    PendingRead, QueryEvaluationError, QueryOptions, QueryResults, ReadSource, ReadView,
    TypedResults, UpdateError, UpdateOptions, apply_update, evaluate_query, evaluate_query_typed,
    explain_query, plan_query, ql_report,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// The private transactions' own term: never in the dictionary.
fn own() -> Term {
    Literal::new_simple_literal("only the transaction has this").into()
}

fn own_quad() -> Quad {
    Quad::new(ex("a"), ex("p"), own(), GraphName::DefaultGraph)
}

/// An engine holding `ex:a ex:p "published"`.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let published: Term = Literal::new_simple_literal("published").into();
    tx.insert(Quad::new(ex("a"), ex("p"), published, GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    engine
}

fn private<'e>(engine: &'e Engine, budget: &Arc<Budget>) -> PrivateTransaction<'e> {
    engine
        .speculative_over(&engine.snapshot(), Arc::clone(budget))
        .unwrap()
}

fn unlimited() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

const OBJECTS: &str = "SELECT ?o WHERE { <http://example.com/a> <http://example.com/p> ?o }";

fn objects(view: &impl ReadView) -> Result<Vec<Term>, QueryEvaluationError> {
    let query = SparqlParser::new().parse_query(OBJECTS).unwrap();
    let QueryResults::Solutions(solutions) =
        evaluate_query(view, &query, &QueryOptions::default())?
    else {
        panic!("a SELECT");
    };
    let mut terms: Vec<Term> = solutions
        .map(|solution| solution.unwrap().get("o").cloned().unwrap())
        .collect();
    terms.sort_by_key(ToString::to_string);
    Ok(terms)
}

/// The dictionary holds `terms` terms, and not the transaction's own: nothing the private
/// work did reached it.
fn unpublished(engine: &Engine, terms: u64) {
    assert_eq!(
        engine.stats().dictionary.terms,
        terms,
        "a term reached the dictionary"
    );
    assert_eq!(engine.snapshot().lookup(own().as_ref()), None);
}

/// The smallest budget limit under which `work` succeeds on a fresh private transaction over
/// `engine`'s latest snapshot, found by trying each limit in turn: a step's peak request can
/// be more than what it keeps, so the charge left afterwards doesn't size a budget.
fn least_limit(engine: &Engine, work: impl Fn(&mut PrivateTransaction<'_>) -> bool) -> usize {
    (0..=1 << 16)
        .find(|&limit| work(&mut private(engine, &Arc::new(Budget::new(limit)))))
        .expect("the work fits in 64 KiB")
}

/// (the least budget that holds the transaction's own term, the least that also holds a
/// pending snapshot of it), measured on `engine`: what the refusal tests size their budgets
/// by. Guard: the second is larger, as freezing the vocabulary is charged.
fn limits(engine: &Engine) -> (usize, usize) {
    let interning = least_limit(engine, |tx| tx.try_insert(own_quad().as_ref()).is_ok());
    let freezing = least_limit(engine, |tx| {
        tx.try_insert(own_quad().as_ref()).is_ok()
            && tx.pending_snapshot_within(&unlimited()).is_ok()
    });
    assert!(freezing > interning, "freezing the vocabulary is charged");
    (interning, freezing)
}

/// Every read of a private transaction sees its own changes and terms.
///
/// - Input: `ex:a ex:p "published"` committed; a private transaction asserting
///   `ex:a ex:p "only the transaction has this"`.
/// - Expected: the query answers both objects, and the dictionary is as it was (invariant
///   item 1, A5, A6).
#[test]
fn a_query_over_a_private_transaction_reads_its_own_state() {
    let engine = engine();
    let terms = engine.stats().dictionary.terms;
    let budget = unlimited();
    let mut tx = private(&engine, &budget);
    tx.try_insert(own_quad().as_ref()).unwrap();
    let reads = unlimited();
    let published: Term = Literal::new_simple_literal("published").into();
    assert_eq!(
        objects(&PendingRead::new(&tx, &reads)).unwrap(),
        [own(), published]
    );
    drop(tx);
    unpublished(&engine, terms);
}

/// A refused evaluation snapshot is a typed error before anything runs, never the base's
/// answer or an empty one (A1).
///
/// - Input: a private transaction whose budget holds its own term but not the frozen view a
///   pending snapshot needs (the budgets measured by [`limits`]).
/// - Expected: evaluation, typed evaluation, explain, plan and the QL report each fail with
///   `MemoryLimit`. Guard: with the frozen view's charge allowed, the same query answers.
#[test]
fn a_refused_evaluation_snapshot_is_a_memory_error_on_every_entry() {
    let engine = engine();
    let (interning, freezing) = limits(&engine);
    let query = SparqlParser::new().parse_query(OBJECTS).unwrap();
    let options = QueryOptions::default();
    let reads = unlimited();
    let budget = Arc::new(Budget::new(interning));
    let mut tx = private(&engine, &budget);
    tx.try_insert(own_quad().as_ref()).unwrap();
    let view = PendingRead::new(&tx, &reads);
    let refused = |result: Result<(), QueryEvaluationError>, entry: &str| {
        assert!(
            matches!(result, Err(QueryEvaluationError::MemoryLimit(_))),
            "{entry}: {result:?}"
        );
    };
    refused(objects(&view).map(drop), "evaluate_query");
    refused(
        evaluate_query_typed(&view, &query, &options).map(drop),
        "evaluate_query_typed",
    );
    refused(
        explain_query(&view, &query, &options).map(drop),
        "explain_query",
    );
    refused(plan_query(&view, &query, &options).map(drop), "plan_query");
    refused(ql_report(&view, &query, &options).map(drop), "ql_report");

    let budget = Arc::new(Budget::new(freezing));
    let mut tx = private(&engine, &budget);
    tx.try_insert(own_quad().as_ref()).unwrap();
    assert_eq!(
        objects(&PendingRead::new(&tx, &reads)).unwrap().len(),
        2,
        "the guard: room for the view answers"
    );
}

/// A held result keeps the vocabulary it was computed in (LC S2, A2).
///
/// - Input: typed results of a query over a private transaction, read after the
///   transaction is dropped.
/// - Expected: they decode the transaction's own term; the dictionary is as it was; the
///   charge goes with the last holder.
#[test]
fn held_results_outlive_the_private_transaction() {
    let engine = engine();
    let terms = engine.stats().dictionary.terms;
    let budget = unlimited();
    let query = SparqlParser::new()
        .parse_query("SELECT ?o WHERE { ?s ?p ?o FILTER(STRSTARTS(STR(?o), \"only\")) }")
        .unwrap();
    let typed = {
        let mut tx = private(&engine, &budget);
        tx.try_insert(own_quad().as_ref()).unwrap();
        let reads = unlimited();
        evaluate_query_typed(
            &PendingRead::new(&tx, &reads),
            &query,
            &QueryOptions::default(),
        )
        .unwrap()
    };
    let TypedResults::Solutions(table) = typed else {
        panic!("a SELECT");
    };
    assert_eq!(table.len(), 1);
    assert_eq!(table.row(0), [Some(own())]);
    unpublished(&engine, terms);
    drop(table);
    assert_eq!(budget.used(), 0, "released with the last holder");
}

/// Queries nested in a running query's rows, on one private view, as SHACL's checks run
/// per focus node (A3).
///
/// - Input: one SELECT over the transaction, an ASK on the same view for each of its rows,
///   then 50 more queries.
/// - Expected: every nested ASK sees the transaction's own term; repeated evaluations leave
///   the transaction's charge where the first one left it.
#[test]
fn queries_reenter_one_private_view() {
    let engine = engine();
    let budget = unlimited();
    let mut tx = private(&engine, &budget);
    tx.try_insert(own_quad().as_ref()).unwrap();
    let reads = unlimited();
    let view = PendingRead::new(&tx, &reads);
    let outer = SparqlParser::new().parse_query(OBJECTS).unwrap();
    let ask = SparqlParser::new()
        .parse_query(&format!("ASK {{ ?s ?p {} }}", own()))
        .unwrap();
    let options = QueryOptions::default();
    let QueryResults::Solutions(rows) = evaluate_query(&view, &outer, &options).unwrap() else {
        panic!("a SELECT");
    };
    let mut seen = 0;
    for row in rows {
        row.unwrap();
        let QueryResults::Boolean(found) = evaluate_query(&view, &ask, &options).unwrap() else {
            panic!("an ASK");
        };
        assert!(found, "the nested query reads the transaction's term");
        seen += 1;
    }
    assert_eq!(seen, 2);
    let settled = budget.used();
    for _ in 0..50 {
        assert_eq!(objects(&view).unwrap().len(), 2);
    }
    assert_eq!(
        budget.used(),
        settled,
        "no charge left behind per evaluation"
    );
}

/// An update over a private transaction: each `WHERE` reads the earlier operations, its own
/// terms included, and nothing enters the dictionary (A4).
///
/// - Input: `INSERT DATA` of the own term, also into a new graph, then `DELETE/INSERT …
///   WHERE` moving it to `ex:q`, then `CLEAR GRAPH` of the new graph.
/// - Expected: `ex:a ex:q` has the own term, the new graph is empty, and the dictionary is as
///   it was, without the term or the graph's IRI.
#[test]
fn an_update_over_a_private_transaction_reads_its_earlier_operations() {
    let engine = engine();
    let terms = engine.stats().dictionary.terms;
    let budget = unlimited();
    let mut tx = private(&engine, &budget);
    let graph = ex("g/private");
    let update = SparqlParser::new()
        .parse_update(&format!(
            "INSERT DATA {{ <{EX}a> <{EX}p> {own} . GRAPH {graph} {{ <{EX}a> <{EX}p> {own} }} }} ;
             DELETE {{ ?s <{EX}p> ?o }} INSERT {{ ?s <{EX}q> ?o }}
               WHERE {{ ?s <{EX}p> ?o FILTER(?o = {own}) }} ;
             CLEAR GRAPH {graph}",
            own = own()
        ))
        .unwrap();
    apply_update(&mut tx, &update, &UpdateOptions::default()).unwrap();
    let reads = unlimited();
    let view = PendingRead::new(&tx, &reads);
    let moved = SparqlParser::new()
        .parse_query(&format!("SELECT ?o WHERE {{ <{EX}a> <{EX}q> ?o }}"))
        .unwrap();
    let QueryResults::Solutions(rows) =
        evaluate_query(&view, &moved, &QueryOptions::default()).unwrap()
    else {
        panic!("a SELECT");
    };
    let rows: Vec<Term> = rows
        .map(|row| row.unwrap().get("o").cloned().unwrap())
        .collect();
    assert_eq!(rows, [own()]);
    let left = SparqlParser::new()
        .parse_query(&format!("ASK {{ GRAPH {graph} {{ ?s ?p ?o }} }}"))
        .unwrap();
    assert!(matches!(
        evaluate_query(&view, &left, &QueryOptions::default()).unwrap(),
        QueryResults::Boolean(false)
    ));
    drop(tx);
    unpublished(&engine, terms);
    assert_eq!(engine.snapshot().lookup(graph.as_ref().into()), None);
}

/// A refused write and a refused `WHERE` snapshot fail the update with a typed error;
/// neither is skipped (A1, A4).
///
/// - Input: (1) a budget with no room, `INSERT DATA` of a new term; (2) the least budget that
///   holds the own term ([`limits`]), `INSERT DATA` of it, then an `INSERT … WHERE` that must
///   read it (its snapshot refused).
/// - Expected: both fail with `MemoryLimit`; in (2), after the insert was applied (guard: the
///   refusal is the `WHERE`'s, not the insert's).
#[test]
fn a_refused_update_is_a_memory_error() {
    let engine = engine();
    let (interning, _) = limits(&engine);
    let memory = |result: Result<(), UpdateError>| {
        assert!(
            matches!(
                result,
                Err(UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(
                    _
                )))
            ),
            "{result:?}"
        );
    };
    let insert = SparqlParser::new()
        .parse_update(&format!("INSERT DATA {{ <{EX}a> <{EX}p> {} }}", own()))
        .unwrap();
    let mut tx = private(&engine, &Arc::new(Budget::new(0)));
    memory(apply_update(&mut tx, &insert, &UpdateOptions::default()));

    let reading = SparqlParser::new()
        .parse_update(&format!(
            "INSERT DATA {{ <{EX}a> <{EX}p> {own} }} ;
             INSERT {{ <{EX}a> <{EX}q> ?o }} WHERE {{ <{EX}a> <{EX}p> ?o FILTER(?o = {own}) }}",
            own = own()
        ))
        .unwrap();
    let mut tx = private(&engine, &Arc::new(Budget::new(interning)));
    memory(apply_update(&mut tx, &reading, &UpdateOptions::default()));
    assert_eq!(
        tx.pending(),
        (1, 0),
        "the insert was applied before the refusal"
    );
}

/// The update's reader view of a private transaction comes from its read source's private
/// method (A4, A5: a session read's `WHERE`).
///
/// - Input: a read source answering a transaction's pending snapshot and counting its calls
///   per kind; an update with one `WHERE` over a private transaction.
/// - Expected: one private call, no shared call, and the `WHERE` saw the earlier insert.
#[test]
fn an_updates_read_source_reads_a_private_transaction() {
    #[derive(Default)]
    struct Counting {
        shared: AtomicUsize,
        private: AtomicUsize,
    }
    fn state(
        tx: &impl PendingView,
        budget: &Arc<Budget>,
    ) -> Result<Snapshot, QueryEvaluationError> {
        tx.pending_snapshot_within(budget)
            .map_err(|error| QueryEvaluationError::Unexpected(error.to_string()))
    }
    impl ReadSource for Counting {
        fn state(
            &self,
            tx: &nrese_engine::Transaction<'_>,
            budget: &Arc<Budget>,
        ) -> Result<Snapshot, QueryEvaluationError> {
            self.shared.fetch_add(1, Ordering::SeqCst);
            state(tx, budget)
        }
        fn private_state(
            &self,
            tx: &PrivateTransaction<'_>,
            budget: &Arc<Budget>,
        ) -> Result<Snapshot, QueryEvaluationError> {
            self.private.fetch_add(1, Ordering::SeqCst);
            state(tx, budget)
        }
    }
    let engine = engine();
    let budget = unlimited();
    let mut tx = private(&engine, &budget);
    let source = Arc::new(Counting::default());
    let options = UpdateOptions {
        read: Some(Arc::clone(&source) as Arc<dyn ReadSource>),
        ..UpdateOptions::default()
    };
    let update = SparqlParser::new()
        .parse_update(&format!(
            "INSERT DATA {{ <{EX}a> <{EX}p> {own} }} ;
             INSERT {{ <{EX}b> <{EX}q> ?o }} WHERE {{ <{EX}a> <{EX}p> ?o FILTER(?o = {own}) }}",
            own = own()
        ))
        .unwrap();
    apply_update(&mut tx, &update, &options).unwrap();
    assert_eq!(source.private.load(Ordering::SeqCst), 1);
    assert_eq!(source.shared.load(Ordering::SeqCst), 0);
    let copied = SparqlParser::new()
        .parse_query(&format!("ASK {{ <{EX}b> <{EX}q> {} }}", own()))
        .unwrap();
    let reads = unlimited();
    assert!(matches!(
        evaluate_query(
            &PendingRead::new(&tx, &reads),
            &copied,
            &QueryOptions::default()
        )
        .unwrap(),
        QueryResults::Boolean(true)
    ));
}

/// Search over a private view finds what the view holds, never an empty match (A7). Until V3
/// such a search was refused with an error; V3 (dictionary lifecycle) searches a private
/// snapshot through its own charged path, so the property A7 protects (no empty answer, no
/// read of another state) is now the transaction's own term as the one match.
///
/// - Input: a text search for a word only the transaction's own term has; the published base
///   holds only `"published"`.
/// - Expected: exactly that term, so neither the base nor the shared transaction answered;
///   the dictionary unchanged.
#[test]
fn a_search_over_a_private_transaction_finds_its_own_term() {
    let engine = engine();
    let terms = engine.stats().dictionary.terms;
    let budget = unlimited();
    let mut tx = private(&engine, &budget);
    tx.try_insert(own_quad().as_ref()).unwrap();
    let reads = unlimited();
    let query = SparqlParser::new()
        .parse_query(
            "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
             SELECT ?o WHERE { ?o bds:search \"transaction\" }",
        )
        .unwrap();
    let rows = evaluate_query(
        &PendingRead::new(&tx, &reads),
        &query,
        &QueryOptions::default(),
    )
    .and_then(|results| match results {
        QueryResults::Solutions(rows) => rows
            .map(|row| row.map(|solution| solution.get("o").cloned()))
            .collect::<Result<Vec<_>, _>>(),
        _ => panic!("a SELECT"),
    })
    .expect("a private view's search is answered");
    assert_eq!(
        rows,
        [Some(own())],
        "the transaction's own term, and only it"
    );
    drop(tx);
    unpublished(&engine, terms);
}
