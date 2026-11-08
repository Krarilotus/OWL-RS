//! The SHACL commit gate (C2): a commit that introduces results is rejected (enforce) or
//! only reported (report); data that was invalid before doesn't block other commits.

use std::io::Write as _;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, CancellationToken, DEFAULT_SHAPES_GRAPH, GateSeverity, GraphTarget,
    MutationCommand, MutationError, MutationPipeline, MutationTicket, QueryEvaluationError,
    ReadScope, ShaclGate, ShaclValidationRequest, SparqlUpdateRequest, StoreConfig, StoreError,
    StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#> ";

fn pipeline(gate: ShaclGate) -> MutationPipeline {
    pipeline_with(StoreConfig {
        shacl_gate: gate,
        ..StoreConfig::in_memory()
    })
}

fn pipeline_with(config: StoreConfig) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    )
}

fn update(pipeline: &MutationPipeline, body: &str) -> Result<(), MutationError> {
    let command = MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{body}")));
    pipeline
        .apply(
            command,
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .map(drop)
}

fn holds(pipeline: &MutationPipeline, pattern: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("{PREFIXES} ASK {{ {pattern} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

/// Persons need a name; their friends must be persons (a nested path and class).
fn add_shapes(pipeline: &MutationPipeline) {
    update(
        pipeline,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:Person a sh:NodeShape ; sh:targetClass ex:Person ;
                  sh:property [ sh:path ex:name ; sh:minCount 1 ] ;
                  sh:property [ sh:path ex:friend ; sh:class ex:Person ] .
            }} }}"
        ),
    )
    .expect("shapes that no data violates are accepted");
}

#[test]
fn enforce_rejects_what_a_commit_introduces_and_changes_nothing() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    add_shapes(&pipeline);
    let rejected = update(&pipeline, "INSERT DATA { ex:a a ex:Person }");
    assert!(
        matches!(rejected, Err(MutationError::Rejected(_))),
        "{rejected:?}"
    );
    assert!(!holds(&pipeline, "ex:a a ex:Person"));
    update(
        &pipeline,
        "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }",
    )
    .expect("valid");
    assert!(holds(&pipeline, "ex:a ex:name \"A\""));
    // A change elsewhere along a path: ex:b becomes a friend that isn't a person.
    let rejected = update(&pipeline, "INSERT DATA { ex:a ex:friend ex:b }");
    assert!(matches!(rejected, Err(MutationError::Rejected(_))));
    update(
        &pipeline,
        "INSERT DATA { ex:a ex:friend ex:b . ex:b a ex:Person ; ex:name \"B\" }",
    )
    .expect("valid together");
    // Removing what makes it valid is rejected too.
    let rejected = update(&pipeline, "DELETE DATA { ex:b ex:name \"B\" }");
    assert!(matches!(rejected, Err(MutationError::Rejected(_))));
    assert!(holds(&pipeline, "ex:b ex:name \"B\""));
}

#[test]
fn data_that_was_invalid_before_does_not_block_other_commits() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    add_shapes(&pipeline);
    // A bulk load doesn't pass the gate: ex:old is a person without a name.
    let mut file = tempfile::Builder::new()
        .suffix(".nt")
        .tempfile()
        .expect("file");
    writeln!(
        file,
        "<http://example.com/old> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Person> ."
    )
    .expect("write");
    pipeline
        .store()
        .bulk_load(&BulkLoadRequest {
            files: vec![file.path().to_path_buf()],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: Some(nrese_store::InvalidStatements::Strict),
        })
        .expect("load");
    update(
        &pipeline,
        "INSERT DATA { ex:new a ex:Person ; ex:name \"N\" }",
    )
    .expect("an unrelated commit");
    update(
        &pipeline,
        "INSERT DATA { ex:old ex:note \"still unnamed\" }",
    )
    .expect("a commit on the invalid node that doesn't add a result");
}

#[test]
fn report_and_warning_level_gates_let_commits_through() {
    let reporting = pipeline(ShaclGate::Report);
    add_shapes(&reporting);
    update(&reporting, "INSERT DATA { ex:a a ex:Person }").expect("reported, not rejected");
    assert!(holds(&reporting, "ex:a a ex:Person"));

    // A warning (the severity of the shape that reports it) passes a gate that enforces
    // violations only.
    let enforcing = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    update(
        &enforcing,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:Soft a sh:NodeShape ; sh:targetClass ex:Thing ;
                  sh:property [ sh:path ex:label ; sh:minCount 1 ; sh:severity sh:Warning ] .
            }} }}"
        ),
    )
    .expect("shapes");
    update(&enforcing, "INSERT DATA { ex:t a ex:Thing }").expect("a warning only");
    let strict = pipeline(ShaclGate::Enforce(GateSeverity::Warning));
    update(
        &strict,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:Soft a sh:NodeShape ; sh:targetClass ex:Thing ;
                  sh:property [ sh:path ex:label ; sh:minCount 1 ; sh:severity sh:Warning ] .
            }} }}"
        ),
    )
    .expect("shapes");
    assert!(matches!(
        update(&strict, "INSERT DATA { ex:t a ex:Thing }"),
        Err(MutationError::Rejected(_))
    ));
}

