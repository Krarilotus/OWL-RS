//! Full-text corpora (`Snapshot::text_search_in`): a search ranks among the literals its
//! snapshot's statements in its scope hold, so neither an open transaction, nor a later
//! version, nor another scope changes its scores; a replica answers as its primary does.
//! The corpus is walked once, kept, and derived across commits from the last one; the
//! kept ones are bounded (performance.md §0).

use nrese_engine::{
    DurabilityConfig, EncodedQuad, EncodedTriple, Engine, EngineConfig, GraphSelector,
    InferredSubset, ReadModel, Snapshot, SyncPolicy, TermId, TextMatch, TextQuery, TextScope,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

fn label(subject: &str, text: &str, graph: Option<&str>) -> Quad {
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

fn query(text: &str) -> TextQuery {
    TextQuery {
        text: text.to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    }
}

/// The matches as texts with their relevance (ids differ between engines).
fn texts(snapshot: &Snapshot, found: &[TextMatch]) -> Vec<(String, f64)> {
    found
        .iter()
        .map(|m| match snapshot.decode(TermId::from_raw(m.id)) {
            Some(Term::Literal(l)) => (l.value().to_owned(), m.relevance),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn base(engine: &Engine) {
    let mut tx = engine.transaction();
    tx.insert(label("a", "alpha one two", None).as_ref());
    tx.insert(label("b", "beta", None).as_ref());
    for i in 0..8 {
        tx.insert(label(&format!("f{i}"), &format!("filler {i}"), None).as_ref());
    }
    tx.insert(label("g", "beta in g", Some("g")).as_ref());
    tx.commit().unwrap();
}

/// The corpora built so far: by a walk, and derived from an ancestor's.
fn built(snapshot: &Snapshot) -> (u64, u64) {
    let stats = snapshot.text_corpus_stats();
    (stats.walked, stats.derived)
}

/// Guard (performance.md §0): one walk per scope, then the kept corpus; an open
/// transaction builds nothing; after a commit the corpus is derived from the last one (a
/// probe per term the commit touched), not walked again.
#[test]
fn corpora_are_walked_once_then_derived_across_commits() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let default = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let model = ReadModel::Materialised;
    let every = snapshot.text_search(&query("alpha beta"));
    assert_eq!(built(&snapshot), (1, 0));
    assert_eq!(snapshot.text_search(&query("beta")).len(), 2);
    assert_eq!(built(&snapshot), (1, 0));
    let in_default = snapshot.text_search_in(model, &default, &query("alpha beta"));
    assert_eq!(built(&snapshot), (2, 0));
    assert_eq!(in_default.len(), 2);
    assert_eq!(every.len(), 3);
    // Graph g's literal counts in every graph, not in the default graph's.
    assert_ne!(
        texts(&snapshot, &every)[..2],
        texts(&snapshot, &in_default)[..]
    );

    let mut tx = engine.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    assert_eq!(snapshot.text_search(&query("alpha beta")), every);
    assert_eq!(
        snapshot.text_search_in(model, &default, &query("alpha beta")),
        in_default
    );
    assert_eq!(built(&snapshot), (2, 0));
    tx.commit().unwrap();
    // The old version keeps its corpus; the new one derives its own from it.
    assert_eq!(snapshot.text_search(&query("alpha beta")), every);
    assert_eq!(built(&snapshot), (2, 0));
    let later = engine.snapshot();
    assert_eq!(later.text_search(&query("alpha beta")).len(), 63);
    assert_eq!(built(&later), (2, 1));
    assert_eq!(
        later.text_search_in(model, &default, &query("beta")).len(),
        61
    );
    assert_eq!(built(&later), (2, 2));
    // Three more commits without a search between: derived over the three steps.
    for i in 0..3 {
        let mut tx = engine.transaction();
        tx.insert(label(&format!("y{i}"), &format!("beta more {i}"), None).as_ref());
        tx.commit().unwrap();
    }
    let latest = engine.snapshot();
    assert_eq!(latest.text_search(&query("beta")).len(), 65);
    assert_eq!(built(&latest), (2, 3));
    // Autocompletion's other index (IRIs by local name, among the subjects) has its own.
    let names = latest.iri_search_in(model, &TextScope::EVERY, &query("x1"));
    assert_eq!(names.len(), 1);
    assert_eq!(built(&latest), (3, 3));
}

/// Guard: the kept corpora stay within the engine's budget and count, least recently used
/// out; scopes of equal graph sets share one; what they hold is in the engine's stats.
#[test]
fn kept_corpora_are_bounded_and_shared_by_equal_graph_sets() {
    let engine_with = |budget| {
        let engine = Engine::new(EngineConfig {
            text_corpus_bytes: budget,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        for g in 0..80 {
            for i in 0..20 {
                let text = format!("word{i} graph{g}");
                let graph = format!("g{g}");
                tx.insert(label(&format!("s{g}-{i}"), &text, Some(&graph)).as_ref());
            }
        }
        tx.commit().unwrap();
        engine
    };
    let model = ReadModel::Materialised;
    let graph = |snapshot: &Snapshot, g: usize| {
        let iri = NamedNode::new_unchecked(format!("http://example.com/g{g}"));
        snapshot.lookup(iri.as_ref().into()).unwrap()
    };
    let engine = engine_with(1 << 30);
    let snapshot = engine.snapshot();
    // One graph, written three ways: one corpus.
    let g0 = graph(&snapshot, 0);
    for scope in [
        TextScope::Graphs(GraphSelector::Exact(g0)),
        TextScope::Merge(vec![g0]),
        TextScope::Merge(vec![g0, g0]),
    ] {
        assert_eq!(
            snapshot
                .text_search_in(model, &scope, &query("word1"))
                .len(),
            1
        );
    }
    assert_eq!(built(&snapshot), (1, 0));
    // Two graphs in either order: one corpus.
    let (g1, g2) = (graph(&snapshot, 1), graph(&snapshot, 2));
    for graphs in [vec![g1, g2], vec![g2, g1]] {
        let found = snapshot.text_search_in(model, &TextScope::Merge(graphs), &query("word1"));
        assert_eq!(found.len(), 2);
    }
    assert_eq!(built(&snapshot), (2, 0));
    // More scopes than are kept: at most the count.
    for g in 0..80 {
        let scope = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, g)));
        snapshot.text_search_in(model, &scope, &query("word1"));
    }
    let stats = engine.stats().text_corpora;
    assert_eq!(stats.kept, 64, "{stats:?}");
    assert!(stats.bytes > 0 && stats.bytes <= stats.budget, "{stats:?}");
    // A budget of a few corpora's bytes: never more held than it.
    let one = stats.bytes / stats.kept;
    let engine = engine_with(4 * one as usize);
    let snapshot = engine.snapshot();
    for g in 0..80 {
        let scope = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, g)));
        snapshot.text_search_in(model, &scope, &query("word1"));
        let stats = snapshot.text_corpus_stats();
        assert!(stats.bytes <= stats.budget, "{stats:?}");
        assert!(stats.kept <= 4, "{stats:?}");
    }
    // The most recently used stays: searching it again builds nothing.
    let last = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, 79)));
    let before = built(&snapshot);
    snapshot.text_search_in(model, &last, &query("word2"));
    assert_eq!(built(&snapshot), before);
}

