//! Inferred statements under graph access with `inferred = "supported"`: a reader sees an
//! inferred statement when one of its derivations uses graphs it may read alone (support
//! graph sets, `nrese_store::support`), in every read path.

use crate::support::Writes;
use std::sync::Arc;

use crate::support::in_memory_store_config;
use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::GraphAccess;
use nrese_store::{
    ReadContext, ReadScope, SparqlQueryRequest, SparqlUpdateRequest, StatementPattern, StoreService,
};

const EX: &str = "http://example.com/";

fn store() -> StoreService {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ ex:C rdfs:subClassOf ex:D . ex:D rdfs:subClassOf ex:E .
                            GRAPH ex:open {{ ex:a a ex:C }}
                            GRAPH ex:secret {{ ex:b a ex:C . ex:C rdfs:subClassOf ex:F }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    store
}

fn reader(by_support: bool) -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        default_graph: true,
        inferred: true,
        inferred_by_support: by_support,
        ..GraphAccess::default()
    }))
}

/// The classes `subject` is inferred to be in, as `scope` reads them.
fn classes(store: &StoreService, scope: &ReadScope, subject: &str) -> Vec<String> {
    let pattern = StatementPattern {
        subject: Some(nrese_rdf::NamedNode::new_unchecked(format!("{EX}{subject}")).into()),
        predicate: Some(nrese_rdf::NamedNode::new_unchecked(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        )),
        ..StatementPattern::default()
    };
    let mut classes: Vec<String> = store
        .statements(&ReadContext::new(scope.clone()), &pattern)
        .unwrap()
        .into_iter()
        .filter(|quad| quad.graph_name.is_default_graph())
        .map(|quad| quad.object.to_string().replace(EX, ""))
        .collect();
    classes.sort();
    classes.dedup();
    classes
}

fn select(store: &StoreService, scope: &ReadScope, query: &str) -> String {
    let result = store
        .execute_query(&SparqlQueryRequest::new(
            format!("PREFIX ex: <{EX}> {query}"),
            scope.clone(),
        ))
        .unwrap();
    String::from_utf8(result.payload).unwrap()
}

#[test]
fn readers_see_the_inferences_their_graphs_support() {
    let store = store();
    let supported = reader(true);
    // `a` is in `open`: its classes over the default graph's schema are visible; F needs
    // the secret graph's axiom.
    assert_eq!(classes(&store, &supported, "a"), ["<D>", "<E>"]);
    // `b` is in `secret` only.
    assert!(classes(&store, &supported, "b").is_empty());
    // Every inference with "visible", none with "hidden", all unrestricted.
    assert_eq!(classes(&store, &reader(false), "a"), ["<D>", "<E>", "<F>"]);
    assert_eq!(classes(&store, &reader(false), "b"), ["<D>", "<E>", "<F>"]);
    let hidden = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        default_graph: true,
        ..GraphAccess::default()
    }));
    assert!(classes(&store, &hidden, "a").is_empty());
    assert_eq!(classes(&store, &ReadScope::All, "a"), ["<D>", "<E>", "<F>"]);

    // Queries, counts included, see the same.
    let count = select(
        &store,
        &supported,
        "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:D }",
    );
    assert!(count.contains("\"1\""), "{count}");
    let all = select(
        &store,
        &ReadScope::All,
        "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:D }",
    );
    assert!(all.contains("\"2\""), "{all}");
    let names = select(&store, &supported, "SELECT ?s WHERE { ?s a ex:E }");
    assert!(
        names.contains("example.com/a") && !names.contains("example.com/b"),
        "{names}"
    );

    // Explanations of statements the reader doesn't see are refused.
    let explain = |subject: &str, class: &str| {
        store
            .explain_statement(
                Ruleset::Owl2Rl,
                &supported,
                nrese_rdf::NamedNode::new_unchecked(format!("{EX}{subject}"))
                    .as_ref()
                    .into(),
                nrese_rdf::NamedNode::new_unchecked(
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                )
                .as_ref()
                .into(),
                nrese_rdf::NamedNode::new_unchecked(format!("{EX}{class}"))
                    .as_ref()
                    .into(),
            )
            .unwrap()
    };
    assert!(explain("a", "E").is_some());
    assert!(explain("a", "F").is_none());
    assert!(explain("b", "D").is_none());
}

/// A new revision gets its own sets: data the reader may read, added later, supports
/// what it derives.
#[test]
fn support_follows_the_latest_revision() {
    let store = store();
    let supported = reader(true);
    assert!(classes(&store, &supported, "c").is_empty());
    // Loaded past the pipeline, which would maintain the closure and the sets itself.
    let data = format!("PREFIX ex: <{EX}> ex:open {{ ex:c a ex:C }}");
    crate::support::load_past_the_pipeline(&store, &data);
    // The rematerialisation prepares the new revision's sets in the background (they
    // were in use); the read waits for that computation instead of starting another.
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    assert_eq!(classes(&store, &supported, "c"), ["<D>", "<E>"]);
    assert_eq!(store.support_statistics().computed, 2);
}

