//! Every read of the store takes a `ReadScope`, every write a `Requester` (the audit of
//! 2 October, §2.3): a restricted scope sees and changes its graphs only, whatever the
//! caller forgets, and the operations over the whole dataset refuse it. The rest of a read
//! (inferred statements, a session's pending changes, cancellation) comes with it in a
//! `ReadContext`.

use crate::support::Writes;
use std::sync::Arc;

use crate::support::in_memory_store_config;
use nrese_rdf::GraphName;
use nrese_sparql::GraphAccess;
use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, GraphReadRequest, GraphResultFormat, GraphTarget,
    GraphWriteRequest, MutationCommand, MutationPipeline, ReadContext, ReadScope, Refusal,
    Requester, SparqlQueryRequest, SparqlUpdateRequest, StatementOp, StatementPattern,
    StatementsRequest, StoreService, TellRequest, WriteScope,
};

fn store() -> StoreService {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(
            "INSERT DATA { <urn:a> <urn:p> 1 .
               GRAPH <urn:g:public> { <urn:b> <urn:p> 2 }
               GRAPH <urn:g:secret> { <urn:c> <urn:p> 3 } }",
        ))
        .unwrap();
    store
}

fn public() -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        prefixes: vec!["urn:g:public".to_owned()],
        default_graph: true,
        ..GraphAccess::default()
    }))
}

#[test]
fn a_restricted_scope_reads_its_graphs_only() {
    let store = store();
    let scope = public();
    let quads = store
        .statements(
            &ReadContext::new(scope.clone()).infer(false),
            &StatementPattern::default(),
        )
        .unwrap();
    let graphs: Vec<GraphName> = quads.iter().map(|q| q.graph_name.clone()).collect();
    assert_eq!(quads.len(), 2, "{quads:?}");
    assert!(!graphs.iter().any(|g| g.to_string().contains("secret")));
    assert_eq!(
        store
            .count(
                &ReadContext::new(scope.clone()).infer(false),
                &StatementPattern::default()
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .count(
                &ReadContext::all().infer(false),
                &StatementPattern::default()
            )
            .unwrap(),
        3
    );
    let contexts: Vec<String> = store
        .contexts(&scope)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(contexts, ["<urn:g:public>"]);
    assert_eq!(store.graph_sizes(&scope).len(), 2);
    // A query: the dataset is restricted before anything is evaluated.
    let result = store
        .execute_query(&SparqlQueryRequest::new(
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
            scope.clone(),
        ))
        .unwrap();
    let text = String::from_utf8(result.payload).unwrap();
    assert!(text.contains("\"1\""), "{text}");
    // A graph the scope doesn't read doesn't exist for it.
    let read = |scope: &ReadScope| {
        store
            .execute_graph_read(
                &ReadContext::new(scope.clone()),
                &GraphReadRequest {
                    target: GraphTarget::NamedGraph("urn:g:secret".to_owned()),
                    format: GraphResultFormat::NTriples,
                },
            )
            .unwrap()
    };
    assert!(!read(&scope).exists);
    assert!(read(&ReadScope::All).exists);
}

#[test]
fn operations_over_the_whole_dataset_refuse_a_restricted_scope() {
    let store = store();
    let scope = public();
    assert!(store.autocomplete(&scope, "a", 5, true).is_err());
    assert!(store.autocomplete(&ReadScope::All, "a", 5, true).is_ok());
    assert!(
        store
            .export_dataset(&scope, DatasetBackupFormat::NQuads)
            .is_err()
    );
    assert!(store.classify(&scope).is_err());
    assert!(store.classify(&ReadScope::All).is_ok());
}

/// The objects of `<urn:x> <urn:p> ?o` as `read` sees them.
fn objects(store: &StoreService, read: &ReadContext<'_>) -> Vec<String> {
    let pattern = StatementPattern {
        subject: Some(nrese_rdf::NamedNode::new_unchecked("urn:x").into()),
        ..StatementPattern::default()
    };
    let mut objects: Vec<String> = store
        .statements(read, &pattern)
        .unwrap()
        .into_iter()
        .map(|quad| quad.object.to_string())
        .collect();
    objects.sort();
    objects
}

#[test]
fn reads_in_a_session_keep_its_data_until_the_session_or_the_store_changes() {
    let store = store();
    let sessions = store.sessions();
    let id = sessions.begin(None).unwrap();
    // A fresh value on every replay: a kept view shows the same one.
    let fresh = || {
        StatementOp::Update(SparqlUpdateRequest::new(
            "INSERT { <urn:x> <urn:p> ?u } WHERE { BIND(STRUUID() AS ?u) }",
        ))
    };
    assert!(sessions.add(&id, None, vec![fresh()]));
    let pending = |session: Option<&str>| StatementsRequest {
        ops: sessions.pending(&id, None).unwrap(),
        session: session.map(str::to_owned),
    };
    let in_session = pending(Some(&id));
    let read = ReadContext::all().on(Some(&in_session));
    let first = objects(&store, &read);
    assert_eq!(first.len(), 1);
    assert_eq!(objects(&store, &read), first, "kept for the next read");
    // Without a session, every read replays the operations.
    let outside = pending(None);
    assert_ne!(
        objects(&store, &ReadContext::all().on(Some(&outside))),
        first
    );
    // Another reader's view is its own.
    let scoped = ReadContext::new(public()).on(Some(&in_session));
    assert_ne!(objects(&store, &scoped), first);
    // A commit by another client: the operations are replayed on the new data.
    store
        .execute_update(&SparqlUpdateRequest::new(
            "INSERT DATA { <urn:x> <urn:p> 0 }",
        ))
        .unwrap();
    let after_commit = objects(&store, &read);
    assert_eq!(after_commit.len(), 2, "{after_commit:?}");
    assert!(!after_commit.contains(&first[0]));
    // More operations: replayed too.
    assert!(sessions.add(&id, None, vec![fresh()]));
    let longer = pending(Some(&id));
    assert_eq!(
        objects(&store, &ReadContext::all().on(Some(&longer))).len(),
        3
    );
}

#[test]
fn statements_are_written_as_they_are_read() {
    let store = store();
    let mut out = Vec::new();
    let written = store
        .write_statements(
            &ReadContext::new(public()).infer(false),
            &StatementPattern::default(),
            GraphResultFormat::NQuads,
            &mut out,
        )
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(written, 2);
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("<urn:g:public>") && !text.contains("secret"));
    // Triples formats drop the graphs.
    let mut out = Vec::new();
    store
        .write_statements(
            &ReadContext::all().infer(false),
            &StatementPattern::default(),
            GraphResultFormat::NTriples,
            &mut out,
        )
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(!text.contains("urn:g:"), "{text}");
    // A cancelled read stops.
    let cancel = nrese_store::CancellationToken::new();
    cancel.cancel();
    assert!(
        store
            .write_statements(
                &ReadContext::all().cancelled_by(cancel),
                &StatementPattern::default(),
                GraphResultFormat::NQuads,
                std::io::sink(),
            )
            .is_err()
    );
}

