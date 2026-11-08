//! Shapes sent with a validation request (`ShapesSource::Payload`) are the request's alone,
//! whatever the store holds (SYN-58):
//!
//! - `ShapesSource`: "Shapes sent with the request" are the shapes validated against, so a
//!   shape stored in the repository, in whatever graph, is not one of them;
//! - `ValidatedGraphs::AllData`: "every graph, except the graphs that hold shapes", so a
//!   stored graph that holds no request shape is validated, whatever its name.
//!
//! The names at risk are the graph the store used for request shapes,
//! `https://nrese.dev/ns/graph#shacl-request-shapes`, and the ones after it. Every row is
//! checked and all that fail are reported; guards are labelled.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    CancellationToken, GraphResultFormat, MutationCommand, MutationPipeline, MutationTicket,
    ReadScope, ShaclValidation, ShaclValidationRequest, ShapesSource, SparqlUpdateRequest,
    StoreConfig, ValidatedGraphs,
};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#> ";
const REQUEST_GRAPH: &str = "https://nrese.dev/ns/graph#shacl-request-shapes";

/// The request's shapes: persons need a name.
const PAYLOAD: &str = "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
ex:Person a sh:NodeShape ; sh:targetClass ex:Person ; sh:property [ sh:path ex:name ; sh:minCount 1 ] .";

/// The same shapes, every node named: a payload's blank nodes are fresh on each request,
/// and interned like any term of a speculative transaction (the open row "a dropped
/// transaction does not undo dictionary interning", owner engine/transaction), so only a
/// payload without them can show what the request graph itself costs.
const NAMED_PAYLOAD: &str =
    "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
ex:Person a sh:NodeShape ; sh:targetClass ex:Person ; sh:property ex:Named .
ex:Named sh:path ex:name ; sh:minCount 1 .";

/// A stored shape no data conforms to: `ex:a` needs an age it doesn't have.
const STORED_SHAPE: &str = "ex:Aged a sh:NodeShape ; sh:targetNode ex:a ; sh:property [ sh:path ex:age ; sh:minCount 1 ] .";

/// The data in the default graph: a person with a name, conforming to the request's shapes.
const DATA: &str = "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }";

fn pipeline() -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(nrese_store::StoreService::new(StoreConfig::in_memory()).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    )
}

fn update(pipeline: &MutationPipeline, body: &str) {
    let command = MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{body}")));
    pipeline
        .apply(
            command,
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("update");
}

/// A store with `DATA`, and `stored` (Turtle statements) in each of `graphs`.
fn store(graphs: &[&str], stored: &str) -> MutationPipeline {
    let pipeline = pipeline();
    update(&pipeline, DATA);
    for graph in graphs {
        update(
            &pipeline,
            &format!("INSERT DATA {{ GRAPH <{graph}> {{ {stored} }} }}"),
        );
    }
    pipeline
}

fn validate(pipeline: &MutationPipeline, graphs: ValidatedGraphs) -> ShaclValidation {
    validate_with(pipeline, PAYLOAD, graphs)
}

fn validate_with(
    pipeline: &MutationPipeline,
    payload: &str,
    graphs: ValidatedGraphs,
) -> ShaclValidation {
    pipeline
        .store()
        .validate_shacl(
            &ReadScope::All,
            &ShaclValidationRequest {
                shapes: ShapesSource::Payload {
                    format: GraphResultFormat::Turtle,
                    base_iri: None,
                    payload: payload.as_bytes().to_vec(),
                },
                graphs,
                ..ShaclValidationRequest::default()
            },
            &CancellationToken::new(),
        )
        .expect("validated")
}

/// (guard, case, graphs seeded, statements in them, validated graphs, focus nodes)
type Row<'a> = (
    bool,
    &'a str,
    Vec<&'a str>,
    &'a str,
    ValidatedGraphs,
    Vec<String>,
);

/// The report's focus nodes, sorted and each once, and the shapes counted.
fn outcome(validation: &ShaclValidation) -> (Vec<String>, usize) {
    let mut focus: Vec<String> = validation
        .report
        .results
        .iter()
        .map(|result| result.focus_node.to_string())
        .collect();
    focus.sort();
    focus.dedup();
    (focus, validation.shapes)
}