/// The cap on support graph sets hides nothing: `w a C` in 17 graphs and `C subClassOf D`
/// in the default graph give `w a D` 17 sets, one past the default cap of 16. Graphs sort
/// by IRI, so the cap leaves g16's set out: a reader of g16 sees `w a D` all the same,
/// decided by the closure of what it reads, as a reader of g03 is by a kept set; a reader
/// of none of them doesn't.
#[test]
fn readers_past_the_cap_see_what_their_graphs_derive() {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    let graphs: String = (0..17)
        .map(|g| format!("GRAPH ex:g{g:02} {{ ex:w a ex:C }} "))
        .collect();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ ex:C rdfs:subClassOf ex:D . {graphs} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    let reading = |graph: &str| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}{graph}")],
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        }))
    };
    assert_eq!(classes(&store, &reading("g16"), "w"), ["<D>"]);
    assert_eq!(classes(&store, &reading("g03"), "w"), ["<D>"]);
    assert!(classes(&store, &reading("other"), "w").is_empty());
    // The readers of g16 and of neither asked their closures; g03's kept set decided.
    assert_eq!(store.support_statistics().closures, 2);
}

/// What a reader derives from its own graph doesn't depend on whether a graph it can't read
/// also asserts it (review r1 of N12, P0-1): the stack keeps no inferred copy of a statement
/// asserted anywhere, so a view that only filters the stack lost it exactly when a hidden
/// graph asserted it, an exact membership test for any statement the reader can make
/// derivable. Each row: the reader's graph `open` derives the statement, `secret` asserts it
/// or not; ASK and COUNT answer alike either way, by support and seeing every inference.
#[test]
fn a_hidden_assertion_does_not_change_what_a_reader_derives() {
    const OWL: &str = "PREFIX owl: <http://www.w3.org/2002/07/owl#>";
    // (what `open` holds, what `secret` may assert, the ASK, the COUNT)
    let rows = [
        (
            "ex:alice a ex:Human . ex:Human rdfs:subClassOf ex:Patient",
            "ex:alice a ex:Patient",
            "ASK { ex:alice a ex:Patient }",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:Patient }",
        ),
        (
            "ex:alice ex:noted ex:v . ex:noted rdfs:subPropertyOf ex:diagnosis",
            "ex:alice ex:diagnosis ex:v",
            "ASK { ex:alice ex:diagnosis ex:v }",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ex:diagnosis ?o }",
        ),
        (
            "ex:key a owl:InverseFunctionalProperty . ex:alice ex:key 7 . ex:bob ex:key 7",
            "ex:alice owl:sameAs ex:bob",
            "ASK { ex:alice owl:sameAs ex:bob }",
            "SELECT (COUNT(*) AS ?n) WHERE { ex:alice owl:sameAs ?o }",
        ),
    ];
    let answers = |open: &str, secret: Option<&str>, ask: &str, count: &str, scope: &ReadScope| {
        let store = StoreService::new(in_memory_store_config()).unwrap();
        let hidden = secret.map_or(String::new(), |s| format!("GRAPH ex:secret {{ {s} }}"));
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {OWL}
                 INSERT DATA {{ GRAPH ex:open {{ {open} }} {hidden} }}"
            )))
            .unwrap();
        store.rematerialise(Ruleset::Owl2Rl).unwrap();
        store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        let query = |q: &str| select(&store, scope, &format!("{OWL} {q}"));
        (query(ask), query(count))
    };
    for (by_support, mode) in [(true, "supported"), (false, "visible")] {
        let scope = reader(by_support);
        for (open, secret, ask, count) in rows {
            let public = answers(open, None, ask, count, &scope);
            assert!(
                public.0.contains("true"),
                "{mode}: {ask} derivable from open: {public:?}"
            );
            let hidden = answers(open, Some(secret), ask, count, &scope);
            assert_eq!(hidden, public, "{mode}: {secret} asserted in secret");
        }
    }
}

/// An update's WHERE reads what its requester reads, as its queries do (review r1 of PR #50,
/// F1): a supported reader of `open` finds what `open` derives whether or not `secret`
/// asserts it (`a_hidden_assertion_does_not_change_what_a_reader_derives`), so a marker its
/// update inserts where that holds lands either way. Through the mutation pipeline, and in a
/// transaction's pending operations, read before any commit.
#[test]
fn an_updates_where_reads_what_its_requester_reads() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{
        CancellationToken, MutationCommand, MutationPipeline, MutationTicket, PreparedQuery,
        Requester, StatementOp, StatementsRequest, WriteScope,
    };
    let marker = |name: &str| format!("ex:{name} ex:observed true");
    let insert = |name: &str| {
        format!(
            "PREFIX ex: <{EX}> INSERT {{ GRAPH ex:open {{ {} }} }} WHERE {{ ex:alice a ex:Patient }}",
            marker(name)
        )
    };
    let observed = |hidden: bool| {
        let pipeline = MutationPipeline::new(
            Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        );
        let secret = match hidden {
            true => "GRAPH ex:secret { ex:alice a ex:Patient }",
            false => "",
        };
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                     INSERT DATA {{ GRAPH ex:open {{ ex:alice a ex:Human .
                                    ex:Human rdfs:subClassOf ex:Patient }} {secret} }}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap();
        let writer = Requester::new(
            reader(true),
            WriteScope::Graphs(Arc::new(GraphAccess {
                graphs: vec![format!("{EX}open")],
                ..GraphAccess::default()
            })),
        );
        let store = pipeline.store();
        let derived = select(store, &reader(true), "ASK { ex:alice a ex:Patient }");
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(insert("committed"))),
                &writer,
                &MutationTicket::new(),
            )
            .unwrap();
        let committed = select(
            store,
            &reader(true),
            &format!("ASK {{ GRAPH ex:open {{ {} }} }}", marker("committed")),
        );
        let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
            insert("pending"),
        ))]);
        let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(
            format!(
                "PREFIX ex: <{EX}> ASK {{ GRAPH ex:open {{ {} }} }}",
                marker("pending")
            ),
            reader(true),
        ))
        .unwrap();
        let mut out = Vec::new();
        store
            .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut out)
            .unwrap();
        (derived, committed, String::from_utf8(out).unwrap())
    };
    let public = observed(false);
    assert!(
        public.0.contains("true") && public.1.contains("true") && public.2.contains("true"),
        "derived from open: {public:?}"
    );
    assert_eq!(
        observed(true),
        public,
        "ex:alice a ex:Patient asserted in secret"
    );
}