/// Reads the public graphs, writes `urn:g:public` only.
fn public_writer() -> Requester {
    Requester::new(
        public(),
        WriteScope::Graphs(Arc::new(GraphAccess {
            prefixes: vec!["urn:g:public".to_owned()],
            ..GraphAccess::default()
        })),
    )
}

fn forbidden(
    result: Result<nrese_store::MutationCommitReport, nrese_store::MutationError>,
) -> bool {
    matches!(
        result,
        Err(nrese_store::MutationError::Store {
            source: nrese_store::StoreError::Forbidden(_),
            ..
        })
    )
}

#[test]
fn every_write_is_checked_against_its_requesters_scope() {
    let store = store();
    let writer = public_writer();
    let graph_write = |graph: &str| {
        MutationCommand::GraphWrite(GraphWriteRequest {
            target: GraphTarget::NamedGraph(graph.to_owned()),
            format: GraphResultFormat::NTriples,
            base_iri: None,
            payload: b"<urn:n> <urn:p> \"4\" .".to_vec(),
            replace: true,
        })
    };
    // Graph Store writes, deletes and TELL: the store checks them, whatever the handler did.
    assert!(forbidden(
        store.apply(&graph_write("urn:g:secret"), &writer)
    ));
    assert!(store.apply(&graph_write("urn:g:public"), &writer).is_ok());
    let delete = MutationCommand::GraphDelete(GraphTarget::DefaultGraph);
    // Refusals name what they refuse.
    let refusal = |result: Result<_, nrese_store::MutationError>| match result {
        Err(nrese_store::MutationError::Store {
            source: nrese_store::StoreError::Forbidden(refusal),
            ..
        }) => refusal,
        other => panic!("not refused: {other:?}"),
    };
    assert_eq!(
        refusal(store.apply(&delete, &writer)),
        Refusal::Write(GraphName::DefaultGraph)
    );
    let tell = MutationCommand::Tell(TellRequest {
        target: GraphTarget::NamedGraph("urn:g:secret".to_owned()),
        format: GraphResultFormat::NTriples,
        base_iri: None,
        payload: b"<urn:n> <urn:p> \"5\" .".to_vec(),
    });
    assert_eq!(
        refusal(store.apply(&tell, &writer)),
        Refusal::Write(GraphName::NamedNode(nrese_rdf::NamedNode::new_unchecked(
            "urn:g:secret"
        )))
    );
    // A write that would change nothing is refused all the same: it succeeding where a new
    // statement fails would tell what the graph holds (the review of 3 October 2026, A3).
    let existing = |command: fn(GraphWriteRequest) -> MutationCommand| {
        command(GraphWriteRequest {
            target: GraphTarget::NamedGraph("urn:g:secret".to_owned()),
            format: GraphResultFormat::NTriples,
            base_iri: None,
            payload: b"<urn:c> <urn:p> \"3\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
                .to_vec(),
            replace: false,
        })
    };
    assert!(forbidden(
        store.apply(&existing(MutationCommand::GraphWrite), &writer)
    ));
    let tell = MutationCommand::Tell(TellRequest {
        target: GraphTarget::NamedGraph("urn:g:secret".to_owned()),
        format: GraphResultFormat::NTriples,
        base_iri: None,
        payload: b"<urn:c> <urn:p> \"3\"^^<http://www.w3.org/2001/XMLSchema#integer> .".to_vec(),
    });
    assert!(forbidden(store.apply(&tell, &writer)));
    // Deleting a graph that is empty, or absent, likewise.
    let absent = MutationCommand::GraphDelete(GraphTarget::NamedGraph("urn:g:other".to_owned()));
    assert!(forbidden(store.apply(&absent, &writer)));
    // A restore changes every graph.
    let restore = MutationCommand::Restore(DatasetRestoreRequest {
        format: DatasetBackupFormat::NQuads,
        payload: Vec::new(),
    });
    assert_eq!(
        refusal(store.apply(&restore, &writer)),
        Refusal::WriteAll("a restore".to_owned())
    );
    // Updates: the WHERE clause reads the readable graphs; changes elsewhere are refused.
    let update = |text: &str| MutationCommand::Update(SparqlUpdateRequest::new(text));
    assert!(forbidden(store.apply(
        &update("INSERT DATA { GRAPH <urn:g:secret> { <urn:x> <urn:p> 6 } }"),
        &writer
    )));
    store
        .apply(
            &update(
                "INSERT { GRAPH <urn:g:public> { ?s <urn:copied> ?o } }
                 WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
            ),
            &writer,
        )
        .unwrap();
    let copied = store
        .statements(
            &ReadContext::all().infer(false),
            &StatementPattern {
                predicate: Some(nrese_rdf::NamedNode::new_unchecked("urn:copied")),
                ..StatementPattern::default()
            },
        )
        .unwrap();
    // The public graph holds one statement since the graph write replaced it; the secret
    // graph's isn't read.
    assert_eq!(copied.len(), 1, "{copied:?}");
    // Statement operations: a removal matches in the readable graphs only, so removing
    // everything leaves the secret graph, and the default graph isn't writable.
    let remove_all =
        MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::RemoveMatching(
            StatementPattern::default(),
        )]));
    assert!(forbidden(store.apply(&remove_all, &writer)));
    let remove_public =
        MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::RemoveMatching(
            StatementPattern {
                contexts: vec![GraphName::NamedNode(nrese_rdf::NamedNode::new_unchecked(
                    "urn:g:public",
                ))],
                ..StatementPattern::default()
            },
        )]));
    store.apply(&remove_public, &writer).unwrap();
    assert_eq!(
        store
            .count(
                &ReadContext::all().infer(false),
                &StatementPattern::default()
            )
            .unwrap(),
        2,
        "the default and the secret graph's statements stay"
    );
}

