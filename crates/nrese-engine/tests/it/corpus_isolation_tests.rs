//! A retained corpus stays exact across commits that touch only data the reader can't
//! see (SYN-24 review r1 F2; design note I2, I3). A reader of the default graph keeps a
//! text and an IRI corpus; then commits add, remove and repeat words and subjects in a
//! hidden graph, some of them the same literal the reader sees. The searches after them
//! derive their corpora from the kept ones. Every score must equal an independent
//! engine's that holds the reader's statements alone (an oracle that shares no code with
//! the corpus: a fresh dictionary and index), text and IRI alike: the derived document
//! count and word total are what the scores are made of.

use nrese_engine::{
    CorpusTuning, Engine, EngineConfig, GraphSelector, Snapshot, TermId, TextScope,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

use crate::text_scope_tests::{MODEL, built, query, texts};

const READER: TextScope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));

/// `subject`'s label `text` in `graph` (the default graph where `None`).
fn named(subject: &str, text: &str, graph: Option<&str>) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/{subject}")),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_simple_literal(text),
        match graph {
            Some(g) => NamedNode::new_unchecked(format!("http://example.com/{g}")).into(),
            None => GraphName::DefaultGraph,
        },
    )
}

/// What the reader sees: labels and subjects whose local names share words, and 30 more,
/// so that each corpus (33 documents) is worth deriving over the hidden commits' 13 touched
/// terms at one document a probe.
fn visible(engine: &Engine) {
    let mut tx = engine.transaction();
    tx.insert(named("alphaOne", "alpha one two", None).as_ref());
    tx.insert(named("betaTwo", "beta", None).as_ref());
    tx.insert(named("alphaBeta", "alpha beta gamma", None).as_ref());
    for i in 0..30 {
        tx.insert(named(&format!("filler{i}"), &format!("filler {i}"), None).as_ref());
    }
    tx.commit().unwrap();
}

/// IRI matches as their IRIs with their relevance (ids differ between engines).
fn iris(snapshot: &Snapshot, text: &str) -> Vec<(String, f64)> {
    snapshot
        .iri_search_in(MODEL, &READER, &query(text), None)
        .unwrap()
        .iter()
        .map(|m| match snapshot.decode(TermId::from_raw(m.id)) {
            Some(Term::NamedNode(n)) => (n.as_str().to_owned(), m.relevance),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn literals(snapshot: &Snapshot, text: &str) -> Vec<(String, f64)> {
    let found = snapshot
        .text_search_in(MODEL, &READER, &query(text), None)
        .unwrap();
    texts(snapshot, &found)
}

#[test]
fn scores_after_hidden_only_commits_equal_an_engine_without_them() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    // Corpora of a few documents: derived wherever the touched terms are at most their
    // documents, so that the derivation is what this checks.
    engine.set_corpus_tuning(CorpusTuning {
        documents_per_probe: 1,
        ..CorpusTuning::DEFAULT
    });
    visible(&engine);
    let oracle = Engine::new(EngineConfig::default()).unwrap();
    visible(&oracle);
    let truth = oracle.snapshot();
    let cases = ["alpha", "beta", "alpha beta", "gamma"];
    // The kept corpora, walked once.
    let before = engine.snapshot();
    for text in cases {
        assert_eq!(
            literals(&before, text),
            literals(&truth, text),
            "{text}, before"
        );
        assert_eq!(iris(&before, text), iris(&truth, text), "{text}, before");
    }
    let (walked, _) = built(&before);
    // Commits that touch only the hidden graph: new words and subjects, a literal the
    // reader sees ("beta"), one hidden statement added and later removed.
    let mut tx = engine.transaction();
    for i in 0..4 {
        tx.insert(named(&format!("alphaSecret{i}"), "alpha secret", Some("hidden")).as_ref());
    }
    tx.insert(named("betaHidden", "beta", Some("hidden")).as_ref());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.insert(named("gammaHidden", "gamma gamma", Some("hidden")).as_ref());
    tx.remove(named("alphaSecret0", "alpha secret", Some("hidden")).as_ref());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.remove(named("gammaHidden", "gamma gamma", Some("hidden")).as_ref());
    tx.commit().unwrap();
    let after = engine.snapshot();
    let mut failed = Vec::new();
    for text in cases {
        let (got, want) = (literals(&after, text), literals(&truth, text));
        if got != want {
            failed.push(format!("text {text}: {got:?} != {want:?}"));
        }
        let (got, want) = (iris(&after, text), iris(&truth, text));
        if got != want {
            failed.push(format!("IRI {text}: {got:?} != {want:?}"));
        }
    }
    // Derived, not walked again: the derivation is what was compared.
    let (walked_after, derived) = built(&after);
    if walked_after != walked || derived < 2 {
        failed.push(format!(
            "walked {walked} then {walked_after}, derived {derived}: {:?}",
            after.text_corpus_stats()
        ));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
