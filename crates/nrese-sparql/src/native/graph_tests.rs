//! Deterministic graph-phase ownership and cancellation guards; no sleeps or timing.

use super::*;
use nrese_exec::{SharedBudget, workers::Workers};
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static CHECKPOINT: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

pub(super) fn checkpoint() {
    CHECKPOINT.with(|hook| {
        if let Some(hook) = &*hook.borrow() {
            hook();
        }
    });
}

#[test]
fn graph_completion_and_explain_stay_on_the_owner_and_cancel_after_algebra() {
    let engine = nrese_engine::Engine::new(Default::default()).unwrap();
    let snapshot = engine.snapshot();
    let workers = Workers::pooled(1).unwrap();
    let caller = std::thread::current().id();
    for text in [
        "CONSTRUCT { ?s <urn:p> 1 } WHERE { VALUES ?s { <urn:a> <urn:b> } }",
        "DESCRIBE ?s WHERE { VALUES ?s { <urn:a> <urn:b> } }",
        "CONSTRUCT { ?s <urn:p> 1 } WHERE { VALUES ?s {} }",
        "DESCRIBE ?s WHERE { VALUES ?s {} }",
    ] {
        let query = nrese_sparql_syntax::SparqlParser::new()
            .parse_query(text)
            .unwrap();
        for explaining in [false, true] {
            for cancel in [false, true] {
                let token = CancellationToken::new();
                let shared = SharedBudget::new(1 << 20);
                let observed = Arc::new(AtomicUsize::new(0));
                let count = Arc::clone(&observed);
                let flag = token.clone();
                // Install only on the physical owner. A caller-side graph phase misses
                // this hook, so the count below detects a misplaced completion stage.
                workers.install(move || {
                    CHECKPOINT.set(Some(Box::new(move || {
                        assert_ne!(std::thread::current().id(), caller);
                        count.fetch_add(1, Ordering::Relaxed);
                        if cancel {
                            flag.cancel();
                        }
                    })))
                });
                let options = QueryOptions {
                    workers: Some(workers.clone()),
                    shared_memory: Some(shared.clone()),
                    cancellation: Some(token),
                    ..Default::default()
                };
                let result = if explaining {
                    explain(&snapshot, &query, &options).map(|_| ())
                } else {
                    typed_results::evaluate(snapshot.clone(), &query, &options).map(drop)
                };
                workers.install(|| CHECKPOINT.set(None));
                assert_eq!(observed.load(Ordering::Relaxed), 1, "{text}");
                if cancel {
                    assert!(
                        matches!(result, Err(QueryEvaluationError::Cancelled)),
                        "{result:?}"
                    );
                } else {
                    result.unwrap();
                }
                assert_eq!(
                    shared.used(),
                    0,
                    "completion/drop/error releases reservations"
                );
            }
        }
    }
}

#[test]
fn construct_checks_rows_and_wide_templates_even_when_no_triple_is_emitted() {
    let engine = nrese_engine::Engine::new(Default::default()).unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    for (values, width) in [("<urn:a> <urn:b> <urn:c>", 1), ("<urn:a>", 1024)] {
        let query = nrese_sparql_syntax::SparqlParser::new()
            .parse_query(&format!(
                "CONSTRUCT {{ ?s <urn:p> ?unbound }} WHERE {{ VALUES ?s {{ {values} }} }}"
            ))
            .unwrap();
        let ctx = Context::new(
            &snapshot,
            &options,
            query_dataset(&query),
            query_base(&query),
        );
        let (pattern, form, _, _) = native_pattern(&query, &options, &ctx).unwrap();
        let solutions = ctx
            .eval_root(&pattern, None)
            .map_err(QueryEvaluationError::from)
            .unwrap();
        let Form::Construct(template) = form else {
            panic!()
        };
        let template = vec![template[0].clone(); width];
        let checks = std::cell::Cell::new(0);
        let mut triples = construct(&snapshot, ctx.computed.take(), solutions, &template, || {
            checks.set(checks.get() + 1);
            if checks.get() == 3 {
                Err(QueryEvaluationError::Cancelled.into())
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            triples.next(),
            Some(Err(NativeError::Evaluation(
                QueryEvaluationError::Cancelled
            )))
        ));
        assert!(
            triples.next().is_none(),
            "a cancelled expansion stays stopped"
        );
        assert_eq!(checks.get(), 3);
    }
}