/// `SERVICE` is a privilege of its own: a restricted requester calls other endpoints only
/// if its access says so, in queries and in updates' `WHERE` clauses alike.
#[test]
fn service_is_a_privilege_of_its_own() {
    let store = store();
    let restricted = |service: bool| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            default_graph: true,
            service,
            ..GraphAccess::default()
        }))
    };
    let query = "SELECT * WHERE { SERVICE <http://elsewhere.example/sparql> { ?s ?p ?o } }";
    let error = |scope: ReadScope| {
        store
            .execute_query(&SparqlQueryRequest::new(query, scope))
            .expect_err("no endpoint is reachable here")
            .to_string()
    };
    let refused = error(restricted(false));
    assert!(
        refused.contains("may not call other endpoints"),
        "{refused}"
    );
    // Granted: past the privilege (no federation is set up in this store).
    let granted = error(restricted(true));
    assert!(granted.contains("not enabled"), "{granted}");
    assert!(error(ReadScope::All).contains("not enabled"));
    let update = MutationCommand::Update(SparqlUpdateRequest::new(
        "INSERT { ?s ?p ?o } WHERE { SERVICE <http://elsewhere.example/sparql> { ?s ?p ?o } }",
    ));
    let requester = Requester::new(restricted(false), WriteScope::All);
    let refused = store
        .apply(&update, &requester)
        .expect_err("refused")
        .to_string();
    assert!(
        refused.contains("may not call other endpoints"),
        "{refused}"
    );
}

