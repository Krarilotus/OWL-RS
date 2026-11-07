//! Guard of the shapes compiler's memory (SHCL-AMP): a SPARQL-based constraint component
//! constrains a shape once per combination of the shape's values for its parameters (SHACL
//! §6.2), and compiling it holds memory linear in the shapes graph, not in the number of
//! combinations, which grows as a power of it. Measured (`nrese_exec::measure`), so this
//! file is a test binary of its own.

use nrese_engine::{Engine, EngineConfig, GraphSelector, Snapshot};
use nrese_exec::measure;
use nrese_rdf::{GraphNameRef, NamedNodeRef, QuadRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Selection, compile};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

const SHAPES: &str = "http://example.com/shapes";

/// A shapes graph with a component of three parameters and a node shape giving each of them
/// `m` values: m^3 combinations from about 3m statements. Made before any measurement.
fn snapshot(m: usize) -> Snapshot {
    let values = |parameter: &str| {
        (0..m)
            .map(|i| format!("ex:{parameter} {i}"))
            .collect::<Vec<_>>()
            .join(" ; ")
    };
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:C a sh:ConstraintComponent ;
           sh:parameter [ sh:path ex:p1 ] , [ sh:path ex:p2 ] , [ sh:path ex:p3 ] ;
           sh:validator [ a sh:SPARQLAskValidator ;
             sh:ask \"ASK {{ FILTER ($p1 != $p2 || $p2 != $p3) }}\" ] .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; {} ; {} ; {} .",
        values("p1"),
        values("p2"),
        values("p3")
    );
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let mut tx = engine.transaction();
    let graph = NamedNodeRef::new_unchecked(SHAPES);
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let quad = quad.expect("turtle");
        tx.insert(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            GraphNameRef::from(graph),
        ));
    }
    tx.commit().expect("commit");
    engine.snapshot()
}

/// Doubling the values of each parameter at most doubles what compiling the shapes holds at
/// once (it adds as many statements); built once per combination, it would hold eight times
/// as much.
#[test]
fn compiling_a_component_holds_memory_linear_in_its_parameter_values() {
    if !measure::alone() {
        return;
    }
    let peak = |m: usize| {
        let snapshot = snapshot(m);
        let graph = snapshot
            .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
            .expect("the shapes graph");
        let selection = Selection::asserted(GraphSelector::Exact(graph));
        // Once outside the measurement: what reading builds lazily is the snapshot's.
        drop(compile(&snapshot, selection).expect("shapes"));
        let (shapes, bytes) = measure::measure(|| compile(&snapshot, selection).expect("shapes"));
        drop(shapes);
        bytes
    };
    let (small, large) = (peak(8), peak(16));
    assert!(
        large <= 2 * small,
        "doubling each parameter's values took compiling from {small} to {large} bytes"
    );
}