/// A pipeline over `open` (`alice a Human`, `Human ⊑ Patient`) and `secret` (`bob a Human`,
/// and `alice a Patient` where `secret_alice`), its closure made, and the writer of `open`
/// who reads it by support: `alice a Patient` is derived from `open` alone, `bob a Patient`
/// only with `secret`.
fn patients(
    secret_alice: bool,
) -> (
    nrese_store::MutationPipeline,
    nrese_store::Requester,
    impl Fn(&nrese_store::MutationPipeline, &str, &nrese_store::Requester),
) {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester, WriteScope};
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let apply = |pipeline: &MutationPipeline, update: &str, requester: &Requester| {
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {update}"
                ))),
                requester,
                &MutationTicket::new(),
            )
            .unwrap();
    };
    let alice = match secret_alice {
        true => "ex:alice a ex:Patient .",
        false => "",
    };
    apply(
        &pipeline,
        &format!(
            "INSERT DATA {{ GRAPH ex:open {{ ex:alice a ex:Human . ex:Human rdfs:subClassOf ex:Patient }}
                            GRAPH ex:secret {{ ex:bob a ex:Human . {alice} }} }}"
        ),
        &Requester::all(),
    );
    let writer = Requester::new(
        reader(true),
        WriteScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}open")],
            ..GraphAccess::default()
        })),
    );
    (pipeline, writer, apply)
}

/// Each `WHERE` of a request reads the state before its operation, the earlier operations
/// included, as its requester reads it (the orchestrator's case 1 of F1): after a first
/// operation, the transaction holds changes, and a later `WHERE` that depends on them reads
/// that pending state through the requester's view too. It sees `alice a Patient`, derived
/// from `open`, whether or not `secret` asserts it, and not `bob a Patient`, which only
/// `secret` supports. Through the mutation pipeline, and replayed in a transaction's reads.
#[test]
fn an_updates_later_where_reads_the_earlier_operations_as_its_requester_reads() {
    use nrese_store::{CancellationToken, PreparedQuery, StatementOp, StatementsRequest};
    let request = |tag: &str| {
        format!(
            "INSERT DATA {{ GRAPH ex:open {{ ex:{tag} ex:step 1 }} }} ;
             INSERT {{ GRAPH ex:open {{ ex:{tag}-bob ex:observed true }} }}
             WHERE {{ GRAPH ex:open {{ ex:{tag} ex:step 1 }} ex:bob a ex:Patient }} ;
             INSERT {{ GRAPH ex:open {{ ex:{tag}-alice ex:observed true }} }}
             WHERE {{ GRAPH ex:open {{ ex:{tag} ex:step 1 }} ex:alice a ex:Patient }}"
        )
    };
    let markers = |tag: &str| {
        format!(
            "SELECT ?m WHERE {{ GRAPH ex:open {{ ?m ex:observed true }}
                                FILTER(STRSTARTS(STR(?m), \"{EX}{tag}-\")) }} ORDER BY ?m"
        )
    };
    for secret_alice in [false, true] {
        let (pipeline, writer, apply) = patients(secret_alice);
        apply(&pipeline, &request("committed"), &writer);
        let committed = select(pipeline.store(), &reader(true), &markers("committed"));
        let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
            format!(
                "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {}",
                request("pending")
            ),
        ))]);
        let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(
            format!("PREFIX ex: <{EX}> {}", markers("pending")),
            reader(true),
        ))
        .unwrap();
        let mut out = Vec::new();
        pipeline
            .store()
            .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut out)
            .unwrap();
        let pending = String::from_utf8(out).unwrap();
        for (how, answers, tag) in [
            ("committed", &committed, "committed"),
            ("pending", &pending, "pending"),
        ] {
            assert!(
                answers.contains(&format!("{EX}{tag}-alice"))
                    && !answers.contains(&format!("{EX}{tag}-bob")),
                "{how}, alice a Patient asserted in secret: {secret_alice}: {answers}"
            );
        }
    }
}

