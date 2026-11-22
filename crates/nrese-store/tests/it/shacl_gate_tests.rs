//! The SHACL commit gate (C2): a commit that introduces results is rejected (enforce) or
//! only reported (report); data that was invalid before doesn't block other commits.

use std::io::Write as _;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, CancellationToken, DEFAULT_SHAPES_GRAPH, GateSeverity, GraphTarget,
    MutationCommand, MutationError, MutationPipeline, MutationReject, MutationTicket,
    QueryEvaluationError, ReadScope, ShaclGate, ShaclValidationRequest, SparqlUpdateRequest,
    StoreConfig, StoreError, StoreService,
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

fn refuse(pipeline: &MutationPipeline, update_text: &str) -> Box<MutationReject> {
    match update(pipeline, update_text) {
        Err(MutationError::Rejected(reject)) => reject,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): the refusal counts only results at
/// or above the threshold and names the first by decoded terms, regardless of term ids.
#[test]
fn only_refusing_severities_are_counted_and_listed_in_term_order() {
    for (least, expected) in [
        (GateSeverity::Violation, &["a", "z"][..]),
        (GateSeverity::Warning, &["a", "w", "z"][..]),
        (GateSeverity::Info, &["0", "a", "w", "z"][..]),
    ] {
        let pipeline = pipeline(ShaclGate::Enforce(least));
        update(
            &pipeline,
            &format!(
                "INSERT DATA {{
                    ex:z ex:note ex:0, ex:w, ex:a .
                    GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                        ex:I sh:targetClass ex:Soft ; sh:path ex:name ; sh:minCount 1 ; sh:severity sh:Info .
                        ex:W sh:targetClass ex:Warn ; sh:path ex:name ; sh:minCount 1 ; sh:severity sh:Warning .
                        ex:V sh:targetClass ex:Hard ; sh:path ex:name ; sh:minCount 1 .
                    }}
                }}"
            ),
        )
        .expect("shapes and terms");
        let rejected = refuse(
            &pipeline,
            "INSERT DATA { ex:z a ex:Hard . ex:0 a ex:Soft . ex:w a ex:Warn . ex:a a ex:Hard }",
        );
        let first = format!(
            "the commit introduces {} SHACL result(s) the gate rejects; first: focus node <http://example.com/{}>",
            expected.len(),
            expected[0]
        );
        assert!(rejected.detail.starts_with(&first), "{rejected:?}");
        let report = rejected.shacl.as_ref().expect("SHACL refusal report");
        let focuses: Vec<_> = report
            .results
            .iter()
            .map(|result| {
                result
                    .focus_node
                    .strip_prefix("http://example.com/")
                    .unwrap()
            })
            .collect();
        assert_eq!(focuses.as_slice(), expected);
    }
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): result paths and values in refusals
/// use the validation report's spelling, including stored blank node labels.
#[test]
fn shacl_refusal_writes_rdf_terms_and_property_paths_as_text() {
    let pipeline = pipeline_with(StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        shacl_reject_results: None,
        ..StoreConfig::in_memory()
    });
    update(
        &pipeline,
        &format!(
            "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{
                ex:inverse sh:targetClass ex:Paths ; sh:path [ sh:inversePath ex:p ] ; sh:minCount 1 .
                ex:sequence sh:targetClass ex:Paths ; sh:path ( ex:p ex:q ) ; sh:minCount 1 .
                ex:alternative sh:targetClass ex:Paths ; sh:path [ sh:alternativePath ( ex:p ex:q ) ] ; sh:minCount 1 .
                ex:star sh:targetClass ex:Paths ; sh:path [ sh:zeroOrMorePath ex:p ] ; sh:maxCount 0 .
                ex:plus sh:targetClass ex:Paths ; sh:path [ sh:oneOrMorePath ex:p ] ; sh:minCount 1 .
                ex:question sh:targetClass ex:Paths ; sh:path [ sh:zeroOrOnePath ex:p ] ; sh:maxCount 0 .
                ex:literals sh:targetClass ex:Values ; sh:path ex:p ; sh:nodeKind sh:IRI .
                ex:resources sh:targetClass ex:Values ; sh:path ex:q ; sh:nodeKind sh:Literal .
                ex:blank sh:targetClass ex:Blanks ; sh:nodeKind sh:IRI .
            }} }}"
        ),
    )
    .expect("shapes");
    let rejected = refuse(
        &pipeline,
        r#"INSERT DATA {
            ex:x a ex:Paths . ex:y a ex:Values ; ex:p 5, "x"@en ; ex:q ex:o, _:b .
            _:b a ex:Blanks .
        }"#,
    );
    let report = rejected.shacl.as_ref().expect("SHACL refusal report");
    let rows = report
        .results
        .iter()
        .map(|r| {
            (
                r.source_shape.as_str(),
                r.path.as_deref(),
                r.value.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let mut paths = rows
        .iter()
        .filter_map(|(shape, path, value)| {
            let p = shape.strip_prefix("http://example.com/")?;
            (value.is_none()).then_some((p, (*path)?))
        })
        .collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            (
                "alternative",
                "(<http://example.com/p>|<http://example.com/q>)"
            ),
            ("inverse", "^<http://example.com/p>"),
            ("plus", "(<http://example.com/p>)+"),
            ("question", "(<http://example.com/p>)?"),
            (
                "sequence",
                "(<http://example.com/p>/<http://example.com/q>)"
            ),
            ("star", "(<http://example.com/p>)*"),
        ]
    );
    let blank = report
        .results
        .iter()
        .find(|r| r.source_shape == "http://example.com/blank")
        .expect("blank focus");
    assert!(blank.focus_node.starts_with("_:"));
    assert_eq!(blank.path, None);
    assert_eq!(blank.value.as_deref(), Some(blank.focus_node.as_str()));
    for (path, value) in [
        ("p", r#""5"^^<http://www.w3.org/2001/XMLSchema#integer>"#),
        ("p", r#""x"@en"#),
        ("q", "http://example.com/o"),
        ("q", blank.focus_node.as_str()),
    ] {
        let path = format!("http://example.com/{path}");
        assert!(
            rows.iter()
                .any(|(_, p, v)| *p == Some(path.as_str()) && *v == Some(value)),
            "{rows:?}"
        );
    }
    assert!(report.results.iter().all(|r| r.messages.is_empty()));
}

/// Spec (docs/ops/http-api.md, docs/design/shacl.md): the default lists the first ten
/// refusing results and counts omitted ones. An unlimited refusal lists them all.
#[test]
fn the_refusal_bounds_its_results_unless_unlimited() {
    let default = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    let unlimited = pipeline_with(StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        shacl_reject_results: None,
        ..StoreConfig::in_memory()
    });
    let statements: String = (0..12)
        .rev()
        .map(|i| format!("ex:person{i:02} a ex:Person . "))
        .collect();
    let update = format!("INSERT DATA {{ {statements} }}");
    for (pipeline, listed, omitted) in [(default, 10, 2), (unlimited, 12, 0)] {
        add_shapes(&pipeline);
        let rejected = refuse(&pipeline, &update);
        let report = rejected.shacl.as_ref().expect("SHACL refusal report");
        assert_eq!(
            (report.results.len(), report.results_omitted),
            (listed, omitted)
        );
        for (i, result) in report.results.iter().enumerate() {
            assert_eq!(
                result.focus_node,
                format!("http://example.com/person{i:02}")
            );
        }
    }
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
/// every validation fail, a shapes graph with nothing else included: the
/// enforcing gate rejects a write, though no shape constrains anything. The refusal lists
/// the failure texts in order, up to the limit (docs/ops/http-api.md, docs/design/shacl.md).
#[test]
fn a_shapes_graph_asking_for_an_unsupported_entailment_fails_the_gate() {
    let pipeline = pipeline_with(StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        shacl_reject_results: Some(1),
        ..StoreConfig::in_memory()
    });
    load_shapes(
        &pipeline,
        "<http://example.com/S> <http://www.w3.org/ns/shacl#entailment> <urn:z> .
         <http://example.com/S> <http://www.w3.org/ns/shacl#entailment> <urn:a> .",
    );
    let rejected = refuse(&pipeline, "INSERT DATA { ex:a ex:name \"A\" }");
    let report = rejected.shacl.as_ref().expect("SHACL refusal report");
    assert_eq!(
        report.failures,
        [
            "the shapes graph asks for the entailment regime <urn:a> (sh:entailment), \
          which this processor doesn't support"
        ]
    );
    assert_eq!(report.failures_omitted, 1);
    assert!(report.results.is_empty());
    assert_eq!(report.results_omitted, 0);
    assert!(rejected.explanation.is_none());
    assert!(!holds(&pipeline, "ex:a ex:name \"A\""));
}

/// On the gate's side: a shapes graph that asks for an unsupported
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

/// Through the gate: a constraint whose query computes its result's path
/// (`urn:computed-path`, which no statement uses) has that result before and after a write
/// that stores the very IRI as an unrelated statement's predicate. The write introduces no
/// result, so the enforcing gate accepts it.
#[test]
fn a_write_that_stores_a_term_a_constraint_computed_is_accepted() {
    let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
    load_shapes(
        &pipeline,
        r#"<http://example.com/S> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#NodeShape> .
<http://example.com/S> <http://www.w3.org/ns/shacl#targetNode> <http://example.com/a> .
<http://example.com/S> <http://www.w3.org/ns/shacl#sparql> _:q .
_:q <http://www.w3.org/ns/shacl#select> "SELECT $this (IRI(CONCAT(\"urn:computed-\", \"path\")) AS ?path) WHERE { }" ."#,
    );
    update(&pipeline, "INSERT DATA { ex:z <urn:computed-path> ex:w }")
        .expect("a write that introduces no result is accepted");
    assert!(holds(&pipeline, "ex:z <urn:computed-path> ex:w"));
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