#[test]
fn shapes_that_existing_data_violates_are_rejected() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    update(&pipeline, "INSERT DATA { ex:a a ex:Person }").expect("no shapes yet");
    let rejected = update(
        &pipeline,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:Person a sh:NodeShape ; sh:targetClass ex:Person ;
                  sh:property [ sh:path ex:name ; sh:minCount 1 ] .
            }} }}"
        ),
    );
    assert!(matches!(rejected, Err(MutationError::Rejected(_))));
}

/// RSH-07: the gate validates on the write's resources, its SHACL-SPARQL queries included. A
/// query past the write's memory (the server's query memory, 64 KiB here: forty subjects of
/// ex:p make a 64,000-row product) stops the validation, and the write fails with that
/// memory error, under a gate that only reports too: a stopped validation is no verdict to
/// commit past. Nothing is committed.
#[test]
fn a_validation_past_the_writes_memory_fails_the_write_under_any_gate() {
    let pipeline = pipeline_with(StoreConfig {
        shacl_gate: ShaclGate::Report,
        total_query_memory_bytes: 64 * 1024,
        ..StoreConfig::in_memory()
    });
    let wide = "SELECT $this ?value ?b ?c WHERE { ?value <http://example.com/p> ?x .
        ?b <http://example.com/p> ?y . ?c <http://example.com/p> ?z }";
    update(
        &pipeline,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:Wide a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''{wide}''' ] .
            }} }}"
        ),
    )
    .expect("without data the query is empty");
    let data: String = (0..40).map(|i| format!("ex:s{i} ex:p ex:o . ")).collect();
    let refused = update(&pipeline, &format!("INSERT DATA {{ {data} }}"));
    assert!(
        matches!(&refused, Err(MutationError::Store { source, .. }) if source.is_memory_limit()),
        "{refused:?}"
    );
    assert!(!holds(&pipeline, "ex:s0 ex:p ex:o"));
}

/// Loads `ntriples` into the shapes graph without passing the gate (a bulk load doesn't).
fn load_shapes(pipeline: &MutationPipeline, ntriples: &str) {
    let mut file = tempfile::Builder::new()
        .suffix(".nt")
        .tempfile()
        .expect("file");
    writeln!(file, "{ntriples}").expect("write");
    pipeline
        .store()
        .bulk_load(&BulkLoadRequest {
            files: vec![file.path().to_path_buf()],
            replace: false,
            graph: GraphTarget::NamedGraph(DEFAULT_SHAPES_GRAPH.to_owned()),
            invalid: Some(nrese_store::InvalidStatements::Strict),
        })
        .expect("load");
}

/// SHACL §1.5: a shapes graph with `sh:entailment E` the processor doesn't support makes
/// every validation fail, a shapes graph with nothing else included (SYN-56 r1 S2): the
/// enforcing gate rejects a write, though no shape constrains anything.
#[test]
fn a_shapes_graph_asking_for_an_unsupported_entailment_fails_the_gate() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    load_shapes(
        &pipeline,
        "<http://example.com/S> <http://www.w3.org/ns/shacl#entailment> <http://www.w3.org/ns/entailment/RDFS> .",
    );
    let rejected = update(&pipeline, "INSERT DATA { ex:a ex:name \"A\" }");
    assert!(
        matches!(rejected, Err(MutationError::Rejected(_))),
        "{rejected:?}"
    );
    assert!(!holds(&pipeline, "ex:a ex:name \"A\""));
}

/// SYN-56 review r1 F3, the gate's side: a shapes graph that asks for an unsupported
/// entailment, and holds nothing else, can be removed under the enforcing gate: the commit
/// that clears it leaves no failure, and the state before's failures aren't the commit's.
#[test]
fn a_shapes_graph_asking_for_an_unsupported_entailment_can_be_removed() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    load_shapes(
        &pipeline,
        "<http://example.com/S> <http://www.w3.org/ns/shacl#entailment> <http://www.w3.org/ns/entailment/RDFS> .",
    );
    update(&pipeline, &format!("CLEAR GRAPH <{DEFAULT_SHAPES_GRAPH}>"))
        .expect("clearing the shapes graph is accepted");
    update(&pipeline, "INSERT DATA { ex:a ex:name \"A\" }").expect("then writes are accepted");
}

/// SS-22: a validation request runs on its caller's cancellation (the server cancels it at
/// its deadline and when the client is gone): cancelled, it stops with that, not a report.
#[test]
fn a_cancelled_validation_request_stops() {
    let pipeline = pipeline(ShaclGate::Off);
    add_shapes(&pipeline);
    update(&pipeline, "INSERT DATA { ex:a a ex:Person }").expect("no gate");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let outcome = pipeline.store().validate_shacl(
        &ReadScope::All,
        &ShaclValidationRequest::default(),
        &cancellation,
    );
    assert!(
        matches!(
            outcome,
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::Cancelled
            ))
        ),
        "{outcome:?}"
    );
}
