//! Inconsistencies OWL 2 QL can state, under the `owl2-ql` ruleset: a commit that makes the
//! data inconsistent is refused, and data loaded past the commit gates are found
//! inconsistent at their rebuild, the store is in quarantine, and no read is told
//! `complete` (an inconsistent ontology entails every answer). The empty `owl:Thing` is
//! checked under `owl2-rl` too.
//!
//! Spec: OWL 2 Direct Semantics §2.5: an ontology is consistent if a model of it exists.
//! §2.2: the object domain ΔI is a nonempty set, `(owl:Thing)C = ΔI`, `(owl:Nothing)C = ∅`.
//! Each case below is an OWL 2 QL ontology (OWL 2 Profiles §3.2.3, §3.2.5) with no model:
//! - `SubClassOf(:A ObjectComplementOf(:B))` (a `superObjectComplementOf`, §3.2.3) with
//!   `:x` in `:A` and `:B`: Table 4 gives `ObjectComplementOf(CE)` as `ΔI \ (CE)C`, so
//!   Table 5's `(:A)C ⊆ ΔI \ (:B)C` fails for `:x`.
//! - `EquivalentClasses(owl:Thing owl:Nothing)` (§3.2.5, both are classes): Table 5 needs
//!   `ΔI = ∅`, which §2.2 rules out, whatever individuals there are.
//! - `DisjointClasses(:A :B :D)` (n-ary, §3.2.5) with `:x` in `:A` and `:B`: Table 5 needs
//!   `(:A)C ∩ (:B)C = ∅`.
//! - `DisjointObjectProperties(:p :q :r)` (n-ary, §3.2.5) with `:x :p :y` and `:x :q :y`:
//!   Table 6 needs `(:p)OP ∩ (:q)OP = ∅`.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    ConsistencyStatus, MutationCommand, MutationError, MutationPipeline, MutationTicket,
    QlRewritingMode, RejectKind, Requester, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig,
    StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// The cases: a name, the data, and an assertion no statement of the data states (so a
/// consistent store answers `false`), which the inconsistent data entail.
const CLASHES: [(&str, &str, &str); 3] = [
    (
        "complement on the right of a subclass axiom",
        ":A rdfs:subClassOf [ owl:complementOf :B ] . :x a :A , :B . :y a :C .",
        ":y a :D",
    ),
    (
        "owl:Thing equivalent to owl:Nothing, no individual",
        "owl:Thing owl:equivalentClass owl:Nothing .",
        ":y a :D",
    ),
    (
        "owl:Thing equivalent to owl:Nothing, an individual of an undeclared class",
        "owl:Thing owl:equivalentClass owl:Nothing . :y a :C .",
        ":y a :D",
    ),
];

/// [`CLASHES`]' kind, through n-ary disjointness: lists the `owl2-ql` closure doesn't read yet.
const N_ARY_CLASHES: [(&str, &str, &str); 2] = [
    (
        "three disjoint classes",
        "[] a owl:AllDisjointClasses ; owl:members ( :A :B :D ) . :x a :A , :B . :y a :C .",
        ":y a :E",
    ),
    (
        "three disjoint object properties",
        "[] a owl:AllDisjointProperties ; owl:members ( :p :q :r ) . \
         :p a owl:ObjectProperty . :q a owl:ObjectProperty . :r a owl:ObjectProperty . \
         :x :p :y ; :q :y . :y a :C .",
        ":y a :E",
    ),
];

/// The same axioms where no individual meets the clash: each has a model, so the commit is
/// applied, the store is consistent, and the read keeps its status.
const CONTROLS: [(&str, &str, &str); 3] = [
    (
        "complement, members apart",
        ":A rdfs:subClassOf [ owl:complementOf :B ] . :x a :A . :z a :B . :y a :C .",
        ":y a :C",
    ),
    (
        "three disjoint classes, members apart",
        "[] a owl:AllDisjointClasses ; owl:members ( :A :B :D ) . :x a :A . :z a :B . :y a :C .",
        ":y a :C",
    ),
    (
        "three disjoint object properties, pairs apart",
        "[] a owl:AllDisjointProperties ; owl:members ( :p :q :r ) . \
         :p a owl:ObjectProperty . :q a owl:ObjectProperty . :r a owl:ObjectProperty . \
         :x :p :y . :z :q :y . :y a :C .",
        ":y a :C",
    ),
];

