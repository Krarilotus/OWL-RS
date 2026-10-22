//! Strict DEBUG phase-boundary guard: real old failure payload/token, fresh measurement.
use super::*;
use crate::model::Shapes;
use crate::resource_tests::{Probe, engine, resources, shapes};
use crate::validate::{RawBatch, RawChunk, RawResult};
use nrese_exec::measure;

/// Construct genuine old raw storage before each fresh measurement. This isolates the
/// raw->decoded boundary from unrelated Rayon/engine background allocations. Runtime
/// probes separately execute actual raw SPARQL failure production and parallel cleanup.
fn old_raw<'s>(shapes: &'s Shapes, budget: &Arc<Budget>) -> (RawBatch<'s>, *const u8) {
    let shape = &shapes.shapes[0];
    let mut text_held = Reservation::new(&**budget);
    text_held.charge(12 * 1024).unwrap();
    let text = "old failure ".repeat(1024);
    assert_eq!(text.capacity(), text_held.bytes());
    let address = text.as_ptr();
    let (failures, token) = ChargedVec::collect(&**budget, 1, [text])
        .unwrap()
        .into_parts();
    text_held.absorb(token);
    let row = RawResult {
        shape: 0,
        focus: shape.node,
        path: shape.path.as_ref().map(std::borrow::Cow::Borrowed),
        value: None,
        component: Component::MaxCount,
        source: None,
        messages: None,
    };
    let (results, held) = ChargedVec::collect(&**budget, 1, [row])
        .unwrap()
        .into_parts();
    let chunk = RawChunk {
        results,
        failures,
        held: held.into_shared(budget),
        failure_held: Some(text_held.into_shared(budget)),
    };
    assert!(std::ptr::eq(
        chunk.failure_held.as_ref().unwrap().budget(),
        &**budget
    ));
    (
        RawBatch {
            chunks: ChargedVec::collect(budget.clone(), 1, [chunk]).unwrap(),
            budget: budget.clone(),
            applicable: true,
        },
        address,
    )
}

#[test]
fn original_raw_failure_credit_cannot_cover_fresh_decode_or_output() {
    if !measure::alone() {
        return;
    }
    assert!(
        std::hint::black_box(cfg!(debug_assertions)),
        "this guard requires DEBUG instrumentation"
    );
    let engine = engine(
        "",
        &format!(
            "ex:S sh:targetNode ex:a ; sh:path ex:p ; sh:maxCount 0 ; sh:message 'small', '{}' .",
            "m".repeat(4096)
        ),
    );
    let snapshot = engine.snapshot();
    let mut resources = resources(1024 * 1024);
    let shapes = crate::compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = resources.memory.used();
    let budget = Arc::new(resources.memory.part());
    let probe = Probe::new(&snapshot);

    // Directly promote an old borrowed token inside a new measurement. Its old backing
    // buffer stays live; the deliberate new unreserved bytes must still be uncovered.
    let mut prior = Reservation::new(&*budget);
    prior.charge(8192).unwrap();
    let backing = vec![0_u8; 8192];
    let ((promoted, output), promotion_excess) = measure::measure_within(&budget, || {
        let promoted = prior.into_shared(&budget);
        assert!(std::ptr::eq(promoted.budget(), &*budget));
        (promoted, vec![0_u8; 128])
    });
    assert_eq!(
        promotion_excess, 128,
        "promotion must preserve the original generation"
    );
    drop((output, backing, promoted));
    assert_eq!(resources.memory.used(), baseline);

    // Fresh decode itself must reserve every new byte, including both header arrays
    // and the lease-vector allocation, while its old real text/token are excluded.
    let (raw, address) = old_raw(&shapes, &budget);
    let ((), decode_excess) = measure::measure_within(&budget, || {
        let report = decode(&probe, &shapes, raw, &resources).unwrap();
        assert_eq!(
            report.failures()[0].as_ptr(),
            address,
            "move the same String"
        );
        assert!(std::ptr::eq(report._failure_leases[0].budget(), &*budget));
        assert!(std::ptr::eq(
            report.held.as_ref().unwrap().budget(),
            &*budget
        ));
        drop(report);
    });
    assert_eq!(
        decode_excess, 0,
        "no uncovered fresh decode/header/cleanup allocation"
    );
    assert_eq!(resources.memory.used(), baseline);

    // Within the SAME fresh measurement that decoded the old raw owner, deliberately
    // leave a new 128-byte output unreserved. Reattributing the old 12KiB failure
    // credit into the fresh token would falsely report zero here.
    let (raw, address) = old_raw(&shapes, &budget);
    let ((report, output), output_excess) = measure::measure_within(&budget, || {
        let report = decode(&probe, &shapes, raw, &resources).unwrap();
        (report, vec![0_u8; 128])
    });
    assert_eq!(
        output_excess, 128,
        "fresh output cannot borrow old failure credit"
    );
    assert_eq!(report.failures()[0].as_ptr(), address);
    assert!(std::ptr::eq(report._failure_leases[0].budget(), &*budget));
    drop((output, report));
    assert_eq!(resources.memory.used(), baseline);

    let (raw, _) = old_raw(&shapes, &budget);
    let fresh_headers =
        size_of::<ValidationResult>() + size_of::<String>() + size_of::<Reservation>();
    let mut blocker = Reservation::new(budget.clone());
    blocker
        .charge(resources.memory.remaining() - fresh_headers - 256)
        .unwrap();
    let (error, refusal_excess) = measure::measure_within(&budget, || {
        decode(&probe, &shapes, raw, &resources).unwrap_err()
    });
    assert!(matches!(error, QueryEvaluationError::MemoryLimit(_)));
    assert_eq!(refusal_excess, 0);
    assert_eq!(resources.memory.used(), baseline + blocker.bytes());
    drop(blocker);

    let (raw, _) = old_raw(&shapes, &budget);
    let mut probe = Probe::new(&snapshot);
    probe.cancel_decode_at = 1;
    resources.cancellation = Some(probe.token.clone());
    let (error, cancellation_excess) = measure::measure_within(&budget, || {
        decode(&probe, &shapes, raw, &resources).unwrap_err()
    });
    assert!(matches!(error, QueryEvaluationError::Cancelled));
    assert_eq!(cancellation_excess, 0);
    assert_eq!(resources.memory.used(), baseline);
    drop(shapes);
    assert_eq!(resources.memory.used(), 0);
    println!(
        "cross_measure decode_excess={decode_excess} output_excess={output_excess} promotion_excess={promotion_excess} refusal_excess={refusal_excess} cancellation_excess={cancellation_excess} same_budget=true same_text_pointer=true"
    );
}
