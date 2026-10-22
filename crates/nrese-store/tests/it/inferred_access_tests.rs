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

/// A store for N12's noninterference tests: a public transitive `p` and a public chain
/// `a→b→c→e→d`, then `private` added to a secret graph; reasoned, the rules registered.
fn chain_store(private: &str) -> StoreService {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             INSERT DATA {{ ex:p a owl:TransitiveProperty .
                            GRAPH ex:open {{ ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:e . ex:e ex:p ex:d }}
                            GRAPH ex:secret {{ {private} }} }}"
        )))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    store
}

/// The secret additions of the noninterference tests: a shorter derivation of `a p d`
/// through secret asserted statements (Sol r1, F1), and one through a secret inferred
/// intermediate (RSH-02, the validation's corrected trigger).
const SHORTCUTS: [&str; 2] = [
    "ex:a ex:p ex:x . ex:x ex:p ex:d",
    "ex:a ex:p ex:x . ex:x ex:p ex:y . ex:y ex:p ex:d",
];

fn chain_statement() -> [nrese_rdf::NamedNode; 3] {
    ["a", "p", "d"].map(|local| nrese_rdf::NamedNode::new_unchecked(format!("{EX}{local}")))
}

/// N12 (RSH-02; Sol r1, F1): a reader who sees `a p d` by its public support gets the same
/// explanation whatever a graph it can't read adds: a shorter secret derivation, through
/// asserted statements or an inferred intermediate, is neither chosen nor hinted at.
#[test]
fn a_supported_explanation_is_the_same_whatever_secret_graphs_hold() {
    let explain = |store: &StoreService| {
        let [a, p, d] = chain_statement();
        store
            .explain_statement(
                Ruleset::Owl2Rl,
                &reader(true),
                a.as_ref().into(),
                p.as_ref().into(),
                d.as_ref().into(),
            )
            .unwrap()
            .expect("a public derivation")
    };
    let public = explain(&chain_store(""));
    assert!(
        public.iter().all(|step| step.origin != "hidden"),
        "{public:#?}"
    );
    for private in SHORTCUTS {
        assert_eq!(explain(&chain_store(private)), public, "{private}");
    }
}

/// N12 (Sol r1, F2): a supported reader's justifications of `a p d`, in every mode, with
/// their completeness and verification, are the same whatever a graph it can't read adds.
/// A secret shortcut changed `one`'s members, `all`'s count and `core` (with no hidden
/// member at all): the sets were enumerated over every graph, then redacted.
#[test]
fn supported_justifications_are_the_same_whatever_secret_graphs_hold() {
    use nrese_store::reasoning::JustificationMode;
    let justify = |store: &StoreService| {
        let [a, p, d] = chain_statement();
        [
            JustificationMode::One,
            JustificationMode::All,
            JustificationMode::Core,
            JustificationMode::Union,
            JustificationMode::Top(2),
        ]
        .map(|mode| {
            store
                .justify_statement(
                    Ruleset::Owl2Rl,
                    &reader(true),
                    [a.as_ref().into(), p.as_ref().into(), d.as_ref().into()],
                    mode,
                )
                .unwrap()
                .expect("a public justification")
        })
    };
    let public = justify(&chain_store(""));
    assert!(
        public.iter().all(|answer| answer.verified != Some(false)),
        "{public:#?}"
    );
    for private in SHORTCUTS {
        assert_eq!(justify(&chain_store(private)), public, "{private}");
    }
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
/// A statement in the tests' Turtle shorthand (`ex:`, `owl:`, `a`), as terms.
fn statement_of(text: &str) -> [nrese_rdf::NamedNode; 3] {
    let term = |name: &str| {
        nrese_rdf::NamedNode::new_unchecked(match name.split_once(':') {
            Some(("ex", local)) => format!("{EX}{local}"),
            Some(("owl", local)) => format!("http://www.w3.org/2002/07/owl#{local}"),
            _ if name == "a" => "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_owned(),
            _ => panic!("{name}: not in the shorthand"),
        })
    };
    let names: Vec<&str> = text.split_whitespace().collect();
    [term(names[0]), term(names[1]), term(names[2])]
}

/// What `scope` is told of how `statement` holds: its explanation and its justifications
/// in every mode (N12, with the hidden-assertion rows of item 18).
fn proofs(
    store: &StoreService,
    scope: &ReadScope,
    statement: &str,
) -> (
    Option<Vec<nrese_store::reasoning::InferenceStep>>,
    Vec<Option<nrese_store::reasoning::JustificationAnswer>>,
) {
    use nrese_store::reasoning::JustificationMode;
    let [s, p, o] = statement_of(statement);
    let explained = store
        .explain_statement(
            Ruleset::Owl2Rl,
            scope,
            s.as_ref().into(),
            p.as_ref().into(),
            o.as_ref().into(),
        )
        .unwrap();
    let justified = [
        JustificationMode::One,
        JustificationMode::All,
        JustificationMode::Core,
        JustificationMode::Union,
        JustificationMode::Top(2),
    ]
    .into_iter()
    .map(|mode| {
        store
            .justify_statement(
                Ruleset::Owl2Rl,
                scope,
                [s.as_ref().into(), p.as_ref().into(), o.as_ref().into()],
                mode,
            )
            .unwrap()
    })
    .collect();
    (explained, justified)
}

/// Also its explanation and justifications (N12): what a reader is told of how the statement
/// holds doesn't depend on the hidden assertion either.
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
    // `statement`: the row's secret one, asked for whether or not `secret` asserts it.
    let answers = |open: &str,
                   secret: Option<&str>,
                   statement: &str,
                   ask: &str,
                   count: &str,
                   scope: &ReadScope| {
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
        (query(ask), query(count), proofs(&store, scope, statement))
    };
    for (by_support, mode) in [(true, "supported"), (false, "visible")] {
        let scope = reader(by_support);
        for (open, secret, ask, count) in rows {
            let public = answers(open, None, secret, ask, count, &scope);
            assert!(
                public.0.contains("true") && public.2.0.is_some(),
                "{mode}: {ask} derivable from open, and explained: {public:?}"
            );
            let hidden = answers(open, Some(secret), secret, ask, count, &scope);
            assert_eq!(hidden, public, "{mode}: {secret} asserted in secret");
        }
    }
}