fn store(ql: QlRewritingMode) -> Arc<StoreService> {
    Arc::new(
        StoreService::new(StoreConfig {
            ql_rewriting: ql,
            ..crate::support::in_memory_store_config()
        })
        .expect("store"),
    )
}

fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

/// A commit's outcome: `applied`, `refused` as inconsistent, or `failed` with any other error.
fn commit(pipeline: &MutationPipeline, data: &str) -> String {
    match pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}INSERT DATA {{ {data} }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
    ) {
        Ok(_) => "applied".to_owned(),
        Err(MutationError::Rejected(r)) if r.kind == RejectKind::Inconsistent => {
            "refused".to_owned()
        }
        Err(e) => format!("failed: {e:?}"),
    }
}

/// What the store says of a read of `ASK { question }`: the answer and the status word an
/// HTTP read is told (`NRESE-Completeness`; a read with no status is told `complete`).
fn read(store: &StoreService, question: &str) -> String {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ {question} }}"));
    let result = store.execute_query(&request).expect("query");
    let answer = String::from_utf8_lossy(&result.payload)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .contains("\"boolean\":true");
    let word = result.completeness.unwrap_or_default().as_str();
    format!("{answer}, {word}")
}

/// `data` loaded past the commit gates into a store reasoning in `mode` (as
/// `nrese-server load` and a start-up rebuild see them) and rebuilt: the store's verdict,
/// whether it is in quarantine, and what a read of `question` is told.
fn loaded(mode: ReasoningMode, ql: QlRewritingMode, data: &str, question: &str) -> String {
    let store = store(ql);
    let _reasoning = pipeline(&store, mode);
    crate::support::load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
    let program = ReasonerConfig::for_mode(mode)
        .materialised_program()
        .expect("a materialised program");
    store.rematerialise(program).expect("rebuilt");
    let verdict = match store.consistency() {
        ConsistencyStatus::Consistent => "consistent",
        ConsistencyStatus::Inconsistent { .. } => "inconsistent",
        _ => "other",
    };
    format!(
        "{verdict}, quarantined {}, read {}",
        store.is_quarantined(),
        read(&store, question)
    )
}

/// Spec (module docs): each OWL 2 QL ontology without a model is refused as a commit, and,
/// loaded past the gates, is found inconsistent at its rebuild, puts the store in quarantine,
/// and no read is told `complete`: the read below answers `false` to an assertion the data
/// entail.
#[test]
fn an_inconsistency_owl2_ql_states_is_found() {
    found_inconsistent(ReasoningMode::Owl2Ql, &CLASHES);
}

/// Spec (module docs): as [`an_inconsistency_owl2_ql_states_is_found`], for n-ary
/// `DisjointClasses` and `DisjointObjectProperties` (Direct Semantics Tables 5 and 6).
#[test]
#[ignore = "owl2-ql doesn't check n-ary disjointness yet: its list rules come after the list module's rework"]
fn an_n_ary_disjointness_owl2_ql_states_is_found() {
    found_inconsistent(ReasoningMode::Owl2Ql, &N_ARY_CLASHES);
}

/// Spec: under `owl2-rl`'s semantics, the OWL 2 RDF-Based Semantics, the universe `IR` is a
/// non-empty set (RDF 1.1 Semantics §5), `ICEXT(I(owl:Thing)) = IR` and
/// `ICEXT(I(owl:Nothing)) = ∅` (OWL 2 RDF-Based Semantics, Table 5.2), and
/// `owl:equivalentClass` makes two classes' extensions equal (§5.9): so
/// `owl:Thing owl:equivalentClass owl:Nothing` has no model, with or without individuals. It
/// is refused as a commit, and found inconsistent when loaded, with no read told `complete`.
#[test]
fn an_empty_owl_thing_is_found_under_owl2_rl() {
    // The two `owl:Thing ≡ owl:Nothing` cases of [`CLASHES`].
    found_inconsistent(ReasoningMode::Owl2Rl, &CLASHES[1..]);
}

