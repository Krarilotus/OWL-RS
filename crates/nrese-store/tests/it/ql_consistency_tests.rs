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
//!
//! And the converse: annotations "have no semantic meaning in OWL 2" (Direct Semantics §1), so
//! an annotation property's domain or range (`AnnotationPropertyDomain` / `Range`, which
//! OWL 2 QL allows, Profiles §3.2.5) entails no membership, and data clashing only through one
//! has a model: it is consistent, and its membership isn't an answer.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    CancellationToken, ConsistencyStatus, MutationCommand, MutationError, MutationPipeline,
    MutationTicket, QlRewritingMode, RejectKind, Requester, SparqlQueryRequest,
    SparqlUpdateRequest, StatementOp, StatementsRequest, StoreConfig, StoreService,
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

/// An annotation property's domain or range (`:note`, declared one) that, read as an object
/// property's, would put `:alice` (a `:Person`) in `:Doc` and so into a clash. Each has a model
/// in which `:alice` isn't a `:Doc`.
const ANNOTATION_AXIOMS: [(&str, &str); 3] = [
    (
        "an annotation domain beside a disjointness",
        ":note a owl:AnnotationProperty ; rdfs:domain :Doc . :Doc a owl:Class . :Person a owl:Class . \
         :Doc owl:disjointWith :Person . :alice a :Person ; :note \"x\" .",
    ),
    (
        "an annotation domain beside a complement",
        ":note a owl:AnnotationProperty ; rdfs:domain :Doc . :Doc a owl:Class . :Person a owl:Class . \
         :Doc rdfs:subClassOf [ owl:complementOf :Person ] . :alice a :Person ; :note \"x\" .",
    ),
    (
        "an annotation range beside a disjointness",
        ":note a owl:AnnotationProperty ; rdfs:range :Doc . :Doc a owl:Class . :Person a owl:Class . \
         :Doc owl:disjointWith :Person . :alice a :Person . :bob :note :alice .",
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
    apply(pipeline, &format!("INSERT DATA {{ {data} }}"))
}

/// [`commit`] of `update`, an update request without prefixes.
fn apply(pipeline: &MutationPipeline, update: &str) -> String {
    match pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{update}"))),
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
    let (store, _reasoning) = rebuilt(mode, ql, data);
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

/// `data` loaded past the commit gates into a store reasoning in `mode` and rebuilt, with its
/// commits' pipeline.
fn rebuilt(
    mode: ReasoningMode,
    ql: QlRewritingMode,
    data: &str,
) -> (Arc<StoreService>, MutationPipeline) {
    let store = store(ql);
    let reasoning = pipeline(&store, mode);
    crate::support::load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
    store.rematerialise(program(mode)).expect("rebuilt");
    (store, reasoning)
}

fn program(mode: ReasoningMode) -> nrese_reasoner::RuleProgram {
    ReasonerConfig::for_mode(mode)
        .materialised_program()
        .expect("a materialised program")
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

/// Spec (module docs, Direct Semantics §1): under `owl2-ql` each of [`ANNOTATION_AXIOMS`] is
/// applied as a commit, after which `:alice a :Doc` is read `false`; loaded and rebuilt, the
/// store is consistent, not in quarantine, and the read is `false` again, with the QL rewriting
/// on or as configured.
#[test]
fn an_annotation_propertys_domain_and_range_mean_nothing_under_owl2_ql() {
    let mut wrong = Vec::new();
    for (name, data) in ANNOTATION_AXIOMS {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, ReasoningMode::Owl2Ql), data);
        let after = read(&store, ":alice a :Doc");
        if committed != "applied" || !after.starts_with("false, ") {
            wrong.push(format!("{name}, committed: {committed}, read {after}"));
        }
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let found = loaded(ReasoningMode::Owl2Ql, ql, data, ":alice a :Doc");
            if !found.starts_with("consistent, quarantined false, read false, ") {
                wrong.push(format!("{name}, loaded, {ql:?}: {found}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec: what keeps its meaning beside [`an_annotation_propertys_domain_and_range_mean_nothing_under_owl2_ql`].
/// Under `owl2-ql` an object property's domain (`ObjectPropertyDomain`, Direct Semantics
/// Table 6) and an undeclared property's (which the OWL reader reads as an object property's)
/// still type its subject: `:alice a :Doc` is read `true`. Under `owl2-rl` (RDF-Based Semantics,
/// where `rdfs:domain` and `rdfs:range` hold for every property: RDFS entailment patterns
/// rdfs2 and rdfs3) each of [`ANNOTATION_AXIOMS`] is still refused. (Guards against skipping
/// more than annotation properties: red only when the skip reaches them.)
#[test]
fn other_properties_domains_keep_their_meaning() {
    let mut wrong = Vec::new();
    for (name, data) in [
        (
            "an object property's domain",
            ":note a owl:ObjectProperty ; rdfs:domain :Doc . :alice :note :x .",
        ),
        (
            "an undeclared property's domain",
            ":note rdfs:domain :Doc . :alice :note :x .",
        ),
    ] {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, ReasoningMode::Owl2Ql), data);
        let after = read(&store, ":alice a :Doc");
        if committed != "applied" || !after.starts_with("true, ") {
            wrong.push(format!("{name}, committed: {committed}, read {after}"));
        }
    }
    for (name, data) in ANNOTATION_AXIOMS {
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, ReasoningMode::Owl2Rl), data);
        if committed != "refused" {
            wrong.push(format!("owl2-rl, {name}, committed: {committed}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each `owl2-ql` rule beside `prp-dom` / `prp-rng` that can consume what the OWL reader reads
/// as an annotation: an annotation property's assertion (`:x :note :v`), its domain or range,
/// or an `rdfs:subPropertyOf` with an annotation property at either end. `:note` is declared
/// an annotation property, `:p` and `:q` object properties. The case's name is the rule; the
/// data has a model; the question (where there is one) is a consequence only an annotation
/// would give, so it isn't an answer. The last four each reach the closure through one of
/// those positions alone (a domain or range's subject; a sub-property statement's subject;
/// its object), so each position is stated on its own.
const ANNOTATION_READINGS: [(&str, &str, Option<&str>); 18] = [
    (
        "prp-spo1",
        ":note rdfs:subPropertyOf :q . :x :note :v .",
        Some(":x :q :v"),
    ),
    (
        "prp-eqp1",
        ":note owl:equivalentProperty :q . :x :note :v .",
        Some(":x :q :v"),
    ),
    (
        "prp-eqp2",
        ":q owl:equivalentProperty :note . :x :note :v .",
        Some(":x :q :v"),
    ),
    (
        "prp-inv1",
        ":note owl:inverseOf :q . :x :note :v .",
        Some(":v :q :x"),
    ),
    (
        "prp-inv2",
        ":q owl:inverseOf :note . :x :note :v .",
        Some(":v :q :x"),
    ),
    (
        "prp-symp",
        ":note a owl:SymmetricProperty . :x :note :v .",
        Some(":v :note :x"),
    ),
    (
        "prp-asyp",
        ":note a owl:AsymmetricProperty . :x :note :v . :v :note :x .",
        None,
    ),
    (
        "prp-irp",
        ":note a owl:IrreflexiveProperty . :x :note :x .",
        None,
    ),
    (
        "prp-pdw",
        ":note owl:propertyDisjointWith :q . :x :note :v ; :q :v .",
        None,
    ),
    (
        "cls-svf2",
        "[ a owl:Restriction ; owl:onProperty :note ; owl:someValuesFrom owl:Thing ] \
         rdfs:subClassOf :D . :x :note :v .",
        Some(":x a :D"),
    ),
    (
        "scm-spo",
        ":p rdfs:subPropertyOf :note . :note rdfs:subPropertyOf :q . :x :p :v .",
        Some(":x :q :v"),
    ),
    (
        "scm-eqp2",
        ":p rdfs:subPropertyOf :note . :note rdfs:subPropertyOf :p . :x :p :v .",
        Some(":x :note :v"),
    ),
    (
        "scm-dom2",
        ":note rdfs:domain :Doc . :p rdfs:subPropertyOf :note . :x :p :v .",
        Some(":x a :Doc"),
    ),
    (
        "scm-rng2",
        ":note rdfs:range :Doc . :p rdfs:subPropertyOf :note . :x :p :v .",
        Some(":v a :Doc"),
    ),
    (
        "scm-dom1",
        ":note rdfs:domain :Doc . :Doc rdfs:subClassOf :Top .",
        Some(":note rdfs:domain :Top"),
    ),
    (
        "scm-rng1",
        ":note rdfs:range :Doc . :Doc rdfs:subClassOf :Top .",
        Some(":note rdfs:range :Top"),
    ),
    (
        "scm-spo, from an annotation property",
        ":note rdfs:subPropertyOf :p . :p rdfs:subPropertyOf :q .",
        Some(":note rdfs:subPropertyOf :q"),
    ),
    (
        "prp-spo1, into an annotation property",
        ":p rdfs:subPropertyOf :note . :x :p :v .",
        Some(":x :note :v"),
    ),
];

/// Spec (module docs, Direct Semantics §1; the OWL reader's readings: an annotation
/// property's assertion, its domain and range, and an `rdfs:subPropertyOf` with an annotation
/// property at either end, are annotations, with no axiom): under `owl2-ql`, each of [`ANNOTATION_READINGS`] is
/// applied as a commit, and its question, if it has one, is read `false`; loaded and rebuilt,
/// the store is consistent, not in quarantine, and the question is `false` again, the QL
/// rewriting on or as configured.
#[test]
fn no_owl2_ql_rule_reads_an_annotation_as_an_axiom() {
    let declarations = ":note a owl:AnnotationProperty . :p a owl:ObjectProperty . \
                        :q a owl:ObjectProperty . ";
    let mut wrong = Vec::new();
    for (rule, data, question) in ANNOTATION_READINGS {
        let data = format!("{declarations}{data}");
        let asked = question.unwrap_or(":nothing a :Asked");
        let store = store(QlRewritingMode::Auto);
        let committed = commit(&pipeline(&store, ReasoningMode::Owl2Ql), &data);
        let after = read(&store, asked);
        if committed != "applied" || !after.starts_with("false, ") {
            wrong.push(format!("{rule}, committed: {committed}, read {after}"));
        }
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let found = loaded(ReasoningMode::Owl2Ql, ql, &data, asked);
            if !found.starts_with("consistent, quarantined false, read false, ") {
                wrong.push(format!("{rule}, loaded, {ql:?}: {found}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec: a store with a custom program answers what that program entails, and the user's
/// rules are the user's choice (as `equality_rule_names_tests` states of their names); the
/// guard against annotation readings is on `owl2-ql`'s built-in rules only
/// (reasoning-semantics.md, "User rules (Notation3)"). On
/// `owl2-ql`, a user rule `(?p rdfs:subPropertyOf ?q), (?x ?p ?y)
/// -> (?x ?q ?y)` applies to `:note`, declared an annotation property: with
/// `:note rdfs:subPropertyOf :q` and `:x :note :v` committed, `:x :q :v` is read `true`.
#[test]
fn a_user_rule_on_owl2_ql_reads_an_annotation_property_as_written() {
    let rules = nrese_reasoner::UserRules::n3(
        "user.n3",
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         { ?p rdfs:subPropertyOf ?q . ?x ?p ?y } => { ?x ?q ?y } .",
    )
    .expect("the rules compile");
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Ql)
        .with_rules(Some(Arc::new(rules)))
        .expect("rules on a ruleset");
    let store = store(QlRewritingMode::Auto);
    let reasoning =
        MutationPipeline::new(Arc::clone(&store), Arc::new(ReasonerService::new(config)));
    let data = ":note a owl:AnnotationProperty ; rdfs:subPropertyOf :q . :x :note :v .";
    let committed = commit(&reasoning, data);
    let after = read(&store, ":x :q :v");
    assert!(
        committed == "applied" && after.starts_with("true, "),
        "committed: {committed}, read {after}"
    );
}

/// A property's domain, with `:alice` using it: `:alice a :Doc` follows while `:note` isn't
/// declared an annotation property (the OWL reader reads the domain as an object property's),
/// and doesn't once it is.
const UNDECLARED_DOMAIN: &str = ":note rdfs:domain :Doc . :alice :note :x .";
const DECLARATION: &str = ":note a owl:AnnotationProperty .";

/// Whether `ASK { question }` holds with `pending` (an update) pending in a client transaction.
fn ask_pending(store: &StoreService, pending: &str, question: &str) -> bool {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ {question} }}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let ops = vec![StatementOp::Update(SparqlUpdateRequest::new(format!(
        "{PREFIXES}{pending}"
    )))];
    let mut out = Vec::new();
    store
        .run_query_pending(
            &StatementsRequest::new(ops),
            &prepared,
            &CancellationToken::new(),
            &mut out,
        )
        .expect("pending query");
    String::from_utf8_lossy(&out)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .contains("\"boolean\":true")
}

/// Spec (module docs; the reader's rule, under which a declaration makes `:note` an annotation
/// property): a read inside a transaction answers by the transaction's own state. Under
/// `owl2-ql`, over committed [`UNDECLARED_DOMAIN`], a pending declaration of `:note` makes
/// `:alice a :Doc` `false` inside the transaction while the committed state still answers
/// `true`; over the declaration committed too, a pending retraction of it makes the read
/// `true` inside, while the committed state answers `false`.
#[test]
fn a_transaction_reads_by_its_own_declarations() {
    let mut wrong = Vec::new();
    let store = store(QlRewritingMode::Auto);
    let reasoning = pipeline(&store, ReasoningMode::Owl2Ql);
    let committed = commit(&reasoning, UNDECLARED_DOMAIN);
    let (outside, inside) = (
        read(&store, ":alice a :Doc"),
        ask_pending(
            &store,
            &format!("INSERT DATA {{ {DECLARATION} }}"),
            ":alice a :Doc",
        ),
    );
    if committed != "applied" || !outside.starts_with("true, ") || inside {
        wrong.push(format!(
            "declaration pending: committed {committed}, outside {outside}, inside {inside}"
        ));
    }
    let committed = commit(&reasoning, DECLARATION);
    let (outside, inside) = (
        read(&store, ":alice a :Doc"),
        ask_pending(
            &store,
            &format!("DELETE DATA {{ {DECLARATION} }}"),
            ":alice a :Doc",
        ),
    );
    if committed != "applied" || !outside.starts_with("false, ") || !inside {
        wrong.push(format!(
            "retraction pending: committed {committed}, outside {outside}, inside {inside}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (module docs; the reader's rule): a replica answers as its primary at each revision it
/// has taken. Under `owl2-ql` a primary commits [`UNDECLARED_DOMAIN`] (`:alice a :Doc` is
/// `true`), then the declaration of `:note` (`false`), then its retraction (`true`); a replica
/// that takes the primary's records after each commit answers as the primary each time.
#[test]
fn a_replica_follows_its_primary_across_a_declaration() {
    let (at, follows) = (
        tempfile::tempdir().expect("dir"),
        tempfile::tempdir().expect("dir"),
    );
    let on_disk = |dir: &std::path::Path, replica: bool| {
        let store = Arc::new(
            StoreService::new(StoreConfig {
                replica,
                ..StoreConfig::on_disk(dir)
            })
            .expect("store"),
        );
        let reasoning = pipeline(&store, ReasoningMode::Owl2Ql);
        (store, reasoning)
    };
    let (primary, reasoning) = on_disk(at.path(), false);
    let (replica, _following) = on_disk(follows.path(), true);
    let mut wrong = Vec::new();
    for (step, data, expected) in [
        (
            "the domain",
            format!("INSERT DATA {{ {UNDECLARED_DOMAIN} }}"),
            "true, ",
        ),
        (
            "the declaration",
            format!("INSERT DATA {{ {DECLARATION} }}"),
            "false, ",
        ),
        (
            "its retraction",
            format!("DELETE DATA {{ {DECLARATION} }}"),
            "true, ",
        ),
    ] {
        reasoning
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{data}"))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .unwrap_or_else(|error| panic!("{step}: {error}"));
        let after = replica.current_revision();
        let batch = primary.replication_log(after, 1 << 20).expect("log");
        let expected_log = nrese_store::LogExpectation {
            after,
            records: batch.records as u64,
            last: batch.last,
        };
        let stop = nrese_exec::stop::StopSource::new(None, None);
        replica
            .apply_replication_log(&batch.frames, &expected_log, &stop)
            .unwrap_or_else(|error| panic!("{step}, taken: {error}"));
        let (on_primary, on_replica) = (
            read(&primary, ":alice a :Doc"),
            read(&replica, ":alice a :Doc"),
        );
        let same_revision = replica.current_revision() == primary.current_revision();
        let same_answer = on_primary.split(',').next() == on_replica.split(',').next();
        if !on_primary.starts_with(expected) || !same_answer || !same_revision {
            wrong.push(format!(
                "after {step}: primary {on_primary}, replica {on_replica}, same revision {same_revision}"
            ));
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

/// `:note` an annotation property by inference alone (`:MyAP` under `owl:AnnotationProperty`),
/// with a domain `:alice` uses.
const INFERRED_DECLARATION: &str = ":MyAP rdfs:subClassOf owl:AnnotationProperty . \
                                    :note a :MyAP ; rdfs:domain :Doc . :alice :note :x .";

/// Spec (module docs; the reader's rule: a property is an annotation property by an asserted
/// declaration, never by an inferred one, as a rebuild reads it): under `owl2-ql`, over
/// committed [`INFERRED_DECLARATION`], `:alice a :Doc` is read `true`; committing
/// [`DECLARATION`], whose statement the closure already held, makes it `false`; retracting it
/// again, while the closure still infers it, makes it `true`.
#[test]
fn a_declaration_counts_where_it_is_asserted_not_where_it_is_inferred() {
    let store = store(QlRewritingMode::Auto);
    let reasoning = pipeline(&store, ReasoningMode::Owl2Ql);
    let mut wrong = Vec::new();
    for (step, update, read_true) in [
        (
            "inferred only",
            format!("INSERT DATA {{ {INFERRED_DECLARATION} }}"),
            true,
        ),
        (
            "asserted",
            format!("INSERT DATA {{ {DECLARATION} }}"),
            false,
        ),
        (
            "retracted, still inferred",
            format!("DELETE DATA {{ {DECLARATION} }}"),
            true,
        ),
    ] {
        let committed = apply(&reasoning, &update);
        let after = read(&store, ":alice a :Doc");
        if committed != "applied" || !after.starts_with(&format!("{read_true}, ")) {
            wrong.push(format!("{step}: committed {committed}, read {after}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (the commit gate: in quarantine a commit is refused for a clash of its own and applied
/// without one, as `mutation_pipeline_tests` states; Direct Semantics §1 for what an annotation
/// property's statements mean): under `owl2-ql`, over a store in quarantine (`:x` in the
/// disjoint `:A` and `:B`, loaded past the gates), each commit below is refused when it brings a
/// clash and applied when it doesn't, whatever annotation property it declares or retracts: a
/// clash (`:alice` in `:Doc`, disjoint from her `:Person`) alone or with an unrelated
/// declaration; a declaration of `:note` used at once, whose domain then types nothing; the
/// retraction of an irreflexive `:note`'s declaration, after which `:y :note :y` clashes; and,
/// where `:alice a :Doc` clashes both asserted and derived (`:wrote`'s domain), its retraction
/// alone, with an unrelated declaration, or with an unrelated retraction: the fact stays derived,
/// so the clash held before and is not the commit's own.
#[test]
fn a_redeclaring_commit_in_quarantine_is_judged_by_its_own_clashes() {
    let disjoint = ":Doc owl:disjointWith :Person . :alice a :Person . ";
    let derived_clash = format!("{disjoint}:alice a :Doc ; :wrote :w . :wrote rdfs:domain :Doc . ");
    let derived_clash = derived_clash.as_str();
    let mut wrong = Vec::new();
    for (name, base, update, outcome) in [
        (
            "a clash",
            disjoint.to_owned(),
            "INSERT DATA { :alice a :Doc . }",
            "refused",
        ),
        (
            "a clash and a declaration",
            disjoint.to_owned(),
            "INSERT DATA { :alice a :Doc . :other a owl:AnnotationProperty . }",
            "refused",
        ),
        (
            "a declaration used at once",
            format!("{disjoint}:note rdfs:domain :Doc ."),
            "INSERT DATA { :note a owl:AnnotationProperty . :alice :note \"x\" . }",
            "applied",
        ),
        (
            "a retraction",
            ":note a owl:AnnotationProperty , owl:IrreflexiveProperty . :y :note :y .".to_owned(),
            "DELETE DATA { :note a owl:AnnotationProperty . }",
            "refused",
        ),
        (
            "a fact retracted and still derived",
            derived_clash.to_owned(),
            "DELETE DATA { :alice a :Doc . }",
            "applied",
        ),
        (
            "a fact retracted and still derived, and a declaration",
            derived_clash.to_owned(),
            "DELETE DATA { :alice a :Doc . } ; INSERT DATA { :other a owl:AnnotationProperty . }",
            "applied",
        ),
        (
            "a fact retracted and still derived, and a retraction",
            format!("{derived_clash}:other a owl:AnnotationProperty ."),
            "DELETE DATA { :alice a :Doc . :other a owl:AnnotationProperty . }",
            "applied",
        ),
    ] {
        let (store, reasoning) = rebuilt(
            ReasoningMode::Owl2Ql,
            QlRewritingMode::Auto,
            &format!(":A owl:disjointWith :B . :x a :A , :B . {base}"),
        );
        if !store.is_quarantined() {
            wrong.push(format!("{name}: setup: not in quarantine"));
            continue;
        }
        let committed = apply(&reasoning, update);
        if committed != outcome {
            wrong.push(format!("{name}: {committed}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (`entailment` module docs: a negative statement is decided by refutation, its opposite
/// added and a violated consistency rule refuting it): with `:Item` disjoint from
/// `owl:AnnotationProperty` and `:other` an `:Item`, the data entail
/// `:other a [ owl:complementOf owl:AnnotationProperty ]` under `owl2-ql` as under `owl2-rl`:
/// the opposite, a declaration of `:other`, clashes (`cax-dw`).
#[test]
fn a_refutation_through_a_declaration_is_judged() {
    let node =
        |local: &str| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{local}"));
    let owl = |local: &str| {
        nrese_rdf::NamedNode::new_unchecked(format!("http://www.w3.org/2002/07/owl#{local}"))
    };
    let complement = nrese_rdf::BlankNode::new_unchecked("c");
    let rdf_type =
        nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let conclusion = [
        nrese_rdf::Triple::new(node("other"), rdf_type, complement.clone()),
        nrese_rdf::Triple::new(complement, owl("complementOf"), owl("AnnotationProperty")),
    ];
    let mut wrong = Vec::new();
    for mode in [ReasoningMode::Owl2Ql, ReasoningMode::Owl2Rl] {
        let (store, _reasoning) = rebuilt(
            mode,
            QlRewritingMode::Auto,
            ":Item owl:disjointWith owl:AnnotationProperty . :other a :Item .",
        );
        let answer = store.entails(program(mode), &conclusion);
        if !matches!(answer, Ok(nrese_store::Entailment::Entailed)) {
            wrong.push(format!("{mode:?}: {answer:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (module docs, Direct Semantics §1; an explanation is a derivation by the rules, so it
/// takes no step the closure doesn't): under `owl2-ql`, `:alice a :Doc` holds through `:p`, an
/// object property with the domain `:Doc`, which `:alice :p :y` gets from its inverse `:q`;
/// `:note`, an annotation property with the domain `:Doc` that `:alice` uses too, gives it no
/// derivation, not even the shorter one it would be. Its explanation (the shortest derivation)
/// and the union of its justifications name no statement about or with `:note`.
#[test]
fn an_explanation_takes_no_step_through_an_annotation_property() {
    let data = ":note a owl:AnnotationProperty ; rdfs:domain :Doc . :alice :note :x . \
                :p a owl:ObjectProperty ; owl:inverseOf :q ; rdfs:domain :Doc . \
                :q a owl:ObjectProperty . :y :q :alice .";
    let (store, _reasoning) = rebuilt(ReasoningMode::Owl2Ql, QlRewritingMode::Auto, data);
    let node =
        |local: &str| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/{local}"));
    let rdf_type =
        nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let (alice, doc) = (node("alice"), node("Doc"));
    let statement: [nrese_rdf::TermRef<'_>; 3] = [
        alice.as_ref().into(),
        rdf_type.as_ref().into(),
        doc.as_ref().into(),
    ];
    let note = |terms: [&str; 3]| terms.contains(&"http://example.com/note");
    let scope = nrese_store::ReadScope::All;
    let mut wrong = Vec::new();
    let [s, p, o] = statement;
    let steps = (store.explain_statement(program(ReasoningMode::Owl2Ql), &scope, s, p, o))
        .expect("explained");
    match steps {
        None => wrong.push("no explanation".to_owned()),
        Some(steps) => wrong.extend(
            (steps.iter())
                .filter(|s| note([&s.subject, &s.predicate, &s.object]))
                .map(|s| format!("explanation: {} {} {}", s.subject, s.predicate, s.object)),
        ),
    }
    let mode = nrese_store::reasoning::JustificationMode::Union;
    let union = (store.justify_statement(program(ReasoningMode::Owl2Ql), &scope, statement, mode))
        .expect("justified");
    match union {
        None => wrong.push("no justification".to_owned()),
        Some(answer) => wrong.extend(
            (answer.sets.iter().flatten())
                .filter(|s| note([&s.subject, &s.predicate, &s.object]))
                .map(|s| format!("justification: {} {} {}", s.subject, s.predicate, s.object)),
        ),
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
