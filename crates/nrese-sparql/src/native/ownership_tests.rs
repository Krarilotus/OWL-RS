//! Composed lifetimes: a result, reader and unrelated owner on the exact same Budget.

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

fn literal(text: &str) -> Term {
    Literal::new_simple_literal(text).into()
}

#[test]
fn computed_result_and_independent_reader_release_only_their_own_storage() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let term = literal("a computed value kept after the context and interning map are gone");
    for result_first in [false, true] {
        let ctx = Context::new(&snapshot, &QueryOptions::default(), None, None);
        let budget = Arc::clone(&ctx.budget);
        let reader = ctx.snapshot.clone().into_owned();
        let reads = budget.used();
        let mut unrelated = Reservation::new(Arc::clone(&budget));
        unrelated.charge(137).unwrap();
        ctx.id(&term).ok().expect("computed term admitted");
        let values = ctx.take_computed();
        let bytes = values._held.bytes();
        assert_eq!(
            bytes,
            values.capacity() * size_of::<Term>() + term.heap_bytes()
        );
        assert_eq!(budget.used(), reads + unrelated.bytes() + bytes);
        drop(ctx);
        assert_eq!(
            values[0], term,
            "payload still readable after Context drops"
        );
        assert_eq!(budget.used(), reads + unrelated.bytes() + bytes);
        if result_first {
            drop(values);
            assert_eq!(budget.used(), reads + unrelated.bytes());
            drop(reader);
        } else {
            drop(reader);
            assert_eq!(budget.used(), unrelated.bytes() + bytes);
            drop(values);
        }
        assert_eq!(budget.used(), unrelated.bytes());
        drop(unrelated);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn run_handoff_does_not_adopt_the_read_state_that_context_already_released() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let query = nrese_sparql_syntax::SparqlParser::new().parse_query(
        "SELECT ?x WHERE { VALUES ?x { \"a query-local computed literal retained by delivery\" } }",
    ).unwrap();
    let (outcome, held) = run(&snapshot, &query, &QueryOptions::default()).unwrap();
    let budget = Arc::clone(held.handle());
    let Outcome::Solutions(solutions, computed) = outcome else {
        panic!("SELECT")
    };
    assert_eq!(budget.used(), solutions.table.charge() + held.bytes());
    let result = Holding::new((solutions, computed), held);
    let mut unrelated = Reservation::new(Arc::clone(&budget));
    unrelated.charge(137).unwrap();
    drop(result);
    assert_eq!(
        budget.used(),
        unrelated.bytes(),
        "result never releases a reader's or another holder's bytes"
    );
    drop(unrelated);
    assert_eq!(budget.used(), 0);
}

#[test]
fn refused_computation_drops_context_caches_while_a_reader_keeps_the_budget_alive() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let stored = literal("a stored term whose optional decoded copy must die with the context");
    let mut tx = engine.transaction();
    tx.insert(
        Quad::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            stored.clone(),
            GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        resources: crate::Resources {
            memory: nrese_exec::Envelope::new(Some(16 << 10), None),
            ..Default::default()
        },
        ..Default::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let budget = Arc::clone(&ctx.budget);
    let reader = ctx.snapshot.clone().into_owned();
    let reads = budget.used();
    let mut unrelated = Reservation::new(Arc::clone(&budget));
    unrelated.charge(137).unwrap();
    let id = snapshot.lookup(stored.as_ref()).unwrap().raw();
    assert_eq!(ctx.term(id), Some(stored));
    ctx.id(&literal("computed before refusal"))
        .ok()
        .expect("first fits");
    let oversized = literal(&"x".repeat(32 << 10));
    assert!(matches!(
        ctx.id(&oversized),
        Err(NativeError::Evaluation(QueryEvaluationError::MemoryLimit(
            _
        )))
    ));
    drop(ctx);
    assert_eq!(
        budget.used(),
        reads + unrelated.bytes(),
        "both context caches freed their own tokens"
    );
    drop(reader);
    assert_eq!(budget.used(), unrelated.bytes());
    drop(unrelated);
    assert_eq!(budget.used(), 0);
}

#[test]
fn grouping_retry_preserves_live_owners_and_reuses_the_existing_streaming_path() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for s in 0..128 {
        let subject = NamedNode::new_unchecked(format!("urn:s{s}"));
        tx.insert(
            Quad::new(
                subject.clone(),
                NamedNode::new_unchecked("urn:p"),
                Literal::new_simple_literal(format!("{s:0100}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        for n in 0..64 {
            tx.insert(
                Quad::new(
                    subject.clone(),
                    NamedNode::new_unchecked("urn:q"),
                    Literal::from(n),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let query = nrese_sparql_syntax::SparqlParser::new().parse_query(
        "SELECT (SUM(STRLEN(?value)) AS ?sum) WHERE { ?s <urn:p> ?value ; <urn:q> ?n . FILTER(STRLEN(?value) > 0) }",
    ).unwrap();
    let options = QueryOptions {
        resources: crate::Resources {
            memory: nrese_exec::Envelope::new(Some(96 << 10), None),
            ..Default::default()
        },
        ..Default::default()
    };
    let ctx = Context::new(&snapshot, &options, None, None);
    let budget = Arc::clone(&ctx.budget);
    let reader = ctx.snapshot.clone().into_owned();
    let reads = budget.used();
    let mut unrelated = Reservation::new(Arc::clone(&budget));
    unrelated.charge(137).unwrap();
    let prior = literal("computed outside the failed group, still referenced by an outer binding");
    let prior_id = ctx.id(&prior).ok().expect("prior binding");
    let baseline = budget.used();
    // Exercise the group owner directly: optimizer eager aggregation may otherwise
    // replace the whole materialization whose refusal this guard deliberately needs.
    let mut pattern = pattern_and_form(&query).0.clone();
    let (inner, variables, aggregates) = loop {
        match &pattern {
            GraphPattern::Project { inner, .. } | GraphPattern::Extend { inner, .. } => {
                pattern = (**inner).clone();
            }
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => break ((**inner).clone(), variables.clone(), aggregates.clone()),
            _ => panic!("a projected grouping"),
        }
    };
    assert!(stream::Plan::of(&inner, &aggregates).is_some());
    let failed = ctx.group_direct(&inner, &variables, &aggregates, None);
    assert!(matches!(
        failed,
        Err(NativeError::Evaluation(QueryEvaluationError::MemoryLimit(
            _
        )))
    ));
    assert!(
        budget.used() > baseline,
        "failed attempt populated the optional decode cache"
    );
    ctx.drop_decoded();
    assert_eq!(
        budget.used(),
        baseline,
        "failed output storage released through RAII"
    );
    let rows = ctx
        .group(&inner, &variables, &aggregates, None)
        .unwrap_or_else(|_| panic!("streamed retry fits"));
    assert_eq!(rows.table.len(), 1);
    assert_eq!(
        ctx.term(rows.table.get(0, 0)),
        Some(Literal::from(128 * 64 * 100).into())
    );
    assert_eq!(ctx.term(prior_id), Some(prior));
    drop(rows);
    ctx.drop_decoded();
    let values = ctx.take_computed();
    let bytes = values._held.bytes();
    drop(ctx);
    assert_eq!(budget.used(), reads + unrelated.bytes() + bytes);
    drop(values);
    assert_eq!(budget.used(), reads + unrelated.bytes());
    drop(reader);
    assert_eq!(budget.used(), unrelated.bytes());
}
