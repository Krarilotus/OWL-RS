//! Reader-scoped search corpora (`Snapshot::text_search_in`, `iri_search_in`,
//! `vector_search_in`; `SearchScope::Reader`): a search ranks among the documents its
//! snapshot's statements in its scope hold, so neither an open transaction, nor a later
//! version, nor another scope changes its scores; a replica answers as its primary does.
//! What a corpus holds is what the reader's scans show, equality classes as it reads them.
//! The corpus is walked once, kept, and derived across commits from the last one; the kept
//! ones are bounded. An engine whose searches are all global keeps none and records nothing.
//!
//! Oracles: [`visible`] reads the statements as the reader's own scans do, independently of
//! the corpus code; scores are compared against the same search before a change, or against
//! a store without the hidden data.

use std::sync::Arc;

use nrese_engine::{
    DurabilityConfig, EncodedQuad, EncodedTriple, Engine, EngineConfig, GraphSelector,
    InferredSubset, QuadPattern, ReadModel, Snapshot, SyncPolicy, TermId, TermKind, TextMatch,
    TextQuery, TextScope,
};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

/// `subject`'s label `text`: a vector literal where it is one (`[1, 2]`).
pub(crate) fn label(subject: &str, text: &str, graph: Option<&str>) -> Quad {
    let literal = match text.starts_with('[') {
        true => Literal::new_typed_literal(
            text,
            NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
        ),
        false => Literal::new_simple_literal(text),
    };
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/{subject}")),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        literal,
        match graph {
            Some(g) => NamedNode::new_unchecked(format!("http://example.com/{g}")).into(),
            None => GraphName::DefaultGraph,
        },
    )
}

pub(crate) fn query(text: &str) -> TextQuery {
    TextQuery {
        text: text.to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    }
}

pub(crate) const EVERY: TextScope = TextScope::EVERY;
pub(crate) const MODEL: ReadModel = ReadModel::Materialised;

/// A reader's search of every graph.
pub(crate) fn every(snapshot: &Snapshot, text: &str) -> Vec<TextMatch> {
    snapshot
        .text_search_in(MODEL, &EVERY, &query(text), None)
        .unwrap()
}

/// The matches as texts with their relevance (ids differ between engines).
pub(crate) fn texts(snapshot: &Snapshot, found: &[TextMatch]) -> Vec<(String, f64)> {
    found
        .iter()
        .map(|m| match snapshot.decode(TermId::from_raw(m.id)) {
            Some(Term::Literal(l)) => (l.value().to_owned(), m.relevance),
            other => panic!("{other:?}"),
        })
        .collect()
}

pub(crate) fn base(engine: &Engine) {
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
pub(crate) fn built(snapshot: &Snapshot) -> (u64, u64) {
    let stats = snapshot.text_corpus_stats();
    (stats.walked, stats.derived)
}

/// Global searches only (the default `SearchScope::Global`): the engine never makes its
/// store of corpora, and commits, pending states and views record no lineage (design note
/// I1: no corpus work on a global-only store). A first reader search makes it.
#[test]
fn a_global_engine_keeps_no_corpora() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.text_search(&query("alpha beta")).len(), 3);
    assert_eq!(snapshot.iri_search(&query("alpha")).len(), 0);
    let mut tx = engine.transaction();
    tx.insert(label("c", "gamma", None).as_ref());
    let _pending = tx.pending_snapshot();
    tx.commit().unwrap();
    let _view = engine
        .snapshot()
        .with_inferred_subset(InferredSubset::Without(Vec::new()));
    let stats = engine.snapshot().text_corpus_stats();
    assert!(
        !stats.started && stats.steps == 0 && stats.kept == 0,
        "{stats:?}"
    );
    // The first reader search makes it; then commits record their steps.
    every(&engine.snapshot(), "gamma");
    let mut tx = engine.transaction();
    tx.insert(label("d", "delta", None).as_ref());
    tx.commit().unwrap();
    let stats = engine.snapshot().text_corpus_stats();
    assert!(
        stats.started && stats.steps == 1 && stats.kept == 1,
        "{stats:?}"
    );
}

