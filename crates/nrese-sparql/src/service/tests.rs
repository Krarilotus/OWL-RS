use super::*;
use nrese_exec::{
    StopSource,
    measure::{alone, measure_within},
};
use nrese_rdf::{BlankNode, Literal, NamedNode};

#[test]
fn n7_decoded_result_holds_actual_budget_through_borrow_and_extraction() {
    if !alone() {
        return;
    }
    let source = vec![
        Some(
            Triple::new(
                BlankNode::new_unchecked("remote"),
                NamedNode::new_unchecked("urn:p"),
                Literal::new_language_tagged_literal("payload".repeat(200), "de").unwrap(),
            )
            .into(),
        ),
        None,
    ];
    let variables = vec![
        Variable::new_unchecked("x"),
        Variable::new_unchecked("unbound"),
    ];
    let budget = Arc::new(Budget::new(1 << 20));
    let (response, uncovered) = measure_within(&budget, || {
        let mut response = ServiceResults::new(variables, &budget);
        response.push_row(&source, &StopSource::none()).unwrap();
        response.push_row(&source, &StopSource::none()).unwrap();
        response
    });
    assert_eq!(uncovered, 0);
    assert!(Arc::ptr_eq(response.payload.handle(), &budget));
    assert_eq!(response.rows(), &[source.clone(), source]);
    assert!(budget.used() > 2800);
    let held = budget.used();
    {
        let labels: Vec<_> = response
            .rows()
            .iter()
            .map(|row| row[0].as_ref().unwrap())
            .collect();
        assert_eq!(labels.len(), 2);
        assert_eq!(budget.used(), held, "a borrow retains the decoded lease");
    }
    let variables = response.into_variables();
    assert_eq!(variables.len(), 2);
    assert_eq!(
        budget.used(),
        0,
        "raw rows freed before their lease; metadata moved"
    );
}

#[test]
fn n7_decoded_copy_refusal_and_partial_stop_release_payload() {
    if !alone() {
        return;
    }
    let source = vec![Some(
        Literal::new_simple_literal("value".repeat(300)).into(),
    )];
    for limit in [0, 64, 1 << 20] {
        let variables = vec![Variable::new_unchecked("x")];
        let budget = Arc::new(Budget::new(limit));
        let ((response, result), uncovered) = measure_within(&budget, || {
            let mut response = ServiceResults::new(variables, &budget);
            let result = response.push_row(&source, &StopSource::none());
            (response, result)
        });
        assert_eq!(uncovered, 0);
        if limit == 1 << 20 {
            assert!(result.is_ok());
            assert_eq!(response.rows().len(), 1);
        } else {
            assert!(matches!(result, Err(QueryEvaluationError::MemoryLimit(_))));
            assert!(response.rows().is_empty());
        }
        drop(response);
        assert_eq!(budget.used(), 0);
    }

    struct StopAfter(std::cell::Cell<usize>);
    impl StopCheck for StopAfter {
        fn check(&self) -> Result<(), Stop> {
            let left = self.0.get();
            self.0.set(left.saturating_sub(1));
            if left == 0 {
                Err(Stop::Cancelled)
            } else {
                Ok(())
            }
        }
    }
    let budget = Arc::new(Budget::new(1 << 20));
    let variables = vec![Variable::new_unchecked("x")];
    let stop = StopAfter(std::cell::Cell::new(2));
    let ((response, result), uncovered) = measure_within(&budget, || {
        let mut response = ServiceResults::new(variables, &budget);
        let result = response.push_row(&source, &stop);
        (response, result)
    });
    assert_eq!(uncovered, 0);
    assert!(matches!(result, Err(QueryEvaluationError::Cancelled)));
    assert!(response.rows().is_empty());
    drop(response);
    assert_eq!(budget.used(), 0);
}
