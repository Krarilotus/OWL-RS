//! Inferred statements under graph access with `inferred = "supported"`: a reader sees an
//! inferred statement when one of its derivations uses graphs it may read alone (support
//! graph sets, `nrese_store::support`), in every read path.

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
        store.explain_statement(
            Ruleset::Owl2Rl,
            &supported,
            nrese_rdf::NamedNode::new_unchecked(format!("{EX}{subject}"))
                .as_ref()
                .into(),
            nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
                .as_ref()
                .into(),
            nrese_rdf::NamedNode::new_unchecked(format!("{EX}{class}"))
                .as_ref()
                .into(),
        )
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
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "PREFIX ex: <{EX}> INSERT DATA {{ GRAPH ex:open {{ ex:c a ex:C }} }}"
        )))
        .unwrap();
    // The rematerialisation prepares the new revision's sets in the background (they
    // were in use); the read waits for that computation instead of starting another.
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    assert_eq!(classes(&store, &supported, "c"), ["<D>", "<E>"]);
    assert_eq!(store.support_statistics().computed, 2);
}

/// Without rules registered the sets can't be computed: no inferred statement is shown.
#[test]
fn without_rules_nothing_inferred_is_shown() {
    let store = store();
    store.use_reasoning_rules(None);
    assert!(classes(&store, &reader(true), "a").is_empty());
    assert_eq!(classes(&store, &reader(false), "a"), ["<D>", "<E>", "<F>"]);
}

/// Commits through the mutation pipeline update the sets instead of computing them
/// afresh: after every commit, a reader sees what a store forced to compute them sees.
#[test]
fn updated_sets_equal_fresh_ones_across_commits() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationCommand, MutationPipeline, MutationTicket, Requester};
    let pipeline = || {
        MutationPipeline::new(
            Arc::new(StoreService::new(in_memory_store_config()).unwrap()),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        )
    };
    let (updating, fresh) = (pipeline(), pipeline());
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
    // A reader of one graph (it misses most: a list of what it sees) and one of two (it
    // sees most: a mask of what it misses).
    let narrow = reader(true);
    let broad = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open"), format!("{EX}secret")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let seen = |pipeline: &MutationPipeline| -> Vec<String> {
        let mut seen = Vec::new();
        for (name, reader) in [("narrow", &narrow), ("broad", &broad)] {
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
        // Forget the fresh store's sets: they are computed again.
        fresh
            .store()
            .use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        assert_eq!(
            seen(&updating),
            seen(&fresh),
            "after commit {step}: {commit}"
        );
    }
    let statistics = updating.store().support_statistics();
    assert!(statistics.updated >= 6, "{statistics:?}");
    assert!(statistics.patched >= 10, "{statistics:?}");
    assert!(statistics.computed >= 2, "{statistics:?}");
}