/// Guard: one walk per scope, then the kept corpus; an open transaction builds nothing;
/// after a commit the corpus is derived from the last one (a probe per term the commit
/// touched), not walked again. 1,300 labels that match no query make the corpora large
/// enough to be worth deriving over the 60-label commit (120 terms) under the default 10
/// documents a probe (master 7 Nov 18:16).
#[test]
fn corpora_are_walked_once_then_derived_across_commits() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let mut tx = engine.transaction();
    for i in 0..1300 {
        tx.insert(label(&format!("pad{i}"), &format!("pad {i}"), None).as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let default = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let all = every(&snapshot, "alpha beta");
    assert_eq!(built(&snapshot), (1, 0));
    assert_eq!(every(&snapshot, "beta").len(), 2);
    assert_eq!(built(&snapshot), (1, 0));
    let in_default = snapshot
        .text_search_in(MODEL, &default, &query("alpha beta"), None)
        .unwrap();
    assert_eq!(built(&snapshot), (2, 0));
    assert_eq!(in_default.len(), 2);
    assert_eq!(all.len(), 3);
    // Graph g's literal counts in every graph, not in the default graph's.
    assert_ne!(
        texts(&snapshot, &all)[..2],
        texts(&snapshot, &in_default)[..]
    );

    let mut tx = engine.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    assert_eq!(every(&snapshot, "alpha beta"), all);
    assert_eq!(
        snapshot
            .text_search_in(MODEL, &default, &query("alpha beta"), None)
            .unwrap(),
        in_default
    );
    assert_eq!(built(&snapshot), (2, 0));
    tx.commit().unwrap();
    // The old version keeps its corpus; the new one derives its own from it.
    assert_eq!(every(&snapshot, "alpha beta"), all);
    assert_eq!(built(&snapshot), (2, 0));
    let later = engine.snapshot();
    assert_eq!(every(&later, "alpha beta").len(), 63);
    assert_eq!(built(&later), (2, 1));
    assert_eq!(
        later
            .text_search_in(MODEL, &default, &query("beta"), None)
            .unwrap()
            .len(),
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
    assert_eq!(every(&latest, "beta").len(), 65);
    assert_eq!(built(&latest), (2, 3));
    // Autocompletion's other index (IRIs by local name, among the subjects) has its own.
    let names = latest
        .iri_search_in(MODEL, &EVERY, &query("x1"), None)
        .unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(built(&latest), (3, 3));
}

/// The kept corpora stay within the engine's budget, least recently used out, and the
/// budget is their only bound (no count: master 7 Nov 17:28, IB1727-3bc5); scopes of equal
/// graph sets share one.
#[test]
fn kept_corpora_are_bounded_and_shared_by_equal_graph_sets() {
    let engine_with = |budget: Option<usize>| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        if let Some(budget) = budget {
            engine.set_text_corpus_budget(Arc::new(nrese_exec::Budget::new(budget)));
        }
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
    let graph = |snapshot: &Snapshot, g: usize| {
        let iri = NamedNode::new_unchecked(format!("http://example.com/g{g}"));
        snapshot.lookup(iri.as_ref().into()).unwrap()
    };
    let search = |snapshot: &Snapshot, scope: &TextScope, text: &str| {
        snapshot
            .text_search_in(MODEL, scope, &query(text), None)
            .unwrap()
    };
    let engine = engine_with(None);
    let snapshot = engine.snapshot();
    // One graph, written three ways: one corpus.
    let g0 = graph(&snapshot, 0);
    for scope in [
        TextScope::Graphs(GraphSelector::Exact(g0)),
        TextScope::Merge(vec![g0]),
        TextScope::Merge(vec![g0, g0]),
    ] {
        assert_eq!(search(&snapshot, &scope, "word1").len(), 1);
    }
    assert_eq!(built(&snapshot), (1, 0));
    // Two graphs in either order: one corpus.
    let (g1, g2) = (graph(&snapshot, 1), graph(&snapshot, 2));
    for graphs in [vec![g1, g2], vec![g2, g1]] {
        assert_eq!(
            search(&snapshot, &TextScope::Merge(graphs), "word1").len(),
            2
        );
    }
    assert_eq!(built(&snapshot), (2, 0));
    // Eighty scopes more, all within the default budget: every one kept (the merge of g1
    // and g2 and the eighty single graphs, g0's among them, shared).
    for g in 0..80 {
        let scope = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, g)));
        search(&snapshot, &scope, "word1");
    }
    let stats = snapshot.text_corpus_stats();
    assert_eq!(stats.kept, 81, "{stats:?}");
    assert!(stats.bytes > 0 && stats.bytes <= stats.budget, "{stats:?}");
    // A budget of a few corpora's bytes: never more held than it.
    let one = stats.bytes / stats.kept;
    let engine = engine_with(Some(4 * one as usize));
    let snapshot = engine.snapshot();
    for g in 0..80 {
        let scope = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, g)));
        search(&snapshot, &scope, "word1");
        let stats = snapshot.text_corpus_stats();
        assert!(stats.bytes <= stats.budget, "{stats:?}");
        assert!(stats.kept <= 4, "{stats:?}");
    }
    // The most recently used stays: searching it again builds nothing.
    let last = TextScope::Graphs(GraphSelector::Exact(graph(&snapshot, 79)));
    let before = built(&snapshot);
    search(&snapshot, &last, "word2");
    assert_eq!(built(&snapshot), before);
}