/// EXPLAIN and EXPLAIN ANALYZE tell a restricted reader nothing of the graphs it may not
/// read: two stores that differ only in a secret graph answer its plan and its run alike
/// (times aside). The planner's estimates, the order they choose and the operators' rows
/// read every graph (round 13, R13-diag), so a restricted reader gets no operators; a reader
/// of every graph still gets them all.
#[test]
fn explain_tells_a_restricted_reader_nothing_of_hidden_graphs() {
    let with_secret = |secret: usize| {
        let store = store();
        let quads: String = (0..secret)
            .map(|i| format!("<urn:s{i}> <urn:p> {i} . <urn:s{i}> <urn:q> <urn:o{i}> . "))
            .collect();
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ GRAPH <urn:g:secret> {{ {quads} }} }}"
            )))
            .unwrap();
        store
    };
    let query = "SELECT * WHERE { GRAPH ?g { ?s <urn:p> ?v . ?s <urn:q> ?o } }";
    let explain = |store: &StoreService, scope: ReadScope| {
        let prepared = store
            .prepare_query(&SparqlQueryRequest::new(query, scope))
            .unwrap();
        let token = nrese_store::CancellationToken::new();
        let plan = store.plan_query(&prepared, &token).unwrap();
        store.clear_query_cache();
        let mut run = store.explain_query(&prepared, &token).unwrap();
        run.micros = 0;
        for step in &mut run.steps {
            step.micros = 0;
        }
        (plan, run)
    };
    let (small, large) = (with_secret(1), with_secret(2_000));
    assert_eq!(explain(&small, public()), explain(&large, public()));
    let (plan, run) = explain(&large, ReadScope::All);
    assert!(!plan.steps.is_empty() && !run.steps.is_empty());
}

/// Nor do the rewrites: eager aggregation is chosen by an estimate, and a restricted reader's
/// merged default graph is estimated over every graph (Sol's review of R13, r1 B-F1). Hidden
/// prices make it pay in one store and not in the other; the answer, the plan and the run the
/// reader is told are the same.
#[test]
fn restricted_rewrites_tell_nothing_of_hidden_statistics() {
    let with_hidden_prices = |hidden: usize| {
        let mut config = in_memory_store_config();
        config.union_default_graph = true;
        let store = StoreService::new(config).unwrap();
        let visible: String = (0..10)
            .map(|i| {
                format!("<urn:product{i}> <urn:team> <urn:t> . <urn:offer{i}> <urn:product> <urn:product{i}> . ")
            })
            .collect();
        let secret: String = (0..hidden)
            .map(|i| format!("<urn:hidden{i}> <urn:price> {} . ", i + 2))
            .collect();
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ {visible} <urn:visible-price> <urn:price> 1 .
                   GRAPH <urn:g:public> {{ <urn:marker> <urn:present> 1 }}
                   GRAPH <urn:g:secret> {{ <urn:hidden-marker> <urn:present> 1 . {secret} }} }}"
            )))
            .unwrap();
        store
    };
    let query = "SELECT ?team (SUM(?price) AS ?total) WHERE { ?product <urn:team> ?team .
                 ?offer <urn:product> ?product . ?x <urn:price> ?price } GROUP BY ?team";
    let observe = |store: &StoreService| {
        let request = SparqlQueryRequest::new(query, public());
        let payload = store.execute_query(&request).unwrap().payload;
        let prepared = store.prepare_query(&request).unwrap();
        let token = nrese_store::CancellationToken::new();
        let plan = store.plan_query(&prepared, &token).unwrap();
        store.clear_query_cache();
        let run = store.explain_query(&prepared, &token).unwrap();
        (payload, plan.rewrites, run.rewrites)
    };
    assert_eq!(
        observe(&with_hidden_prices(0)),
        observe(&with_hidden_prices(20))
    );
}