/// An update's `WHERE` reads the graphs its requester may read only, its view of the inferred
/// statements notwithstanding (the orchestrator's case 2 of F1: graph access and the read
/// source both apply): copying every statement of every graph into `open` copies none of
/// `secret`'s.
#[test]
fn an_updates_where_reads_the_graphs_its_requester_reads_only() {
    let (pipeline, writer, apply) = patients(true);
    apply(
        &pipeline,
        "INSERT { GRAPH ex:open { ?s ex:copied ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }",
        &writer,
    );
    let copied = select(
        pipeline.store(),
        &ReadScope::All,
        "SELECT ?s WHERE { GRAPH ex:open { ?s ex:copied ?o } }",
    );
    assert!(copied.contains(&format!("{EX}alice")), "{copied}");
    assert!(
        !copied.contains(&format!("{EX}bob")),
        "secret's copied: {copied}"
    );
}

/// The same across commits, live: an unrestricted writer asserts the statement in `secret`
/// and later retracts it; after each commit a reader's answers are what they were. The sets
/// and the re-derived facts are updated for the commits, the views made afresh.
#[test]
fn a_hidden_assertion_committed_later_does_not_change_what_a_reader_derives() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let apply = |update: &str| {
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {update}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap();
    };
    apply(
        "INSERT DATA { GRAPH ex:open { ex:alice a ex:Human . ex:Human rdfs:subClassOf ex:Patient } }",
    );
    let seen = || -> Vec<String> {
        [reader(true), reader(false)]
            .iter()
            .flat_map(|scope| {
                [
                    "ASK { ex:alice a ex:Patient }",
                    "SELECT (COUNT(*) AS ?n) WHERE { ?s a ex:Patient }",
                ]
                .map(|query| select(pipeline.store(), scope, query))
            })
            .collect()
    };
    let before = seen();
    assert!(
        before[0].contains("true") && before[2].contains("true"),
        "{before:?}"
    );
    apply("INSERT DATA { GRAPH ex:secret { ex:alice a ex:Patient } }");
    assert_eq!(seen(), before, "asserted in secret");
    apply("DELETE DATA { GRAPH ex:secret { ex:alice a ex:Patient } }");
    assert_eq!(seen(), before, "retracted from secret");
}

/// A fact asserted only in a graph the reader can't read is shown to it as inferred only
/// where it follows without that assertion: a cycle back through the fact itself is no
/// derivation (review r1 of item 18 part 1, F3). `C ⊑ D` and `D ⊑ C` turn the hidden
/// `alice a C` into `alice a D` and back, and `sameAs` symmetry does the same; with an
/// independent public proof the fact is shown. Both modes.
#[test]
fn a_hidden_assertion_does_not_derive_itself() {
    const OWL: &str = "PREFIX owl: <http://www.w3.org/2002/07/owl#>";
    // (what `open` holds, what `secret` asserts, the ASK, whether `open` derives it)
    let rows = [
        (
            "ex:C rdfs:subClassOf ex:D . ex:D rdfs:subClassOf ex:C",
            "ex:alice a ex:C",
            "ASK { ex:alice a ex:C }",
            false,
        ),
        (
            "ex:alice ex:marker ex:bob",
            "ex:alice owl:sameAs ex:bob",
            "ASK { ex:alice owl:sameAs ex:bob }",
            false,
        ),
        (
            "ex:C rdfs:subClassOf ex:D . ex:D rdfs:subClassOf ex:C .
             ex:Human rdfs:subClassOf ex:C . ex:alice a ex:Human",
            "ex:alice a ex:C",
            "ASK { ex:alice a ex:C }",
            true,
        ),
    ];
    for by_support in [false, true] {
        for (open, secret, ask, derived) in rows {
            let store = StoreService::new(in_memory_store_config()).unwrap();
            store
                .execute_update(&SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {OWL}
                     INSERT DATA {{ GRAPH ex:open {{ {open} }} GRAPH ex:secret {{ {secret} }} }}"
                )))
                .unwrap();
            store.rematerialise(Ruleset::Owl2Rl).unwrap();
            store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
            let answer = select(&store, &reader(by_support), &format!("{OWL} {ask}"));
            assert_eq!(
                answer.contains("true"),
                derived,
                "by support {by_support}: {secret} with {open}: {answer}"
            );
        }
    }
}

/// The same across commits: when a commit removes the public proof while the fact still
/// stands through its own hidden assertion (and the cycle back to it), it leaves the
/// reader's view; when the proof comes back, so does the fact. `alice a X` stays derived
/// through the cycle, so the commit changes no inferred statement and one step from the
/// removed fact doesn't reach `alice a C`: the update follows the whole forward cone.
#[test]
fn a_hidden_assertion_left_without_its_public_proof_leaves_the_view() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let apply = |update: &str| {
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> {update}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap();
    };
    apply(
        "INSERT DATA { ex:Human rdfs:subClassOf ex:X . ex:X rdfs:subClassOf ex:C .
                       ex:C rdfs:subClassOf ex:X .
                       GRAPH ex:open { ex:alice a ex:Human } GRAPH ex:secret { ex:alice a ex:C } }",
    );
    let seen = || -> Vec<bool> {
        [reader(true), reader(false)]
            .iter()
            .map(|scope| {
                select(pipeline.store(), scope, "ASK { ex:alice a ex:C }").contains("true")
            })
            .collect()
    };
    assert_eq!(seen(), [true, true], "derived from open");
    apply("DELETE DATA { GRAPH ex:open { ex:alice a ex:Human } }");
    assert_eq!(seen(), [false, false], "only through itself");
    apply("INSERT DATA { GRAPH ex:open { ex:alice a ex:Human } }");
    assert_eq!(seen(), [true, true], "derived from open again");
}

