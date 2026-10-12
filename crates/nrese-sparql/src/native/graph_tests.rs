//! Deterministic graph-phase cancellation guards; no sleeps or timing.

use super::*;
use nrese_exec::SharedBudget;

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
fn graph_completion_and_explain_cancel_after_algebra() {
    let engine = nrese_engine::Engine::new(Default::default()).unwrap();
    let snapshot = engine.snapshot();
    // CONSTRUCT checks per row: without rows, its empty graph is complete.
    for (text, stops) in [
        (
            "CONSTRUCT { ?s <urn:p> 1 } WHERE { VALUES ?s { <urn:a> <urn:b> } }",
            true,
        ),
        ("DESCRIBE ?s WHERE { VALUES ?s { <urn:a> <urn:b> } }", true),
        ("CONSTRUCT { ?s <urn:p> 1 } WHERE { VALUES ?s {} }", false),
        ("DESCRIBE ?s WHERE { VALUES ?s {} }", true),
    ] {
        let query = nrese_sparql_syntax::SparqlParser::new()
            .parse_query(text)
            .unwrap();
        for explaining in [false, true] {
            for cancel in [false, true] {
                let token = CancellationToken::new();
                let shared = SharedBudget::new(1 << 20);
                let observed = std::rc::Rc::new(Cell::new(0));
                let count = std::rc::Rc::clone(&observed);
                let flag = token.clone();
                // The graph phase passes this hook once, after the algebra.
                CHECKPOINT.set(Some(Box::new(move || {
                    count.set(count.get() + 1);
                    if cancel {
                        flag.cancel();
                    }
                })));
                let options = QueryOptions {
                    resources: crate::Resources {
                        shared_memory: Some(shared.clone()),
                        cancellation: Some(token),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let result = if explaining {
                    explain(&snapshot, &query, &options).map(|_| ())
                } else {
                    typed_results::evaluate(snapshot.clone(), &query, &options).map(drop)
                };
                CHECKPOINT.set(None);
                assert_eq!(observed.get(), 1, "{text}");
                if cancel && stops {
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