/// Nor does a memory refusal, at any entry point that reads: a scan sized by a graph the
/// requester may not read runs out of the query memory, and the refusal it is told keeps its
/// kind and limit, not the bytes held and asked for. A requester of every graph is told the
/// bytes. One row per read and update entry point; a new one gets a row (Sol's review of
/// R13: r1 B-F2, an EXPLAIN's; r2 S1, a run's; r3 S1, an update's WHERE; review of PR #39:
/// F2, a run with operations pending).
#[test]
fn memory_refusals_tell_a_restricted_requester_nothing_of_hidden_sizes() {
    let query = "SELECT * WHERE { GRAPH ?g { ?s <urn:p> ?o } }";
    let memory_limit = |error: &nrese_store::StoreError| {
        assert!(error.is_memory_limit(), "a memory refusal, not {error}");
        error.to_string()
    };
    let run = |pipeline: &MutationPipeline, scope: &ReadScope| {
        let store = pipeline.store();
        let prepared = store
            .prepare_query(&SparqlQueryRequest::new(query, scope.clone()))
            .unwrap();
        let token = nrese_store::CancellationToken::new();
        memory_limit(
            &store
                .run_query(&prepared, &token, std::io::sink())
                .unwrap_err(),
        )
    };
    let pending_run = |pipeline: &MutationPipeline, scope: &ReadScope| {
        let store = pipeline.store();
        let prepared = store
            .prepare_query(&SparqlQueryRequest::new(query, scope.clone()))
            .unwrap();
        let add = StatementOp::Add {
            data: nrese_store::RdfPayload {
                payload: b"<urn:pending> <urn:p> \"2\" .".to_vec(),
                format: GraphResultFormat::NTriples,
                base_iri: None,
            },
            contexts: Vec::new(),
        };
        let token = nrese_store::CancellationToken::new();
        let pending = StatementsRequest::new(vec![add]);
        memory_limit(
            &store
                .run_query_pending(&pending, &prepared, &token, std::io::sink())
                .unwrap_err(),
        )
    };
    let explain = |pipeline: &MutationPipeline, scope: &ReadScope| {
        let store = pipeline.store();
        let prepared = store
            .prepare_query(&SparqlQueryRequest::new(query, scope.clone()))
            .unwrap();
        let token = nrese_store::CancellationToken::new();
        memory_limit(&store.explain_query(&prepared, &token).unwrap_err())
    };
    let update_where = |pipeline: &MutationPipeline, scope: &ReadScope| {
        let requester = match scope {
            ReadScope::All => Requester::all(),
            ReadScope::Graphs(_) => public_writer(),
        };
        let update = SparqlUpdateRequest::new(
            "INSERT { GRAPH <urn:g:public> { ?s <urn:copy> ?o } }
             WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
        );
        let ticket = nrese_store::MutationTicket::new();
        match pipeline
            .apply(MutationCommand::Update(update), &requester, &ticket)
            .unwrap_err()
        {
            nrese_store::MutationError::Store { source, .. } => memory_limit(&source),
            other => panic!("a memory refusal, not {other}"),
        }
    };
    type Entry<'a> = &'a dyn Fn(&MutationPipeline, &ReadScope) -> String;
    let entries: [(&str, Entry<'_>); 4] = [
        ("run", &run),
        ("run with operations pending", &pending_run),
        ("EXPLAIN", &explain),
        ("an update's WHERE", &update_where),
    ];
    let store = |hidden: usize| {
        let mut config = in_memory_store_config();
        config.process_memory_bytes = 0;
        config.total_query_memory_bytes = 64 << 10;
        let pipeline = MutationPipeline::new(
            Arc::new(StoreService::new(config).unwrap()),
            Arc::new(nrese_reasoner::ReasonerService::new(
                nrese_reasoner::ReasonerConfig::for_mode(nrese_reasoner::ReasoningMode::Disabled),
            )),
        );
        let secret: String = (0..hidden)
            .map(|i| format!("<urn:hidden{i}> <urn:p> {i} . "))
            .collect();
        pipeline
            .execute_update_str(&format!(
                "INSERT DATA {{ GRAPH <urn:g:public> {{ <urn:visible> <urn:p> 1 }}
                   GRAPH <urn:g:secret> {{ {secret} }} }}"
            ))
            .unwrap();
        pipeline
    };
    let (small, large) = (store(100_000), store(200_000));
    for (name, entry) in entries {
        let public = public();
        assert_eq!(entry(&small, &public), entry(&large, &public), "{name}");
        let all = ReadScope::All;
        let (small_all, large_all) = (entry(&small, &all), entry(&large, &all));
        assert_ne!(small_all, large_all, "{name}: every graph");
    }
}
