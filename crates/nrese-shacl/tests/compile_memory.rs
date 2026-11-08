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

/// SYN-56 review r1 F1: comparing a change of the shapes copies nothing that a shape's
/// results share. 2048 subjects of ex:p each lack ex:required, against a property shape with
/// 1024 messages: copied into each earlier result's comparison key, the messages' ids alone
/// would take 2048 × 1024 × 8 bytes = 16 MiB (decoded into each result, far more). An
/// annotation added to the shape changes no result, and the comparison holds at most a
/// quarter of those copies.
#[test]
fn a_shapes_change_holds_less_than_its_results_messages() {
    if !measure::alone() {
        return;
    }
    let data: String = (0..2048)
        .map(|i| format!("ex:s{i} ex:p ex:o .\n"))
        .collect();
    let messages: Vec<String> = (0..1024).map(|i| format!("\"m{i}\"")).collect();
    let shapes = format!(
        "ex:S a sh:PropertyShape ; sh:targetSubjectsOf ex:p ; sh:path ex:required ;
           sh:minCount 1 ; sh:message {} .",
        messages.join(" , ")
    );
    let prefixes = "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .";
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let insert = |tx: &mut nrese_engine::Transaction<'_>, turtle: &str, graph: GraphNameRef<'_>| {
        for quad in RdfParser::from_format(RdfFormat::Turtle)
            .for_slice(format!("{prefixes}{turtle}").as_bytes())
        {
            let quad = quad.expect("turtle");
            tx.insert(QuadRef::new(
                &quad.subject,
                &quad.predicate,
                &quad.object,
                graph,
            ));
        }
    };
    let shapes_graph = GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES));
    let mut tx = engine.transaction();
    insert(&mut tx, &data, GraphNameRef::DefaultGraph);
    insert(&mut tx, &shapes, shapes_graph);
    tx.commit().expect("commit");
    let before = engine.snapshot();
    let graph = before
        .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
        .expect("the shapes graph");
    let selection = Selection::asserted(GraphSelector::Exact(graph));
    let shapes_before = compile(&before, selection).expect("shapes");
    let mut tx = engine.transaction();
    insert(&mut tx, "ex:S rdfs:comment \"annotated\" .", shapes_graph);
    let after = tx.pending_snapshot();
    let shapes_after = compile(&after, selection).expect("shapes");
    let data = Selection::of(GraphSelector::Exact(nrese_engine::TermId::DEFAULT_GRAPH));
    let resources = nrese_sparql::Resources::default();
    let (outcome, bytes) = measure::measure(|| {
        nrese_shacl::validate_shapes_change(
            &before,
            Some(&shapes_before),
            &after,
            &shapes_after,
            data,
            &resources,
        )
        .map(|report| report.results.len())
    });
    eprintln!("a shapes change of 2048 results held at most {bytes} bytes: {outcome:?}");
    assert!(matches!(outcome, Ok(0)), "{outcome:?}");
    assert!(bytes < 4 << 20, "held {bytes} bytes");
}

/// SYN-56 B: a report holds the terms a query computed once. 64 solutions each bind ?value to
/// a computed 64 KiB literal (one stored literal and a suffix: no statement holds it), so the
/// report's values are 4 MiB of text. Decoding moves each value from its result into the
/// report: the validation holds at most 6 MiB, where a copy beside the results' would hold
/// more than 8 MiB.
#[test]
fn a_report_holds_its_computed_terms_once() {
    if !measure::alone() {
        return;
    }
    let rows: Vec<String> = (0..64).map(|i| format!("ex:s{i} ex:q ex:o{i} .")).collect();
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:big \"{}\" . {}
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select \"\"\"
           SELECT $this (CONCAT(?big, \"!\") AS ?value) WHERE {{
             $this <http://example.com/big> ?big .
             ?x <http://example.com/q> ?o }}\"\"\" ] .",
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
    let resources = nrese_sparql::Resources::default();
    // Once outside the measurement: what reading builds lazily is the snapshot's.
    drop(nrese_shacl::validate(&snapshot, &shapes, data, &resources).expect("affordable"));
    let (outcome, bytes) = measure::measure(|| {
        nrese_shacl::validate(&snapshot, &shapes, data, &resources).map(|report| {
            let long = report
                .results
                .iter()
                .filter(|result| {
                    matches!(&result.value, Some(nrese_rdf::Term::Literal(value))
                        if value.value().len() == (64 << 10) + 1)
                })
                .count();
            (report.results.len(), long)
        })
    });
    eprintln!("a report of 64 computed 64 KiB values held at most {bytes} bytes: {outcome:?}");
    assert!(matches!(outcome, Ok((64, 64))), "{outcome:?}");
    assert!(bytes < 6 << 20, "held {bytes} bytes");
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