/// A restricted read whose query is cancelled while its view is computed fails as
/// cancelled (the request's time running out cancels it too), never with fewer
/// statements: the view's computation stops at its first round, so no re-derived facts or
/// view are made. Nothing of it is kept, so the next read answers.
#[test]
fn a_cancelled_restricted_read_fails_and_leaves_nothing_behind() {
    use nrese_store::{CancellationToken, PreparedQuery, QueryEvaluationError, StoreError};
    let store = store();
    let scope = reader(false);
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(
        format!("PREFIX ex: <{EX}> SELECT ?c WHERE {{ ex:a a ?c }}"),
        scope.clone(),
    ))
    .unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let before = store.support_statistics();
    let result = store.run_query(&prepared, &cancelled, &mut Vec::new());
    assert!(
        matches!(
            result,
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::Cancelled
            ))
        ),
        "{result:?}"
    );
    assert_eq!(store.support_statistics(), before, "the view was computed");
    assert_eq!(classes(&store, &scope, "a"), ["<D>", "<E>", "<F>"]);
}

/// A read inside a transaction charges the view it makes to its query's memory, as a read
/// outside one does (sol-loop r1 of item 18 part 1, S2). 5,000 facts in the reader's graph
/// give its supported view sets well past 64 KiB, which the query itself fits (a reader of
/// every graph, with no view, answers): the reader's first read fails with the query's
/// memory limit. Nothing of it is kept, so the same read without a limit answers. With the
/// base's view made, the read keeps no more than its operations' delta of it (review r1 of
/// PR #50, D5; review r1 of O4, M charged the copy it made before), and answers within the
/// limit.
#[test]
fn a_pending_read_charges_its_view_to_the_query() {
    use nrese_store::{CancellationToken, PreparedQuery, StatementOp, StatementsRequest};
    let store = StoreService::new(in_memory_store_config()).unwrap();
    let facts: String = (0..5_000).map(|i| format!("ex:i{i} a ex:C . ")).collect();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH ex:open {{ ex:C rdfs:subClassOf ex:D . {facts} }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
        format!("PREFIX ex: <{EX}> INSERT DATA {{ GRAPH ex:open {{ ex:z a ex:C }} }}"),
    ))]);
    let ask = |scope: ReadScope, query: &str, limit: Option<usize>| {
        let mut request = SparqlQueryRequest::new(format!("PREFIX ex: <{EX}> {query}"), scope);
        request.memory_limit = limit;
        let prepared = PreparedQuery::parse(&request).unwrap();
        let mut out = Vec::new();
        store
            .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut out)
            .map(|()| String::from_utf8(out).unwrap())
    };
    let limit = Some(64 * 1024);
    let unrestricted = ask(ReadScope::All, "ASK {}", limit);
    assert!(unrestricted.is_ok(), "the query alone: {unrestricted:?}");
    let restricted = ask(reader(true), "ASK {}", limit);
    assert!(
        matches!(&restricted, Err(error) if error.is_memory_limit()),
        "{restricted:?}"
    );
    let again = ask(reader(true), "ASK { ex:i0 a ex:D }", None);
    assert!(
        again.as_ref().is_ok_and(|a| a.contains("true")),
        "{again:?}"
    );
    // The base's view is made now (the read without a limit made it): the read applies its
    // operations to it, copying nothing of it.
    let warm = ask(reader(true), "ASK {}", limit);
    assert!(warm.is_ok(), "with the base's view made: {warm:?}");
}