/// The corpora's byte budget holds: a corpus larger than it serves its search and isn't
/// kept (the review's 8-byte case).
#[test]
fn a_corpus_larger_than_the_budget_is_not_kept() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(Arc::new(nrese_exec::Budget::new(8)));
    let mut tx = engine.transaction();
    for i in 0..512 {
        tx.insert(label(&format!("s{i}"), &format!("alpha item{i}"), None).as_ref());
    }
    tx.commit().unwrap();
    assert_eq!(every(&engine.snapshot(), "alpha").len(), 512);
    let stats = engine.snapshot().text_corpus_stats();
    assert!(stats.bytes <= stats.budget && stats.kept == 0, "{stats:?}");
}

/// A request's budget that can't hold the corpus refuses the search with its own refusal,
/// nothing kept from it; a later search with room answers as if it never ran.
#[test]
fn a_refused_corpus_is_neither_kept_nor_used() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let snapshot = engine.snapshot();
    let tight = Arc::new(nrese_exec::Budget::new(8));
    let refused = snapshot.text_search_in(MODEL, &EVERY, &query("alpha beta"), Some(&tight));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(tight.used(), 0, "a refusal holds nothing");
    let fresh = Engine::new(EngineConfig::default()).unwrap();
    base(&fresh);
    assert_eq!(
        texts(&snapshot, &every(&snapshot, "alpha beta")),
        texts(&fresh.snapshot(), &every(&fresh.snapshot(), "alpha beta"))
    );
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

/// The documents a corpus must hold: the terms at the documents' position (objects that are
/// string literals with a word; subjects that are IRIs whose local name has one) of the
/// statements `snapshot`'s own scans show in `scope`'s graphs in `model`. The oracle reads
/// statements as the reader does, equality classes expanded or canonical; read
/// canonically, a class whose representative is a subject holds every identity's local
/// name (each answered as the representative).
fn visible(snapshot: &Snapshot, model: ReadModel, scope: &TextScope, iris: bool) -> Vec<u64> {
    let graphs: Vec<GraphSelector> = match scope {
        TextScope::Graphs(graphs) => vec![*graphs],
        TextScope::Merge(graphs) => graphs.iter().map(|&g| GraphSelector::Exact(g)).collect(),
    };
    let has_words = |text: &str| text.chars().any(char::is_alphanumeric);
    let mut found = std::collections::BTreeSet::new();
    for graph in graphs {
        let pattern = QuadPattern {
            graph,
            ..QuadPattern::all()
        };
        for quad in snapshot.quads_for_pattern_in(model, &pattern) {
            let term = if iris { quad.subject } else { quad.object };
            let document = match snapshot.decode(term) {
                Some(Term::Literal(l)) if !iris => {
                    matches!(term.kind(), TermKind::String | TermKind::LangString)
                        && has_words(l.value())
                }
                Some(Term::NamedNode(n)) if iris => {
                    has_words(n.as_str().rsplit(['#', '/', ':']).next().unwrap_or(""))
                }
                _ => false,
            };
            if document {
                found.insert(term.payload());
            }
        }
    }
    let classes = snapshot
        .equality_classes()
        .filter(|_| iris && snapshot.reads_canonically() && model != ReadModel::Asserted);
    for (representative, identities) in classes.iter().flat_map(|classes| classes.iter()) {
        if !found.contains(&TermId::from_raw(representative).payload()) {
            continue;
        }
        for &identity in identities {
            let id = TermId::from_raw(identity);
            let named =
                |n: &NamedNode| has_words(n.as_str().rsplit(['#', '/', ':']).next().unwrap_or(""));
            if matches!(snapshot.decode(id), Some(Term::NamedNode(n)) if named(&n)) {
                found.insert(id.payload());
            }
        }
    }
    found.into_iter().collect()
}

