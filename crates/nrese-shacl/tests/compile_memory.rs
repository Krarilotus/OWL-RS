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

/// A validation that its envelope can't afford stops before it holds its
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

/// A SELECT constraint's solutions are made results as they are read,
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

/// Comparing a change of the shapes copies nothing that a shape's
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

/// An `rdf:XMLLiteral` nested 200,000 elements deep is well-balanced XML: `sh:datatype
/// rdf:XMLLiteral` classifies it (no result) and the same with one end tag missing (one
/// result), on the validation's ordinary threads. Run alone, in a process of its own: a
/// classifier that recursed per element would overflow its stack, which ends the process
/// and fails this test.
#[test]
fn a_deeply_nested_xml_literal_is_classified() {
    if !measure::alone() {
        return;
    }
    let depth = 200_000;
    for (row, closed, results) in [("balanced", depth, 0), ("an end tag missing", depth - 1, 1)] {
        let xml = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(closed));
        let turtle = format!(
            "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
             ex:a ex:v \"{xml}\"^^rdf:XMLLiteral .
             ex:S a sh:NodeShape ; sh:targetNode ex:a ;
               sh:property [ sh:path ex:v ; sh:datatype rdf:XMLLiteral ] ."
        );
        let snapshot = load(&turtle);
        let graph = snapshot
            .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
            .expect("the shapes graph");
        let shapes =
            compile(&snapshot, Selection::asserted(GraphSelector::Exact(graph))).expect("shapes");
        // The data is the same graph as the shapes.
        let data = Selection::of(GraphSelector::Exact(graph));
        let report = nrese_shacl::validate(
            &snapshot,
            &shapes,
            data,
            &nrese_sparql::Resources::default(),
        )
        .expect("nothing stops it");
        assert!(report.failures.is_empty(), "{row}: {:?}", report.failures);
        assert_eq!(report.results.len(), results, "{row}");
    }
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