/// A read inside a transaction charges what its operations' replay reads and builds to its
/// query's memory, as it charges the read itself (sol-loop of PR #50 over F1, D5 and D7, S1;
/// G1's I37). Each row: a control that answers within the query's limit, then the read whose
/// replay takes it past that limit, refused with the query's memory limit, not the server's.
/// - The view: the replay's `WHERE` reads through the reader's view, so the reader's first
///   read makes it there (5,000 facts in its graph: well past 64 KiB, as in
///   `a_pending_read_charges_its_view_to_the_query`). The control: a reader of every graph,
///   who has no view.
/// - The `WHERE`'s solutions: 10,000 statements matched by a reader of every graph, at 16 KiB
///   (`ASK {}` itself needs 64 bytes). The control: a `WHERE` that matches nothing.
#[test]
fn a_pending_reads_replay_is_charged_to_the_query() {
    use nrese_store::{
        CancellationToken, PreparedQuery, StatementOp, StatementsRequest, StoreResult,
    };
    let ask = |store: &StoreService, scope: ReadScope, update: &str, limit: usize| {
        let mut request = SparqlQueryRequest::new("ASK {}", scope);
        request.memory_limit = Some(limit);
        let prepared = PreparedQuery::parse(&request).unwrap();
        let pending =
            StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(update))]);
        store.run_query_pending(
            &pending,
            &prepared,
            &CancellationToken::new(),
            &mut Vec::new(),
        )
    };
    let supported = || {
        let store = StoreService::new(in_memory_store_config()).unwrap();
        let facts: String = (0..5_000).map(|i| format!("ex:i{i} a ex:C . ")).collect();
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 INSERT DATA {{ GRAPH ex:open {{ ex:C rdfs:subClassOf ex:D . {facts} }} }}"
            )))
            .unwrap();
        store.rematerialise(Ruleset::Owl2Rl).unwrap();
        store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        store
    };
    let marker =
        format!("PREFIX ex: <{EX}> INSERT {{ GRAPH ex:open {{ ex:m ex:p ex:o }} }} WHERE {{}}");
    let matched = StoreService::new(in_memory_store_config()).unwrap();
    let facts: String = (0..10_000)
        .map(|i| format!("<urn:s{i}> <urn:p> {i} . "))
        .collect();
    matched
        .execute_update(&SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ GRAPH <urn:open> {{ {facts} }} }}"
        )))
        .unwrap();
    let copy = |p: &str| {
        format!(
            "INSERT {{ GRAPH <urn:open> {{ ?s <urn:q> ?o }} }} WHERE {{ GRAPH <urn:open> {{ ?s <{p}> ?o }} }}"
        )
    };
    let rows: [(&str, StoreResult<()>, StoreResult<()>); 2] = [
        (
            "the view",
            ask(&supported(), ReadScope::All, &marker, 64 * 1024),
            ask(&supported(), reader(true), &marker, 64 * 1024),
        ),
        (
            "the WHERE's solutions",
            ask(&matched, ReadScope::All, &copy("urn:none"), 16 * 1024),
            ask(&matched, ReadScope::All, &copy("urn:p"), 16 * 1024),
        ),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter(|(_, control, read)| {
            !(control.is_ok()
                && matches!(read, Err(error) if error.is_memory_limit() && !error.is_server_memory_limit()))
        })
        .map(|(row, control, read)| format!("{row}: control {control:?}, read {read:?}"))
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A read inside a transaction whose query is cancelled (its time running out cancels it too)
/// fails as cancelled and keeps no view, as one outside it does
/// (`a_cancelled_restricted_read_fails_and_leaves_nothing_behind`; Sol's review of O4's
/// boundary round, S2). Cold, the base's view stops at its first round, so nothing of it is
/// made. With the base's view made, the read stops before it applies its operations to it.
/// With the sets made, a reader of other graphs, whose view is new, doesn't build it either,
/// nor charge for it (Sol's round 3).
/// 5,000 facts in the reader's graph give its view sets to compute; the transaction holds
/// one RDF4J `ADD`.
#[test]
fn a_cancelled_pending_read_fails_and_keeps_no_view() {
    use nrese_store::{
        CancellationToken, GraphResultFormat, PreparedQuery, QueryEvaluationError, RdfPayload,
        StatementOp, StatementsRequest, StoreError, StoreResult,
    };
    let store = StoreService::new(in_memory_store_config()).unwrap();
    let facts: String = (0..5_000).map(|i| format!("ex:i{i} a ex:C . ")).collect();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH ex:open {{ ex:C rdfs:subClassOf ex:D . {facts} }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    let pending = StatementsRequest::new(vec![StatementOp::Add {
        data: RdfPayload {
            payload: format!("<{EX}z> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}C> .")
                .into_bytes(),
            format: GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    }]);
    let ask = |scope: ReadScope, cancellation: &CancellationToken, limit: Option<usize>| {
        let mut request = SparqlQueryRequest::new("ASK {}", scope);
        request.memory_limit = limit;
        let prepared = PreparedQuery::parse(&request).unwrap();
        store.run_query_pending(&pending, &prepared, cancellation, &mut Vec::new())
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let is_cancelled = |result: &StoreResult<()>| {
        matches!(
            result,
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::Cancelled
            ))
        )
    };
    let cold = store.support_statistics();
    let result = ask(reader(true), &cancelled, None);
    assert!(is_cancelled(&result), "cold: {result:?}");
    assert_eq!(store.support_statistics(), cold, "the view was computed");
    // A read that runs makes the sets and the base's view.
    let ran = ask(reader(true), &CancellationToken::new(), None);
    assert!(ran.is_ok(), "{ran:?}");
    let made = store.support_statistics();
    let result = ask(reader(true), &cancelled, Some(64 * 1024));
    assert!(
        is_cancelled(&result),
        "with the base's view made: {result:?}"
    );
    assert_eq!(store.support_statistics(), made);
    let other = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open"), format!("{EX}unused")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let result = ask(other.clone(), &cancelled, None);
    assert!(is_cancelled(&result), "a new reader: {result:?}");
    assert_eq!(
        store.support_statistics(),
        made,
        "a new reader's view was built"
    );
    let result = ask(other, &cancelled, Some(64 * 1024));
    assert!(
        is_cancelled(&result),
        "a new reader within 64 KiB: {result:?}"
    );
}

/// A statement or graph read whose read is cancelled (its time running out cancels it too)
/// fails as cancelled and keeps no view, as a query does (Sol's review of O4's boundary
/// round, round 4, S5). With the sets made, a reader whose view is new doesn't build it: RDF4J's
/// statements (its size goes through the same read) and the Graph Store's GET and HEAD, each
/// for a reader of its own. 5,000 facts in the readers' graph give their views sets to build
/// from.
#[test]
fn a_cancelled_statement_read_fails_and_keeps_no_view() {
    use nrese_store::{
        CancellationToken, GraphReadRequest, GraphResultFormat, GraphTarget, QueryEvaluationError,
        StoreError, StoreResult,
    };
    let store = StoreService::new(in_memory_store_config()).unwrap();
    let facts: String = (0..5_000).map(|i| format!("ex:i{i} a ex:C . ")).collect();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH ex:open {{ ex:C rdfs:subClassOf ex:D . {facts} }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    // A read that runs makes the sets (and its reader's view).
    assert!(classes(&store, &reader(true), "i0").contains(&"<D>".to_owned()));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    // A reader of `open` and one more graph: its view is new.
    let new_reader = |i: usize| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}open"), format!("{EX}unused{i}")],
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        }))
    };
    let open = || GraphTarget::NamedGraph(format!("{EX}open"));
    let statements = |read: &ReadContext<'_>| {
        store
            .write_statements(
                read,
                &StatementPattern::default(),
                GraphResultFormat::NQuads,
                std::io::sink(),
            )
            .map(|_| ())
    };
    let get = |read: &ReadContext<'_>| {
        let request = GraphReadRequest {
            target: open(),
            format: GraphResultFormat::NTriples,
        };
        store
            .write_graph(read, &request, std::io::sink())
            .map(|_| ())
    };
    let head = |read: &ReadContext<'_>| store.graph_exists(read, &open()).map(|_| ());
    type Row<'a> = &'a dyn Fn(&ReadContext<'_>) -> StoreResult<()>;
    let rows: [(&str, Row<'_>); 3] = [
        ("RDF4J statements", &statements),
        ("GSP GET", &get),
        ("GSP HEAD", &head),
    ];
    let mut read_on = Vec::new();
    for (i, (name, row)) in rows.into_iter().enumerate() {
        let before = store.support_statistics();
        let result = row(&ReadContext::new(new_reader(i)).cancelled_by(cancelled.clone()));
        let after = store.support_statistics();
        let stopped = matches!(
            result,
            Err(StoreError::SparqlEvaluation(
                QueryEvaluationError::Cancelled
            ))
        );
        if !stopped || after != before {
            read_on.push(format!(
                "{name}: {result:?}, views {} → {}",
                before.views, after.views
            ));
        }
    }
    assert!(
        read_on.is_empty(),
        "read past their cancellation: {read_on:#?}"
    );
}

/// A planned read (EXPLAIN without running) charges the view it makes to its query's memory,
/// as a run does (Sol's review of O4's boundary round, S1). With the query's limit at 1 byte,
/// a supported reader whose view is past it is refused by that limit, not by the server's
/// query memory (64 KiB here), which the view passes too.
#[test]
fn a_planned_read_charges_its_view_to_the_query() {
    use nrese_store::{CancellationToken, StoreConfig};
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: 64 * 1024,
        ..in_memory_store_config()
    })
    .unwrap();
    let facts: String = (0..5_000).map(|i| format!("ex:i{i} a ex:C . ")).collect();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {{ GRAPH ex:open {{ ex:C rdfs:subClassOf ex:D . {facts} }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    let mut request = SparqlQueryRequest::new(
        format!("PREFIX ex: <{EX}> ASK {{ ex:i0 a ex:D }}"),
        reader(true),
    );
    request.memory_limit = Some(1);
    let prepared = store.prepare_query(&request).unwrap();
    let planned = store.plan_query(&prepared, &CancellationToken::new());
    assert!(
        matches!(&planned, Err(error) if error.is_memory_limit() && !error.is_server_memory_limit()),
        "{planned:?}"
    );
}

/// Without rules registered the sets can't be computed: no inferred statement is shown.
#[test]
fn without_rules_nothing_inferred_is_shown() {
    let store = store();
    store.use_reasoning_rules(None);
    assert!(classes(&store, &reader(true), "a").is_empty());
    assert_eq!(classes(&store, &reader(false), "a"), ["<D>", "<E>", "<F>"]);
}

/// A read inside a session answers by the rules in place, as one outside it does
/// (`without_rules_nothing_inferred_is_shown`; review of O4's fix round, S1): the session's
/// kept view of its first read is no answer once the rules are switched off. A supported
/// reader asks for `ex:a a ex:D` inside a session with one unrelated operation, before and
/// after; outside the session it is no longer shown, and inside it neither.
#[test]
fn a_session_read_follows_a_change_of_the_rules() {
    use nrese_store::{CancellationToken, PreparedQuery, StatementOp, StatementsRequest};
    let store = store();
    let sessions = store.sessions();
    let id = sessions.begin(None).unwrap();
    assert!(sessions.add(
        &id,
        None,
        vec![StatementOp::Update(SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> INSERT DATA {{ GRAPH ex:open {{ ex:z a ex:Z }} }}"
        )))]
    ));
    let in_session = StatementsRequest {
        ops: sessions.pending(&id, None).unwrap(),
        session: Some(id.clone()),
    };
    let ask = |pending: &StatementsRequest| {
        let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(
            format!("PREFIX ex: <{EX}> ASK {{ ex:a a ex:D }}"),
            reader(true),
        ))
        .unwrap();
        let mut out = Vec::new();
        store
            .run_query_pending(pending, &prepared, &CancellationToken::new(), &mut out)
            .unwrap();
        String::from_utf8(out).unwrap().contains("true")
    };
    assert!(ask(&in_session), "derived under the rules");
    store.use_reasoning_rules(None);
    assert!(!ask(&StatementsRequest::default()), "outside the session");
    assert!(!ask(&in_session), "inside the session");
}