#[test]
fn request_shapes_are_the_requests_alone_whatever_the_store_holds() {
    // The same payload on a store holding nothing in those graphs: conforming, and its own
    // shapes counted.
    let reference = outcome(&validate(&store(&[], ""), ValidatedGraphs::AllData));
    assert_eq!(reference.0, Vec::<String>::new(), "the data conforms");
    let shapes = reference.1;
    assert!(shapes > 0, "the payload has shapes");
    let request_1 = format!("{REQUEST_GRAPH}-1");
    let b_without_name = "ex:b a ex:Person .".to_owned();
    let both = format!("{STORED_SHAPE} {b_without_name}");
    let b = vec!["<http://example.com/b>".to_owned()];
    let mut failed = Vec::new();
    let rows: [Row<'_>; 5] = [
        // S1: a failing shape stored in the request graph isn't the request's.
        (
            false,
            "S1 stored shape, all data",
            vec![REQUEST_GRAPH],
            STORED_SHAPE,
            ValidatedGraphs::AllData,
            vec![],
        ),
        // S2: data stored in the request graph is data, and is validated.
        (
            false,
            "S2 stored data, all data",
            vec![REQUEST_GRAPH],
            b_without_name.as_str(),
            ValidatedGraphs::AllData,
            b.clone(),
        ),
        // S3: the same when the next name is taken too.
        (
            false,
            "S3 both names hold shape and data, all data",
            vec![REQUEST_GRAPH, request_1.as_str()],
            both.as_str(),
            ValidatedGraphs::AllData,
            b.clone(),
        ),
        // S8: the request graph validated by name: its statements are data, its shape
        // statement no shape of the request.
        (
            false,
            "S8 shape and data, that graph by name",
            vec![REQUEST_GRAPH],
            both.as_str(),
            ValidatedGraphs::Named(REQUEST_GRAPH.to_owned()),
            b.clone(),
        ),
        // A name after the free one holds statements: no effect.
        (
            true,
            "a later name holds shape and data, all data",
            vec![request_1.as_str()],
            both.as_str(),
            ValidatedGraphs::AllData,
            b.clone(),
        ),
    ];
    for (guard, case, graphs, stored, validated, focus) in rows {
        let got = outcome(&validate(&store(&graphs, stored), validated));
        let want = (focus, shapes);
        if got != want {
            let kind = if guard { "guard " } else { "" };
            failed.push(format!("{kind}{case}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} row(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// S10: a graph validated by name holds its stored statements only, never the request's
/// shapes, even when it has the name the shapes would otherwise go into (sol-loop SYN-58
/// r1 S1). A shape that targets shapes (`sh:targetClass sh:NodeShape`) and fails on every
/// one would find itself if it were data.
#[test]
fn a_graph_validated_by_name_never_holds_the_request_shapes() {
    let self_targeting = "@prefix sh: <http://www.w3.org/ns/shacl#> .
<urn:S> a sh:NodeShape ; sh:targetClass sh:NodeShape ; sh:class <urn:Required> .";
    let request_1 = format!("{REQUEST_GRAPH}-1");
    let mut failed = Vec::new();
    // (guard, case, graphs seeded with a statement, graph validated by name)
    let rows: [(bool, &str, Vec<&str>, &str); 2] = [
        (
            false,
            "the first name, nothing stored",
            vec![],
            REQUEST_GRAPH,
        ),
        (
            true,
            "the second name, the first taken",
            vec![REQUEST_GRAPH],
            request_1.as_str(),
        ),
    ];
    for (guard, case, graphs, validated) in rows {
        let pipeline = store(&graphs, "<urn:a> <urn:p> <urn:o> .");
        let got = outcome(&validate_with(
            &pipeline,
            self_targeting,
            ValidatedGraphs::Named(validated.to_owned()),
        ))
        .0;
        if !got.is_empty() {
            let kind = if guard { "guard " } else { "" };
            failed.push(format!("{kind}{case}: shapes validated as data: {got:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// S4 (guard): the graph a request's shapes go into is the same on a repeated request, so it
/// interns nothing more, even when the request graph's first name is taken. (Shapes with
/// blank nodes intern fresh ones each time: see `NAMED_PAYLOAD`.)
#[test]
fn a_repeated_request_interns_nothing_more() {
    let mut failed = Vec::new();
    for graphs in [vec![], vec![REQUEST_GRAPH]] {
        let pipeline = store(&graphs, STORED_SHAPE);
        let terms = || pipeline.store().engine_stats().dictionary.terms;
        validate_with(&pipeline, NAMED_PAYLOAD, ValidatedGraphs::AllData);
        let first = terms();
        validate_with(&pipeline, NAMED_PAYLOAD, ValidatedGraphs::AllData);
        let second = terms();
        if first != second {
            failed.push(format!("guard {graphs:?}: {first} terms, then {second}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
