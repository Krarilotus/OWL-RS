//! The SHACL commit gate (C2): a commit that introduces results is rejected (enforce) or
//! only reported (report); data that was invalid before doesn't block other commits.

use std::io::Write as _;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, DEFAULT_SHAPES_GRAPH, GateSeverity, GraphTarget, MutationCommand,
    MutationError, MutationPipeline, MutationTicket, ShaclGate, SparqlUpdateRequest, StoreConfig,
    StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> PREFIX sh: <http://www.w3.org/ns/shacl#> ";

fn pipeline(gate: ShaclGate) -> MutationPipeline {
    let config = StoreConfig {
        shacl_gate: gate,
        ..StoreConfig::in_memory()
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        ReasonerConfig::for_mode(ReasoningMode::Disabled),
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

/// N12 (the SHACL first focus; Sol r1, F3): a committer of `open` whose change breaks nodes
/// of a graph it can't read is rejected with the same detail however many there are, and it
/// names none of them (the first focus named a secret node, the count told how many).
#[test]
fn a_restricted_rejection_is_the_same_whatever_secret_graphs_hold() {
    use nrese_sparql::GraphAccess;
    use nrese_store::{ReadScope, Requester, WriteScope};
    let detail = |friends: usize| {
        let pipeline = pipeline(ShaclGate::Enforce(GateSeverity::Violation));
        add_shapes(&pipeline);
        let secret: String = (0..friends)
            .map(|i| format!("ex:hiddenFriend{i} a ex:Person ; ex:name \"H\" ; ex:friend ex:b . "))
            .collect();
        update(
            &pipeline,
            &format!(
                "INSERT DATA {{ GRAPH ex:secret {{ {secret} }}
                               GRAPH ex:open {{ ex:b a ex:Person ; ex:name \"B\" }} }}"
            ),
        )
        .expect("valid");
        let access = Arc::new(GraphAccess {
            graphs: vec!["http://example.com/open".to_owned()],
            ..GraphAccess::default()
        });
        let committer = Requester::new(
            ReadScope::Graphs(Arc::clone(&access)),
            WriteScope::Graphs(access),
        );
        let rejected = pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{PREFIXES}DELETE DATA {{ GRAPH ex:open {{ ex:b a ex:Person }} }}"
            ))),
            &committer,
            &MutationTicket::new(),
        );
        let Err(MutationError::Rejected(reject)) = rejected else {
            panic!("expected a rejection, got {rejected:?}");
        };
        reject.detail
    };
    let one = detail(1);
    assert!(!one.contains("hiddenFriend"), "{one}");
    assert_eq!(detail(2), one);
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