/// Commits through the mutation pipeline update the sets instead of computing them
/// afresh: after every commit, a reader sees what a store forced to compute them sees,
/// and so does a reader of a store that keeps one set per statement (the cap's marks
/// updated with the sets, the views patched or, where a mark leaves a statement open,
/// made afresh).
#[test]
fn updated_sets_equal_fresh_ones_across_commits() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = |support_sets: usize| {
        MutationPipeline::new(
            Arc::new(
                StoreService::new(nrese_store::StoreConfig {
                    support_sets,
                    ..in_memory_store_config()
                })
                .unwrap(),
            ),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        )
    };
    let default = in_memory_store_config().support_sets;
    let (updating, fresh, capped) = (pipeline(default), pipeline(default), pipeline(1));
    let apply = |pipeline: &MutationPipeline, update: &str| {
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                     PREFIX owl: <http://www.w3.org/2002/07/owl#> {update}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap();
    };
    let commits = [
        "INSERT DATA { ex:C rdfs:subClassOf ex:D . ex:D rdfs:subClassOf ex:E .
                       ex:anc a owl:TransitiveProperty .
                       GRAPH ex:open { ex:a a ex:C . ex:x ex:anc ex:y }
                       GRAPH ex:secret { ex:b a ex:C . ex:y ex:anc ex:z } }",
        "INSERT DATA { GRAPH ex:open { ex:c a ex:C } }",
        "INSERT DATA { GRAPH ex:secret { ex:c a ex:C . ex:z ex:anc ex:w } }",
        "DELETE DATA { GRAPH ex:open { ex:c a ex:C } }",
        "DELETE DATA { GRAPH ex:secret { ex:b a ex:C } } ;
         INSERT DATA { GRAPH ex:open { ex:b a ex:C . ex:y ex:anc ex:z } }",
        "DELETE DATA { GRAPH ex:open { ex:x ex:anc ex:y } }",
        "INSERT DATA { GRAPH ex:open { ex:x ex:anc ex:y } GRAPH ex:new { ex:d a ex:D } }",
        // A schema change: computed afresh.
        "INSERT DATA { GRAPH ex:open { ex:D rdfs:subClassOf ex:F } }",
        "DELETE DATA { GRAPH ex:secret { ex:c a ex:C } }",
    ];
    // A reader of one graph (it misses most: a list of what it sees), one of two (it sees
    // most: a mask of what it misses), and one of the other graph alone: where `open` and
    // `secret` both assert a statement (`c a C`, `y anc z`), the cap of one keeps the set
    // of `open`, the lower graph, and that reader's closure decides what follows.
    let narrow = reader(true);
    let reading = |graphs: &[&str]| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: graphs.iter().map(|graph| format!("{EX}{graph}")).collect(),
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        }))
    };
    let broad = reading(&["open", "secret"]);
    let other = reading(&["secret"]);
    let seen = |pipeline: &MutationPipeline| -> Vec<String> {
        let mut seen = Vec::new();
        for (name, reader) in [("narrow", &narrow), ("broad", &broad), ("other", &other)] {
            for s in ["a", "b", "c", "d", "x", "y", "z"] {
                for c in classes(pipeline.store(), reader, s) {
                    seen.push(format!("{name}: {s} a {c}"));
                }
            }
            let chains = select(
                pipeline.store(),
                reader,
                "SELECT ?s ?o WHERE { ?s ex:anc ?o }",
            );
            seen.extend(
                chains
                    .lines()
                    .filter(|line| line.contains("example.com"))
                    .map(|line| format!("{name}: {line}")),
            );
        }
        seen
    };
    for (step, commit) in commits.iter().enumerate() {
        apply(&updating, commit);
        apply(&fresh, commit);
        apply(&capped, commit);
        // Forget the fresh store's sets: they are computed again.
        fresh
            .store()
            .use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        let expected = seen(&fresh);
        assert_eq!(seen(&updating), expected, "after commit {step}: {commit}");
        assert_eq!(
            seen(&capped),
            expected,
            "cap 1, after commit {step}: {commit}"
        );
    }
    assert!(capped.store().support_statistics().closures > 0);
    let statistics = updating.store().support_statistics();
    assert!(statistics.updated >= 6, "{statistics:?}");
    assert!(statistics.patched >= 10, "{statistics:?}");
    assert!(statistics.computed >= 2, "{statistics:?}");
}