/// The same across commits, live: an unrestricted writer asserts the statement in `secret`
/// and later retracts it; after each commit a reader's answers are what they were. The sets
/// and the re-derived facts are updated for the commits, the views made afresh.
#[test]
fn a_hidden_assertion_committed_later_does_not_change_what_a_reader_derives() {
    use nrese_reasoner::{ReasonerConfig, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
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
                .into_iter()
                // And how it holds (N12): explanation and justifications.
                .chain([format!(
                    "{:?}",
                    proofs(pipeline.store(), scope, "ex:alice a ex:Patient")
                )])
            })
            .collect()
    };
    let before = seen();
    assert!(
        before[0].contains("true") && before[3].contains("true"),
        "{before:?}"
    );
    assert!(
        before[2].starts_with("(Some(") && before[5].starts_with("(Some("),
        "explained: {before:?}"
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
    use nrese_reasoner::{ReasonerConfig, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
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

/// Without rules registered the sets can't be computed: no inferred statement is shown.
#[test]
fn without_rules_nothing_inferred_is_shown() {
    let store = store();
    store.use_reasoning_rules(None);
    assert!(classes(&store, &reader(true), "a").is_empty());
    assert_eq!(classes(&store, &reader(false), "a"), ["<D>", "<E>", "<F>"]);
}

/// A known view delta survives a representation rebuild; an unknown delta gets no
/// lineage edge. Corpus members always equal the actual restricted scans.
#[test]
fn supported_search_lineage_survives_mask_rebuilds_but_not_unknown_deltas() {
    use nrese_engine::{GraphSelector, QuadPattern, ReadModel, TermId, TermKind, TextScope};
    use nrese_reasoner::{ReasonerConfig, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};

    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    );
    let apply = |data: String| {
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                     INSERT DATA {{ {data} }}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap();
    };
    let public: String = (0..40)
        .map(|i| format!("ex:s{i} ex:p \"word{i}\" . "))
        .collect();
    apply(format!(
        "ex:p rdfs:subPropertyOf ex:q . GRAPH ex:open {{ {public} }}
         GRAPH ex:secret {{ ex:hidden ex:p \"secret\" }}"
    ));
    let ReadScope::Graphs(access) = reader(true) else {
        unreachable!();
    };
    let check = || {
        let snapshot = pipeline.store().read_snapshot(Some(&access)).unwrap();
        let model = ReadModel::Inferred;
        let pattern = QuadPattern::in_graph(TermId::DEFAULT_GRAPH);
        let mut expected: Vec<_> = snapshot
            .quads_for_pattern_in(model, &pattern)
            .filter(|quad| quad.object.kind() == TermKind::String)
            .map(|quad| quad.object.payload())
            .collect();
        expected.sort_unstable();
        expected.dedup();
        let scope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
        let (members, documents, _) = snapshot.text_corpus_members(model, &scope, false);
        assert_eq!(members, expected);
        assert_eq!(documents, expected.len() as u64);
        assert_eq!(documents, 40, "only the open graph's derived literals");
    };
    check();
    let initial = pipeline.store().engine_stats().text_corpora;
    let patched = pipeline.store().support_statistics().patched;
    // Each newly hidden inference adds a mask run. Going past eight rebuilds the
    // representation; the complete content delta must still derive the next corpus.
    for i in 0..12 {
        apply(format!(
            "GRAPH ex:secret {{ ex:hidden{i} ex:p \"secret{i}\" }}"
        ));
        check();
    }
    let current = pipeline.store().engine_stats().text_corpora;
    assert_eq!(current.walked, initial.walked, "no lost continuation");
    assert_eq!(current.derived, initial.derived + 12);
    let patched = pipeline.store().support_statistics().patched - patched;
    assert!(
        patched > 0 && patched < 12,
        "mask rebuild reached: {patched}"
    );

    // A hidden assertion removes a publicly derivable fact from the global inferred
    // stack. The view retains that derived extra, but changes_since cannot certify its
    // delta. It must walk afresh, with the same exact visible corpus.
    apply("GRAPH ex:secret { ex:s0 ex:q \"word0\" }".to_owned());
    check();
    let unknown = pipeline.store().engine_stats().text_corpora;
    assert_eq!(unknown.walked, current.walked + 1);
    assert_eq!(unknown.derived, current.derived);
}

/// Commits through the mutation pipeline update the sets instead of computing them
/// afresh: after every commit, a reader sees what a store forced to compute them sees,
/// and so does a reader of a store that keeps one set per statement (the cap's marks
/// updated with the sets, the views patched or, where a mark leaves a statement open,
/// made afresh).
#[test]
fn updated_sets_equal_fresh_ones_across_commits() {
    use nrese_reasoner::{ReasonerConfig, ReasoningMode};
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
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
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
