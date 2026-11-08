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
    load(&turtle)
}

/// A snapshot with `turtle` in the shapes graph. Made before any measurement.
fn load(turtle: &str) -> Snapshot {
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

/// SHCL-AMP r1 R1: a validation that its envelope can't afford stops before it holds its
/// report. Four parameters with eight values each and an ASK that every combination fails
/// give 8^4 = 4096 results, each decoding the shape's 4 KiB message: 16 MiB of message text
/// alone. Under a 1 MiB envelope the validation is refused, and what it held at most stays
/// below half of that text, so a run that decoded the report fails here.
#[test]
fn a_refused_validation_holds_less_than_its_report() {
    if !measure::alone() {
        return;
    }
    let values: Vec<String> = (0..8).map(|v| v.to_string()).collect();
    let values = values.join(" , ");
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:C a sh:ConstraintComponent ;
           sh:parameter [ sh:path ex:p0 ] , [ sh:path ex:p1 ] , [ sh:path ex:p2 ] , [ sh:path ex:p3 ] ;
           sh:validator [ a sh:SPARQLAskValidator ; sh:ask \"ASK {{ FILTER(false) }}\" ] .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:message \"{}\" ;
           ex:p0 {values} ; ex:p1 {values} ; ex:p2 {values} ; ex:p3 {values} .",
        "x".repeat(4096)
    );
    let snapshot = load(&turtle);
    let graph = snapshot
        .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
        .expect("the shapes graph");
    let shapes =
        compile(&snapshot, Selection::asserted(GraphSelector::Exact(graph))).expect("shapes");
    let data = Selection::of(GraphSelector::Exact(nrese_engine::TermId::DEFAULT_GRAPH));
    let resources = nrese_sparql::Resources {
        memory: nrese_exec::Envelope::new(Some(1 << 20), None),
        ..nrese_sparql::Resources::default()
    };
    let (outcome, bytes) = measure::measure(|| {
        nrese_shacl::validate(&snapshot, &shapes, data, &resources)
            .map(|report| report.results.len())
    });
    eprintln!("refused validation held at most {bytes} bytes: {outcome:?}");
    assert!(
        matches!(
            outcome,
            Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{outcome:?}"
    );
    assert!(bytes < 8 << 20, "held {bytes} bytes");
}

/// SHCL-AMP R1 sol r1 S2: a SELECT constraint's solutions are made results as they are read,
/// not kept decoded first. 16 × 16 solutions each bind ?value to one stored 64 KiB literal:
/// 16 MiB of decoded values if they were kept. Under a 1 MiB envelope the validation is
/// refused, and what it held at most stays below half of that.
#[test]
fn a_refused_select_validation_holds_less_than_its_solutions() {
    if !measure::alone() {
        return;
    }
    let rows: Vec<String> = (0..16).map(|i| format!("ex:s{i} ex:q ex:o{i} .")).collect();
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:big \"{}\" . {}
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select \"\"\"
           SELECT $this ?value WHERE {{
             $this <http://example.com/big> ?value .
             ?x <http://example.com/q> ?o . ?y <http://example.com/q> ?p }}\"\"\" ] .",
        "x".repeat(64 << 10),
        rows.join(" ")
    );
    let snapshot = load(&turtle);
    let graph = snapshot
        .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
        .expect("the shapes graph");
    let shapes =
        compile(&snapshot, Selection::asserted(GraphSelector::Exact(graph))).expect("shapes");
    // The data is the same graph as the shapes.
    let data = Selection::of(GraphSelector::Exact(graph));
    let resources = nrese_sparql::Resources {
        memory: nrese_exec::Envelope::new(Some(1 << 20), None),
        ..nrese_sparql::Resources::default()
    };
    let (outcome, bytes) = measure::measure(|| {
        nrese_shacl::validate(&snapshot, &shapes, data, &resources)
            .map(|report| report.results.len())
    });
    eprintln!("refused SELECT validation held at most {bytes} bytes: {outcome:?}");
    assert!(
        matches!(
            outcome,
            Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{outcome:?}"
    );
    assert!(bytes < 8 << 20, "held {bytes} bytes");
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