/// Checks the corpus a search of `snapshot` uses, of literals and of IRIs, against
/// [`visible`]; and a vector search's, against the typed literals its scans show. A
/// mismatch is added to `failed`.
fn check(
    snapshot: &Snapshot,
    model: ReadModel,
    scope: &TextScope,
    context: &str,
    failed: &mut Vec<String>,
) {
    let graphs: Vec<GraphSelector> = match scope {
        TextScope::Graphs(graphs) => vec![*graphs],
        TextScope::Merge(graphs) => graphs.iter().map(|&g| GraphSelector::Exact(g)).collect(),
    };
    let mut typed = std::collections::BTreeSet::new();
    for graph in graphs {
        let pattern = QuadPattern {
            graph,
            ..QuadPattern::all()
        };
        for quad in snapshot.quads_for_pattern_in(model, &pattern) {
            if quad.object.kind() == TermKind::TypedLiteral {
                typed.insert(quad.object.payload());
            }
        }
    }
    let typed: Vec<u64> = typed.into_iter().collect();
    let vectors = snapshot.vector_corpus_members(model, scope);
    if vectors != typed {
        failed.push(format!(
            "{context}, {model:?}, {scope:?}, vectors: {vectors:?} != {typed:?}"
        ));
    }
    for iris in [false, true] {
        let (members, documents, _) = snapshot.text_corpus_members(model, scope, iris);
        let expected = visible(snapshot, model, scope, iris);
        if members != expected || documents != expected.len() as u64 {
            failed.push(format!(
                "{context}, {model:?}, {scope:?}, iris {iris}: {members:?} ({documents}) != {expected:?}"
            ));
        }
    }
}

