use super::*;
use nrese_exec::measure;
use nrese_rdf::NamedNodeRef;

/// Spec: a node shape's values are just its focus. After task preparation, a
/// conforming IRI node-kind check needs no heap allocation or owned value buffer.
#[test]
fn node_focus_value_needs_no_heap_after_task_preparation() {
    if !measure::alone() {
        return;
    }
    let engine = crate::resource_tests::engine(
        "",
        "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:nodeKind sh:IRI .",
    );
    let view = engine.snapshot();
    let resources = crate::resource_tests::resources(4 * 1024 * 1024);
    let shapes = crate::compile(&view, crate::resource_tests::shapes(&view), &resources).unwrap();
    let shape = shapes.targeted().next().unwrap();
    let focus = view
        .lookup(NamedNodeRef::new_unchecked("http://example.com/a").into())
        .unwrap();
    let validator = Validator::new(
        &view,
        &shapes,
        Selection::of(nrese_engine::GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
        &resources,
        Arc::new(resources.memory.part()),
    )
    .unwrap();
    let mut task = Task::new(&validator).unwrap();
    let mut results = ChargedVec::new(&*validator.budget);
    task.validate_node(shape, focus, &mut results).unwrap();
    assert!(results.is_empty(), "an IRI conforms to sh:IRI");
    let (outcome, peak) = measure::measure(|| task.validate_node(shape, focus, &mut results));
    outcome.unwrap();
    assert!(results.is_empty(), "a repeated check still conforms");
    eprintln!("node_focus_values peak_requested_bytes={peak} expected=0");
    assert_eq!(
        peak, 0,
        "a prepared node focus requires no owned value heap"
    );
}