/// A small random generator for the differential test (xorshift).
struct Random(u64);

impl Random {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Differential: after random commits (inserts, deletes, a literal moved between graphs,
/// inferred statements added and retracted; searches after some commits, none after
/// others), every corpus a search uses equals one walked afresh, in every scope and
/// model; and so do those of pending states and of restricted views brought forward.
#[test]
fn derived_corpora_equal_walked_ones_after_random_commits() {
    for seed in [0x9e37_79b9_7f4a_7c15, 7, 0xdead_beef, 1 << 40] {
        random_commits(seed);
    }
}

fn random_commits(seed: u64) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let graphs = [None, Some("g1"), Some("g2"), Some("g3")];
    let words = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let mut random = Random(seed);
    // The asserted statements, as (subject, text, graph).
    let mut asserted: Vec<(String, String, Option<&str>)> = Vec::new();
    let mut inferred: Vec<EncodedTriple> = Vec::new();
    let mut view: Option<(Snapshot, Snapshot, Vec<EncodedQuad>)> = None;
    for round in 0..60 {
        let mut tx = engine.transaction();
        for _ in 0..1 + random.below(6) {
            match random.below(10) {
                // A new statement (texts repeat, so a literal may stay in use elsewhere).
                0..=4 => {
                    let a = words[random.below(5) as usize];
                    let text = format!("{a} {}", words[random.below(5) as usize]);
                    let subject = format!("s{}", random.below(12));
                    let graph = graphs[random.below(4) as usize];
                    if tx.insert(label(&subject, &text, graph).as_ref()) {
                        asserted.push((subject, text, graph));
                    }
                }
                // A statement removed.
                5 | 6 if !asserted.is_empty() => {
                    let at = random.below(asserted.len() as u64) as usize;
                    let (subject, text, graph) = asserted.swap_remove(at);
                    tx.remove(label(&subject, &text, graph).as_ref());
                }
                // A statement moved to another graph.
                7 if !asserted.is_empty() => {
                    let at = random.below(asserted.len() as u64) as usize;
                    let (subject, text, graph) = asserted.swap_remove(at);
                    tx.remove(label(&subject, &text, graph).as_ref());
                    let graph = graphs[random.below(4) as usize];
                    if tx.insert(label(&subject, &text, graph).as_ref()) {
                        asserted.push((subject, text, graph));
                    }
                }
                // An inferred statement (default graph).
                8 => {
                    let a = words[random.below(5) as usize];
                    let quad = label(
                        &format!("i{}", random.below(6)),
                        &format!("{a} inferred"),
                        None,
                    );
                    let triple = EncodedTriple {
                        subject: tx.intern(quad.subject.as_ref().into()),
                        predicate: tx.intern(quad.predicate.as_ref().into()),
                        object: tx.intern(quad.object.as_ref()),
                    };
                    if tx.insert_inferred(triple) {
                        inferred.push(triple);
                    }
                }
                // One retracted.
                _ if !inferred.is_empty() => {
                    let at = random.below(inferred.len() as u64) as usize;
                    tx.remove_inferred(inferred.swap_remove(at));
                }
                _ => {}
            }
        }
        let pending = tx.pending_snapshot();
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let id = |g: &str| {
            let iri = NamedNode::new_unchecked(format!("http://example.com/{g}"));
            snapshot.lookup(iri.as_ref().into())
        };
        let mut scopes = vec![
            TextScope::EVERY,
            TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            TextScope::Graphs(GraphSelector::AnyNamed),
        ];
        if let (Some(g1), Some(g2)) = (id("g1"), id("g2")) {
            scopes.push(TextScope::Graphs(GraphSelector::Exact(g1)));
            scopes.push(TextScope::Merge(vec![TermId::DEFAULT_GRAPH, g2]));
            scopes.push(TextScope::Merge(vec![g1, g2]));
        }
        let models = [
            ReadModel::Materialised,
            ReadModel::Asserted,
            ReadModel::Inferred,
        ];
        // Searches after some commits only: the others are derived over several steps.
        if random.below(3) != 0 {
            for scope in &scopes {
                for model in models {
                    let derived = snapshot.text_corpus_members(model, scope, false);
                    let walked = snapshot.text_corpus_members(model, scope, true);
                    assert_eq!(
                        derived, walked,
                        "seed {seed}, round {round}, {model:?}, {scope:?}"
                    );
                }
            }
            let every = TextScope::EVERY;
            let model = ReadModel::Materialised;
            let derived = pending.text_corpus_members(model, &every, false);
            let walked = pending.text_corpus_members(model, &every, true);
            assert_eq!(derived, walked, "seed {seed}, round {round}, pending");
        }
        // A reader who sees some inferred statements: its view brought forward.
        let hidden: Vec<EncodedQuad> = inferred
            .iter()
            .filter(|_| random.below(2) == 0)
            .map(|triple| triple.in_default_graph())
            .collect();
        let now = snapshot.with_inferred_subset(InferredSubset::Without(hidden.clone()));
        if let Some((earlier, earlier_base, earlier_hidden)) = &view {
            let mut flipped: Vec<EncodedQuad> = hidden
                .iter()
                .filter(|q| !earlier_hidden.contains(q))
                .chain(earlier_hidden.iter().filter(|q| !hidden.contains(q)))
                .copied()
                .collect();
            flipped.sort_unstable();
            now.continues(&snapshot, earlier, earlier_base, &flipped);
        }
        let every = TextScope::EVERY;
        let derived = now.text_corpus_members(ReadModel::Materialised, &every, false);
        let walked = now.text_corpus_members(ReadModel::Materialised, &every, true);
        assert_eq!(derived, walked, "seed {seed}, round {round}, view");
        view = Some((now, snapshot, hidden));
    }
    let stats = engine.stats().text_corpora;
    assert!(stats.derived > stats.walked, "{stats:?}");
}

/// The merge of graphs and every named graph: their literals, each once.
#[test]
fn scopes_of_several_graphs() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    tx.insert(label("h", "beta in h", Some("h")).as_ref());
    tx.insert(label("h2", "beta in g", Some("h")).as_ref());
    tx.insert(label("d", "beta in h", None).as_ref());
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let id = |g: &str| {
        snapshot
            .lookup(
                NamedNode::new_unchecked(format!("http://example.com/{g}"))
                    .as_ref()
                    .into(),
            )
            .unwrap()
    };
    let found = |scope: &TextScope| {
        let mut found: Vec<String> = texts(
            &snapshot,
            &snapshot.text_search_in(ReadModel::Materialised, scope, &query("beta")),
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect();
        found.sort();
        found
    };
    let mut g_h = vec![id("g"), id("h")];
    g_h.sort_unstable();
    assert_eq!(found(&TextScope::Merge(g_h)), ["beta in g", "beta in h"]);
    assert_eq!(
        found(&TextScope::Graphs(GraphSelector::AnyNamed)),
        ["beta in g", "beta in h"]
    );
    assert_eq!(
        found(&TextScope::Merge(vec![TermId::DEFAULT_GRAPH])),
        ["beta", "beta in h"]
    );
    assert_eq!(found(&TextScope::EVERY), ["beta", "beta in g", "beta in h"]);
}

/// A replica answers as its primary, whatever the primary's open transactions interned.
#[test]
fn a_replica_scores_as_its_primary() {
    let config = || EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    };
    let (primary_dir, replica_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), config()).unwrap();
    base(&primary);
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), config()).unwrap();
    let mut tx = primary.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    let (p, r) = (primary.snapshot(), replica.snapshot());
    let q = query("alpha beta");
    assert_eq!(p.text_search(&q), r.text_search(&q));
    assert_eq!(texts(&p, &p.text_search(&q))[0].0, "beta");
    drop(tx);
}