/// A message template's text is charged before it is made. 2000 `{?x}` blocks naming a
/// 4 KiB value would fill in to 8 MB of text. Under a 1 MiB envelope the validation is
/// refused, and what it held at most stays below half of that text, so a run that made the
/// text before charging it fails here.
#[test]
fn a_refused_message_template_holds_less_than_its_text() {
    if !measure::alone() {
        return;
    }
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:p \"{}\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:message \"{}\" ;
           sh:select \"SELECT $this ?x WHERE {{ $this <http://example.com/p> ?x }}\" ] .",
        "v".repeat(4096),
        "{?x}".repeat(2000)
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
    eprintln!("refused template validation held at most {bytes} bytes: {outcome:?}");
    assert!(
        matches!(
            outcome,
            Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{outcome:?}"
    );
    assert!(bytes < 4 << 20, "held {bytes} bytes");
}

/// A filled message's dictionary lookup is charged before it runs. 96 `{?x}` blocks naming
/// a 4 KiB value fill in to 384 KiB, whose lookup copies the text into a key grown by
/// doubling. Under a 1 MiB envelope the validation makes the text (its charge fits) and is
/// refused before the lookup allocates the key: it holds the text and little else, well
/// under the text beside its key, so a run that charged less than the lookup takes (and
/// made both) fails here.
#[test]
fn a_refused_message_lookup_holds_less_than_its_text_and_key() {
    if !measure::alone() {
        return;
    }
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:p \"{}\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:message \"{}\" ;
           sh:select \"SELECT $this ?x WHERE {{ $this <http://example.com/p> ?x }}\" ] .",
        "v".repeat(4096),
        "{?x}".repeat(96)
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
    eprintln!("refused lookup validation held at most {bytes} bytes: {outcome:?}");
    assert!(
        matches!(
            outcome,
            Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{outcome:?}"
    );
    assert!(bytes < 640 << 10, "held {bytes} bytes");
}

/// A message the query binds comes first (SHACL §5.3.2), so the constraint's templates are
/// not filled and the values they name are not copied. The solution's 8 MiB `?x`, named by
/// a `{?x}` template, is held once while the row is read: the validation holds well under
/// two copies of it, so a run that copied it for the unused template fails here.
#[test]
fn a_message_the_query_binds_copies_no_value_the_templates_name() {
    if !measure::alone() {
        return;
    }
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:p \"{}\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:message \"{{?x}}\" ;
           sh:select \"SELECT $this ?x (\\\"bound\\\" AS ?message) WHERE {{ $this <http://example.com/p> ?x }}\" ] .",
        "v".repeat(8 << 20)
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
    let (outcome, bytes) = measure::measure(|| {
        nrese_shacl::validate(&snapshot, &shapes, data, &resources)
            .map(|report| report.results.len())
    });
    eprintln!("bound-message validation held at most {bytes} bytes: {outcome:?}");
    assert!(matches!(outcome, Ok(1)), "{outcome:?}");
    assert!(bytes < 12 << 20, "held {bytes} bytes");
}

/// A filled message's language tag is charged with its text, before either is made. A
/// `{?x}` template tagged with a 2 MiB private-use language tag, under a 1 MiB envelope, is
/// refused before the tag is copied: the validation holds well under the tag, so a run that
/// charged the text alone (and copied the tag) fails here.
#[test]
fn a_refused_message_holds_less_than_its_language_tag() {
    if !measure::alone() {
        return;
    }
    let turtle = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:a ex:p \"v\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:message \"{{?x}}\"@x{} ;
           sh:select \"SELECT $this ?x WHERE {{ $this <http://example.com/p> ?x }}\" ] .",
        "-abcdefgh".repeat(233_017)
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
    eprintln!("refused tagged validation held at most {bytes} bytes: {outcome:?}");
    assert!(
        matches!(
            outcome,
            Err(nrese_sparql::QueryEvaluationError::MemoryLimit(_))
        ),
        "{outcome:?}"
    );
    assert!(bytes < 1 << 20, "held {bytes} bytes");
}

/// A change's validation looks the filled message of the state before up in the state
/// after's dictionary (its frontier), charged by its bound before: the frontier code both
/// `validate_changes` and `validate_shapes_change` use. The state after is a transaction's
/// pending state, as every caller passes it: its lookup is the shared dictionary's, on a
/// compressed base (assumed: the checkpoint maps it so, and a failed mapping would only be
/// logged). The message is 1 MiB, the same before and after the change, and neither state
/// stores it. What the validation holds beyond its charge stays under 64 KiB (a few KiB
/// measured).
///
/// What this catches and what it doesn't: the message compresses well, so its lookup holds
/// about 4 × its length; a charge below that fails here, one between it and the engine's
/// bound for this path (7 × the encoded length) does not. Not measured: a message the
/// compressor makes longer than itself, and a private view's overlay, which makes its own
/// key as well (no caller validates a change against one). Charge reserved but not yet
/// spent when the lookup runs could hide an undercount; here there is next to none (a
/// bound of the message's length alone holds 3 × its length beyond its charge).
#[test]
fn a_message_looked_up_through_the_frontier_is_charged_for_its_lookup() {
    if !measure::alone() {
        return;
    }
    /// The test's own data directory, removed when the test ends, also by a panic.
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Dir(std::env::temp_dir().join(format!(
        "nrese-shacl-frontier-measure-{}",
        std::process::id()
    )));
    // A directory left by a killed run of the same process id is not this test's store.
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).expect("the test's directory");
    nrese_engine::set_vocabulary_encoding(nrese_engine::VocabularyEncoding::Fsst);
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.0, config).expect("engine");
    let shapes = format!(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:message \"{}{{?x}}\" ;
           sh:select \"SELECT $this ?x WHERE {{ $this <http://example.com/p> ?x }}\" ] .",
        "m".repeat(1 << 20)
    );
    let insert = |tx: &mut nrese_engine::Transaction<'_>, turtle: &str, graph: GraphNameRef<'_>| {
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
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
    let data_turtle = |turtle: &str| format!("@prefix ex: <http://example.com/> . {turtle}");
    let mut tx = engine.transaction();
    insert(&mut tx, &shapes, shapes_graph);
    insert(
        &mut tx,
        &data_turtle("ex:a ex:p \"v\" ."),
        GraphNameRef::DefaultGraph,
    );
    tx.commit().expect("commit");
    // The dictionary's base, compressed from here on if its mapping succeeds (a failure is
    // only logged).
    engine.checkpoint().expect("checkpoint");
    let before = engine.snapshot();
    let graph = before
        .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
        .expect("the shapes graph");
    let shapes =
        compile(&before, Selection::asserted(GraphSelector::Exact(graph))).expect("shapes");
    // A change of the focus node that keeps its result, and so its message.
    let mut tx = engine.transaction();
    insert(
        &mut tx,
        &data_turtle("ex:a ex:q \"w\" ."),
        GraphNameRef::DefaultGraph,
    );
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    let data = Selection::of(GraphSelector::Exact(nrese_engine::TermId::DEFAULT_GRAPH));
    // The state after has the result and its message, so the state before's is looked up.
    let report = nrese_shacl::validate(&after, &shapes, data, &nrese_sparql::Resources::default())
        .expect("the state after's validation");
    let lengths: Vec<_> = report
        .results
        .iter()
        .map(|result| {
            result
                .messages
                .iter()
                .map(|m| m.value().len())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(lengths, [[(1 << 20) + 1]], "the state after's results");
    let resources = nrese_sparql::Resources::default();
    let (outcome, excess) = measure::measure_within(&resources.memory, || {
        nrese_shacl::validate_changes(&before, &after, &shapes, data, &changed, &resources)
            .map(|report| report.results.len())
    });
    eprintln!("a change's validation held at most {excess} bytes beyond its charge: {outcome:?}");
    assert!(matches!(outcome, Ok(0)), "{outcome:?}");
    assert!(excess < 64 << 10, "held {excess} bytes beyond its charge");
}