/// Differential (design note I3): after random commits (inserts, deletes, a literal moved
/// between graphs, inferred statements and `owl:sameAs` links added and retracted; searches
/// after some commits, none after others), every corpus a search uses holds exactly what
/// the reader's scans show (walked or derived), in every scope and model, read as stored,
/// with equality classes expanded, and canonically; and so do those of pending states and
/// of restricted views. Every mismatch of every seed is reported.
#[test]
fn search_corpora_equal_the_visible_statements_after_random_commits() {
    let mut failed = Vec::new();
    for seed in [0x9e37_79b9_7f4a_7c15, 7, 0xdead_beef, 1 << 40] {
        random_commits(seed, &mut failed);
    }
    assert!(
        failed.is_empty(),
        "{} mismatches:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

fn random_commits(seed: u64, failed: &mut Vec<String>) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    // Corpora of a few dozen documents: derived wherever the touched terms are at most
    // their documents, so that derivation is checked against the walk's oracle.
    engine.set_corpus_tuning(nrese_engine::CorpusTuning {
        documents_per_probe: 1,
        ..nrese_engine::CorpusTuning::DEFAULT
    });
    let graphs = [None, Some("g1"), Some("g2"), Some("g3")];
    let words = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let mut random = Random(seed);
    // The subjects first, in order: s0..s5 represent s6..s11 (the smaller id).
    let (same_as, subjects) = {
        let tx = engine.transaction();
        let same_as = tx.intern(
            NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
                .as_ref()
                .into(),
        );
        let subjects: Vec<TermId> = (0..12)
            .map(|i| {
                let iri = NamedNode::new_unchecked(format!("http://example.com/s{i}"));
                tx.intern(iri.as_ref().into())
            })
            .collect();
        tx.commit().unwrap();
        (same_as, subjects)
    };
    // Reads expand the classes, as a store with equality by representatives does.
    engine.set_equality(Some(same_as));
    let mut asserted: Vec<(String, String, Option<&str>)> = Vec::new();
    let mut inferred: Vec<EncodedTriple> = Vec::new();
    // Of the 3 readers × 6 scopes × 3 models, 16 (within the 64 corpora kept).
    let mut chosen = std::collections::BTreeSet::new();
    while chosen.len() < 16 {
        chosen.insert(1 + random.below(54) as usize);
    }
    for round in 0..60 {
        let mut tx = engine.transaction();
        for _ in 0..1 + random.below(6) {
            match random.below(11) {
                // A new statement (texts repeat, so a literal may stay in use elsewhere).
                0..=4 => {
                    let a = words[random.below(5) as usize];
                    let text = match random.below(4) {
                        0 => format!("[{}, {}]", random.below(3), random.below(3)),
                        _ => format!("{a} {}", words[random.below(5) as usize]),
                    };
                    let subject = format!("s{}", random.below(12));
                    let graph = graphs[random.below(4) as usize];
                    if tx.insert(label(&subject, &text, graph).as_ref()) {
                        asserted.push((subject, text, graph));
                    }
                }
                5 | 6 if !asserted.is_empty() => {
                    let at = random.below(asserted.len() as u64) as usize;
                    let (subject, text, graph) = asserted.swap_remove(at);
                    tx.remove(label(&subject, &text, graph).as_ref());
                }
                7 if !asserted.is_empty() => {
                    let at = random.below(asserted.len() as u64) as usize;
                    let (subject, text, graph) = asserted.swap_remove(at);
                    tx.remove(label(&subject, &text, graph).as_ref());
                    let graph = graphs[random.below(4) as usize];
                    if tx.insert(label(&subject, &text, graph).as_ref()) {
                        asserted.push((subject, text, graph));
                    }
                }
                8 => {
                    let a = words[random.below(5) as usize];
                    let quad = label(
                        &format!("s{}", random.below(12)),
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
                9 => {
                    let i = random.below(6) as usize;
                    let triple = EncodedTriple {
                        subject: subjects[i + 6],
                        predicate: same_as,
                        object: subjects[i],
                    };
                    if tx.insert_inferred(triple) {
                        inferred.push(triple);
                    }
                }
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
        let readers = [
            ("stored", snapshot.stored()),
            ("expanded", snapshot.clone()),
            ("canonical", snapshot.with_canonical_equality()),
        ];
        // Searches after some commits only: the others are derived over several steps.
        if random.below(3) != 0 {
            let mut kind = 0usize;
            for (name, reader) in &readers {
                for scope in &scopes {
                    for model in models {
                        kind += 1;
                        if !chosen.contains(&kind) {
                            continue;
                        }
                        let context = format!("seed {seed}, round {round}, {name}");
                        check(reader, model, scope, &context, failed);
                    }
                }
            }
            for pending in [pending.stored(), pending.clone()] {
                let context = format!("seed {seed}, round {round}, pending");
                check(&pending, MODEL, &EVERY, &context, failed);
            }
        }
        // A reader who sees some inferred statements: its view, walked.
        let hidden: Vec<EncodedQuad> = inferred
            .iter()
            .filter(|t| t.predicate != same_as && random.below(2) == 0)
            .map(|triple| triple.in_default_graph())
            .collect();
        let view = snapshot.with_inferred_subset(InferredSubset::Without(hidden));
        let context = format!("seed {seed}, round {round}, view");
        check(&view, MODEL, &EVERY, &context, failed);
    }
    // Walked: the first of each kind and any after a `sameAs` change (and each view); the
    // rest derived.
    let stats = engine.snapshot().text_corpus_stats();
    if stats.derived == 0 {
        failed.push(format!("seed {seed}: nothing derived, {stats:?}"));
    }
}

/// The review's (18 October) cases of equality by representatives, one per row: a search
/// ranks among what the reader's scans show, as stored, expanded or canonical. Each row:
/// its statements (asserted and inferred), the model and scope searched, literals or IRIs,
/// the query, and the matches a reader that expands classes gets.
#[test]
fn equality_readers_search_what_their_scans_show() {
    const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
    type Statement = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
        bool,
    );
    struct Case {
        name: &'static str,
        /// (subject, predicate or "label", object or text, graph or None, inferred).
        statements: &'static [Statement],
        model: ReadModel,
        default_graph_only: bool,
        iris: bool,
        query: &'static str,
        expanded_matches: usize,
    }
    let cases = [
        Case {
            name: "copies asserted only in a named graph aren't in the default graph",
            statements: &[
                ("Representative", "label", "secret alpha", Some("g"), false),
                ("Alias", "label", "secret alpha", Some("g"), false),
                ("Alias", OWL_SAME_AS, "Representative", None, true),
                ("Representative", "label", "secret alpha", None, true),
            ],
            model: ReadModel::Materialised,
            default_graph_only: true,
            iris: false,
            query: "secret",
            expanded_matches: 0,
        },
        Case {
            name: "an asserted label's equality copy is inferred",
            statements: &[
                ("Alias", OWL_SAME_AS, "Representative", None, false),
                ("Representative", "label", "alpha", None, false),
            ],
            model: ReadModel::Inferred,
            default_graph_only: false,
            iris: false,
            query: "alpha",
            expanded_matches: 1,
        },
        Case {
            name: "an alias is the subject of its representative's inferred statements",
            statements: &[
                ("Alias", OWL_SAME_AS, "Representative", None, false),
                ("Representative", "label", "alpha", None, true),
            ],
            model: ReadModel::Inferred,
            default_graph_only: false,
            iris: true,
            query: "Alias",
            expanded_matches: 1,
        },
    ];
    let mut failed = Vec::new();
    for case in cases {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        // The representative first: the class's smallest id.
        for name in ["Representative", "Alias"] {
            tx.intern(
                NamedNode::new_unchecked(format!("http://example.com/{name}"))
                    .as_ref()
                    .into(),
            );
        }
        let same_as = tx.intern(NamedNode::new_unchecked(OWL_SAME_AS).as_ref().into());
        for &(subject, predicate, object, graph, inferred) in case.statements {
            let quad = match predicate {
                "label" => label(subject, object, graph),
                _ => Quad::new(
                    NamedNode::new_unchecked(format!("http://example.com/{subject}")),
                    NamedNode::new_unchecked(predicate),
                    NamedNode::new_unchecked(format!("http://example.com/{object}")),
                    GraphName::DefaultGraph,
                ),
            };
            match inferred {
                false => assert!(tx.insert(quad.as_ref())),
                true => {
                    let triple = EncodedTriple {
                        subject: tx.intern(quad.subject.as_ref().into()),
                        predicate: tx.intern(quad.predicate.as_ref().into()),
                        object: tx.intern(quad.object.as_ref()),
                    };
                    assert!(tx.insert_inferred(triple));
                }
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let expanded = snapshot.with_equality(Some(same_as));
        let scope = match case.default_graph_only {
            true => TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH)),
            false => TextScope::EVERY,
        };
        for (reader, name) in [
            (&snapshot, "stored"),
            (&expanded, "expanded"),
            (&expanded.with_canonical_equality(), "canonical"),
        ] {
            let context = format!("{}: {name}", case.name);
            check(reader, case.model, &scope, &context, &mut failed);
        }
        let found = match case.iris {
            false => expanded.text_search_in(case.model, &scope, &query(case.query), None),
            true => expanded.iri_search_in(case.model, &scope, &query(case.query), None),
        }
        .unwrap();
        if found.len() != case.expanded_matches {
            failed.push(format!("{}: {found:?}", case.name));
        }
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
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
            &snapshot
                .text_search_in(MODEL, scope, &query("beta"), None)
                .unwrap(),
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect();
        found.sort();
        found
    };
    let mut g_h = vec![id("g"), id("h")];
    g_h.sort_unstable();
    let mut failed = Vec::new();
    let mut row = |name: &str, got: Vec<String>, want: &[&str]| {
        if got != want {
            failed.push(format!("{name}: {got:?} != {want:?}"));
        }
    };
    row(
        "g and h",
        found(&TextScope::Merge(g_h)),
        &["beta in g", "beta in h"],
    );
    row(
        "named",
        found(&TextScope::Graphs(GraphSelector::AnyNamed)),
        &["beta in g", "beta in h"],
    );
    row(
        "default",
        found(&TextScope::Merge(vec![TermId::DEFAULT_GRAPH])),
        &["beta", "beta in h"],
    );
    row("every", found(&EVERY), &["beta", "beta in g", "beta in h"]);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A replica answers as its primary, whatever the primary's open transactions interned;
/// a commit it takes from the primary's log derives its corpus (replication publishes as a
/// commit does).
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
    // A corpus of 11 documents, derived over the log's 4 touched terms.
    replica.set_corpus_tuning(nrese_engine::CorpusTuning {
        documents_per_probe: 1,
        ..nrese_engine::CorpusTuning::DEFAULT
    });
    let q = "alpha beta";
    assert_eq!(
        texts(&replica.snapshot(), &every(&replica.snapshot(), q)),
        texts(&primary.snapshot(), &every(&primary.snapshot(), q))
    );
    let mut tx = primary.transaction();
    tx.insert(label("y", "gamma from the log", None).as_ref());
    tx.remove(label("f0", "filler 0", None).as_ref());
    tx.commit().unwrap();
    let batch = primary
        .log_since(replica.stats().revision, 1 << 20)
        .unwrap();
    replica.apply_log(&batch.frames).unwrap();
    let derived = replica.snapshot().text_corpus_stats().derived;
    assert_eq!(
        texts(&replica.snapshot(), &every(&replica.snapshot(), q)),
        texts(&primary.snapshot(), &every(&primary.snapshot(), q))
    );
    assert_eq!(replica.snapshot().text_corpus_stats().derived, derived + 1);
    let mut tx = primary.transaction();
    for i in 0..60 {
        tx.insert(label(&format!("x{i}"), &format!("beta extra {i}"), None).as_ref());
    }
    let (p, r) = (primary.snapshot(), replica.snapshot());
    assert_eq!(texts(&p, &every(&p, q)), texts(&r, &every(&r, q)));
    assert_eq!(texts(&p, &every(&p, q))[0].0, "beta");
    drop(tx);
}

/// Terms an aborted transaction or a speculative one interned (the shared dictionary keeps
/// them; no visible statement has them) change no reader's text or IRI search: answers and
/// scores as before, for the reader taken before and one taken after. Their words are the
/// visible ones', so a corpus or statistic over them would show.
#[test]
fn aborted_and_speculative_terms_change_no_answer_or_score() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (subject, text) in [
        ("s1", "alpha beta"),
        ("s2", "alpha"),
        ("s3", "beta gamma"),
        ("alphaThing", "omega"),
        ("alphaRest", "omega"),
    ] {
        tx.insert(label(subject, text, None).as_ref());
    }
    tx.commit().unwrap();
    let reader = engine.snapshot();
    let answers = |snapshot: &Snapshot| {
        let text = every(snapshot, "alpha beta");
        // Two visible IRIs of different scores: `alphaThing` matches both words.
        let iris = snapshot
            .iri_search_in(MODEL, &EVERY, &query("alpha thing"), None)
            .unwrap();
        assert_eq!(iris.len(), 2);
        format!("{text:?} {iris:?}")
    };
    let before = answers(&reader);
    {
        let mut aborted = engine.transaction();
        for i in 0..50 {
            let subject = format!("alphaThing{i}");
            aborted.insert(label(&subject, "alpha alpha zeta", None).as_ref());
        }
    }
    let mut failed = Vec::new();
    for (name, got) in [
        ("aborted, earlier reader", answers(&reader)),
        ("aborted, new reader", answers(&engine.snapshot())),
    ] {
        if got != before {
            failed.push(name);
        }
    }
    let mut speculative = engine.speculative();
    for i in 0..50 {
        let subject = format!("alphaThing{i}x");
        speculative.insert(label(&subject, "beta beta eta", None).as_ref());
    }
    let pending = speculative.pending_snapshot();
    for (name, got) in [
        ("speculative, earlier reader", answers(&reader)),
        ("speculative, new reader", answers(&engine.snapshot())),
    ] {
        if got != before {
            failed.push(name);
        }
    }
    drop((pending, speculative));
    assert!(failed.is_empty(), "{failed:?}");
}

/// A reader's vector search: the nearest visible vectors, compared exactly, with the report
/// counting the visible ones only; a vector in a graph outside the scope, nearer than all,
/// is neither found nor counted.
#[test]
fn a_reader_vector_search_selects_and_counts_the_visible_vectors() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..20 {
        tx.insert(label(&format!("v{i}"), &format!("[{i}, 1]"), None).as_ref());
    }
    // Nearest of all to the query, in a graph the default-graph reader doesn't read.
    tx.insert(label("secret", "[0.25, 1]", Some("hidden")).as_ref());
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let mut query = nrese_engine::VectorQuery::new(vec![0.3, 1.0], 3);
    query.metric = nrese_engine::vector::Metric::L2;
    let search = |scope: &TextScope| {
        let (hits, report) = snapshot
            .vector_search_in(MODEL, scope, &query, &|_| true, None)
            .unwrap();
        let values: Vec<String> = hits
            .iter()
            .map(|(id, _)| match snapshot.decode(*id) {
                Some(Term::Literal(l)) => l.value().to_owned(),
                other => format!("{other:?}"),
            })
            .collect();
        (values, report.space, report.graph)
    };
    let mut failed = Vec::new();
    let default = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let want = (
        vec![
            "[0, 1]".to_owned(),
            "[1, 1]".to_owned(),
            "[2, 1]".to_owned(),
        ],
        20,
        false,
    );
    let got = search(&default);
    if got != want {
        failed.push(format!("default graph: {got:?} != {want:?}"));
    }
    let got = search(&TextScope::Graphs(GraphSelector::AnyNamed));
    let want = (vec!["[0.25, 1]".to_owned()], 1, false);
    if got != want {
        failed.push(format!("named graphs: {got:?} != {want:?}"));
    }
    assert!(failed.is_empty(), "{failed:?}");
}

/// Where equality relates an IRI and a literal (`r owl:sameAs "alpha"`), a statement
/// committed about the IRI shows the literal to a reader of equality classes: the corpus
/// after that commit holds `alpha`, as a walk finds it (sol-loop r1 S1: the corpus derived
/// from the kept one probed only the commit's literal objects, and left it out). Oracle:
/// [`visible`], the statements the reader's scans show.
#[test]
fn a_literal_related_by_equality_is_in_the_corpus_after_a_commit() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let ex = |s: &str| NamedNode::new_unchecked(format!("http://example.com/{s}"));
    let mut tx = engine.transaction();
    let r = tx.intern(ex("r").as_ref().into());
    let alpha = tx.intern(Literal::new_simple_literal("alpha").as_ref().into());
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    assert!(tx.insert_inferred(EncodedTriple {
        subject: alpha,
        predicate: same_as,
        object: r,
    }));
    tx.insert(label("filler", "beta", None).as_ref());
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    // A kept corpus to derive from.
    every(&engine.snapshot(), "alpha");
    let mut tx = engine.transaction();
    let (s, p) = (
        tx.intern(ex("s").as_ref().into()),
        tx.intern(ex("p").as_ref().into()),
    );
    assert!(tx.insert_inferred(EncodedTriple {
        subject: s,
        predicate: p,
        object: r,
    }));
    tx.commit().unwrap();
    let mut failed = Vec::new();
    let snapshot = engine.snapshot();
    for model in [ReadModel::Materialised, ReadModel::Inferred] {
        check(&snapshot, model, &EVERY, "after the commit", &mut failed);
    }
    assert!(failed.is_empty(), "{failed:?}");
}

/// What a corpus walk reads under equality expansion is charged to the search's budget,
/// where the snapshot brought no reader's budget of its own (sol-loop r1 S2: a store's
/// autocompletion searches a plain snapshot): a class of 2,001 identities whose
/// representative has 50 labels, searched within 16 KiB, is refused; within 1 GiB it finds
/// the labels (guard). The budget's peak is printed: the measured limit.
#[test]
fn a_corpus_walk_under_expansion_reads_within_the_search_budget() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    let ids: Vec<TermId> = (0..2002)
        .map(|n| {
            let iri = NamedNode::new_unchecked(format!("http://example.com/class/{n}"));
            tx.intern(iri.as_ref().into())
        })
        .collect();
    let (rep, aliases) = (ids[0], &ids[1..]);
    for &alias in aliases {
        assert!(tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        }));
    }
    for i in 0..50 {
        tx.insert(label("class/0", &format!("alpha {i}"), None).as_ref());
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let mut failed = Vec::new();
    for (limit, want_refused) in [(16usize << 10, true), (1 << 30, false)] {
        // A fresh engine store of corpora each time: nothing kept from the other row.
        let snapshot = engine.snapshot();
        let budget = Arc::new(nrese_exec::Budget::new(limit));
        let found = snapshot.text_search_in(MODEL, &EVERY, &query("alpha"), Some(&budget));
        eprintln!(
            "limit {limit}: {:?}, budget peak {} bytes",
            found.as_ref().map(Vec::len),
            budget.peak()
        );
        let ok = match want_refused {
            true => found.is_err(),
            false => found.as_ref().is_ok_and(|found| found.len() == 50),
        };
        if !ok {
            failed.push(format!("limit {limit}: {:?}", found.map(|f| f.len())));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}

/// A vector search of a dimension no vector has finds nothing and reports nothing, before it
/// builds a corpus (sol-loop r1 S3; attack list item 7): within an 8-byte budget, on an empty
/// engine, no refusal.
#[test]
fn a_vector_search_of_an_absent_dimension_builds_no_corpus() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let budget = Arc::new(nrese_exec::Budget::new(8));
    let query = nrese_engine::VectorQuery::new(vec![1.0, 0.0], 3);
    let found = engine
        .snapshot()
        .vector_search_in(MODEL, &EVERY, &query, &|_| true, Some(&budget))
        .map(|(hits, report)| (hits.len(), report.space, report.scanned));
    assert_eq!(found.ok(), Some((0, 0, 0)));
}