/// Spec: a refused write's report names the violated rule and says what its violation means
/// (docs/ops/http-api.md, "Reasoner Reject": `cax-dw (an instance of two disjoint classes)`).
/// For an empty `owl:Thing` it names `cls-thing-empty` and says so in its own words, not the
/// text of an unknown rule, under `owl2-ql` and `owl2-rl`.
#[test]
fn a_refused_empty_owl_thing_says_what_it_means() {
    for mode in [ReasoningMode::Owl2Ql, ReasoningMode::Owl2Rl] {
        let store = store(QlRewritingMode::Auto);
        let result = pipeline(&store, mode).apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{PREFIXES}INSERT DATA {{ owl:Thing owl:equivalentClass owl:Nothing . }}"
            ))),
            &Requester::all(),
            &MutationTicket::new(),
        );
        let reject = match result {
            Err(MutationError::Rejected(reject)) => reject,
            other => panic!("{mode:?}: not refused: {other:?}"),
        };
        let meaning = reject
            .detail
            .split_once("cls-thing-empty (")
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(meaning, _)| meaning.to_owned());
        assert!(
            meaning
                .as_deref()
                .is_some_and(|m| m.contains("owl:Thing") && m.contains("owl:Nothing")),
            "{mode:?}: {}",
            reject.detail
        );
    }
}

/// Each case, under `mode`, refused as a commit, and found inconsistent when loaded, with no
/// read told `complete`.
fn found_inconsistent(mode: ReasoningMode, cases: &[(&str, &str, &str)]) {
    let mut wrong = Vec::new();
    for &(name, data, question) in cases {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, mode), data);
        if committed != "refused" {
            wrong.push(format!("{name}, committed: {committed}"));
        }
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let found = loaded(mode, ql, data, question);
            if found != "inconsistent, quarantined true, read false, sound-only" {
                wrong.push(format!("{name}, loaded, {ql:?}: {found}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (module docs): the same axioms with no individual meeting the clash have a model, so
/// the commit is applied and, loaded, the store is consistent, not in quarantine, and a read
/// of an asserted membership is `true`. (The read's status word isn't this test's subject:
/// it depends on what else the store holds, as other tests state.)
#[test]
fn the_same_axioms_without_a_clash_stay_consistent() {
    let mut wrong = Vec::new();
    for (name, data, question) in CONTROLS {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, ReasoningMode::Owl2Ql), data);
        if committed != "applied" {
            wrong.push(format!("{name}, committed: {committed}"));
        }
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let found = loaded(ReasoningMode::Owl2Ql, ql, data, question);
            if !found.starts_with("consistent, quarantined false, read true, ") {
                wrong.push(format!("{name}, loaded, {ql:?}: {found}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (module docs, §2.2): `owl:Thing` is the whole domain and `owl:Nothing` empty, so
/// `owl:Nothing` under `owl:Thing`, a class equivalent to `owl:Thing`, and an empty class
/// under `owl:Nothing` have a model with `:x` in it. Under `owl2-ql` and `owl2-rl` the commit
/// is applied and, loaded, the store is consistent, not in quarantine, and `:x a :C` is read
/// `true`. (A guard against the empty-`owl:Thing` check firing on its near misses: it is red
/// only when that check is wrong, e.g. with its two terms swapped.)
#[test]
fn owl_thing_and_owl_nothing_in_their_own_places_stay_consistent() {
    let data = "owl:Nothing rdfs:subClassOf owl:Thing . :A owl:equivalentClass owl:Thing . \
                :B rdfs:subClassOf owl:Nothing . :C a owl:Class . :x a :A , :C .";
    let mut wrong = Vec::new();
    for mode in [ReasoningMode::Owl2Ql, ReasoningMode::Owl2Rl] {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, mode), data);
        if committed != "applied" {
            wrong.push(format!("{mode:?}, committed: {committed}"));
        }
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let found = loaded(mode, ql, data, ":x a :C");
            if !found.starts_with("consistent, quarantined false, read true, ") {
                wrong.push(format!("{mode:?}, loaded, {ql:?}: {found}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
