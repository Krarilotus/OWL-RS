//! Q07 (N12, RSH-01/03, SYN-65): a rejection shown to a requester who may read only some
//! graphs depends only on statements of those graphs. That the commit is rejected is the
//! approved global-gate oracle (D-1); its terms, counts, engine, time and focus are not.
//! A requester who may read every graph keeps the full evidence (the `*_full_*` controls,
//! which pass before the fix too).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::GraphAccess;
use nrese_store::{
    GateSeverity, MutationCommand, MutationError, MutationPipeline, MutationReject, MutationTicket,
    ReadScope, Requester, ShaclGate, SparqlUpdateRequest, StoreConfig, StoreService, WriteScope,
};

const P: &str = "PREFIX : <http://example.com/> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX sh: <http://www.w3.org/ns/shacl#> ";

fn update(text: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "{P}INSERT DATA {{ {text} }}"
    )))
}

/// May read and write `:open` only.
fn open_reader() -> Requester {
    let access = Arc::new(GraphAccess {
        graphs: vec!["http://example.com/open".into()],
        ..GraphAccess::default()
    });
    Requester::new(
        ReadScope::Graphs(Arc::clone(&access)),
        WriteScope::Graphs(access),
    )
}

/// The rejection `requester` gets for `commit` on a fresh store holding `data`.
fn rejection(
    config: StoreConfig,
    mode: ReasoningMode,
    data: &str,
    commit: &str,
    requester: &Requester,
) -> MutationReject {
    let p = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    p.apply(update(data), &Requester::all(), &MutationTicket::new())
        .expect("setup commits");
    match p.apply(update(commit), requester, &MutationTicket::new()) {
        Err(MutationError::Rejected(reject)) => *reject,
        other => panic!("must reject: {other:?}"),
    }
}

/// A functional data property and its `"SECRET"` value, both in `:secret`; the commit
/// gives the subject a second value.
const RL_DATA: &str =
    "GRAPH :secret { :p a owl:FunctionalProperty, owl:DatatypeProperty . :a :p \"SECRET\" }";
const RL_COMMIT: &str = "GRAPH :open { :a :p \"new\" }";

fn rl(requester: &Requester) -> MutationReject {
    let config = StoreConfig::in_memory();
    rejection(config, ReasoningMode::Owl2Rl, RL_DATA, RL_COMMIT, requester)
}

/// RSH-01: an OWL 2 RL rejection never decodes a term of a premise in a graph the
/// committer can't read.
#[test]
fn rl_rejection_does_not_decode_private_bindings() {
    let reject = rl(&open_reader());
    assert!(!format!("{reject:?}").contains("SECRET"), "{reject:?}");
}

/// Control: a reader of every graph sees the clashing values.
#[test]
fn rl_rejection_full_reader_sees_the_values() {
    let reject = rl(&Requester::all());
    assert!(reject.detail.contains("SECRET"), "{}", reject.detail);
}

/// An open subclass axiom whose superclass, a restriction on `:SECRET_PROPERTY`, is
/// described in `:secret`.
const DL_STRUCTURAL_DATA: &str = "GRAPH :open { :A rdfs:subClassOf _:r } \
     GRAPH :secret { _:r a owl:Restriction ; owl:onProperty :SECRET_PROPERTY ; \
     owl:someValuesFrom owl:Nothing }";
const DL_STRUCTURAL_COMMIT: &str = "GRAPH :open { :a a :A }";

fn dl_structural(requester: &Requester) -> MutationReject {
    let (config, mode) = (StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    rejection(
        config,
        mode,
        DL_STRUCTURAL_DATA,
        DL_STRUCTURAL_COMMIT,
        requester,
    )
}

/// RSH-03: each structural triple of an OWL 2 DL axiom is shown by the graph it is in,
/// not the graph of the axiom's first triple.
#[test]
fn dl_structural_triples_keep_their_own_graph() {
    let reject = dl_structural(&open_reader());
    let shown = format!("{reject:?}");
    assert!(!shown.contains("SECRET_PROPERTY"), "{shown}");
}

/// Control: a reader of every graph sees the restriction's triples.
#[test]
fn dl_structural_full_reader_sees_the_restriction() {
    let reject = dl_structural(&Requester::all());
    let evidence = reject.explanation.expect("an explanation").evidence;
    let shown = |e: &&nrese_reasoner::RejectEvidence| e.object.contains("SECRET_PROPERTY");
    assert!(evidence.iter().any(|e| shown(&e)), "{evidence:#?}");
}

/// A justification of five axioms, three of them in `:secret`; the commit's two type
/// statements are the others.
const DL_COUNT_DATA: &str = "GRAPH :secret { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D }";
const DL_COUNT_COMMIT: &str = "GRAPH :open { :x a :A, :D }";

fn dl_count(requester: &Requester) -> MutationReject {
    let (config, mode) = (StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    rejection(config, mode, DL_COUNT_DATA, DL_COUNT_COMMIT, requester)
}

/// SYN-65: the OWL 2 DL rejection text has no number or name that depends on the secret
/// axioms: no withheld-statement count, no justification size, no engine, no time.
#[test]
fn dl_rejection_has_no_hidden_proof_counts() {
    let shown = format!("{:?}", dl_count(&open_reader()));
    for leak in [
        "of their statements",
        "axiom(s)",
        "hypertableau",
        "context-core",
        " ms)",
    ] {
        assert!(!shown.contains(leak), "{leak:?} in {shown}");
    }
}

/// Control: a reader of every graph keeps the justification's size and the check's time.
#[test]
fn dl_rejection_full_reader_keeps_counts_and_time() {
    let detail = dl_count(&Requester::all()).detail;
    assert!(
        detail.contains("5 axiom(s) have no model together"),
        "{detail}"
    );
    assert!(detail.contains(" ms)"), "{detail}");
}

/// A shape requiring `:p` of every `:B`; `:SECRET_FOCUS a :A` is in `:secret`, and the
/// commit makes every `:A` a `:B`.
fn shacl(requester: &Requester) -> MutationReject {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    };
    let data = format!(
        "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :B ; \
         sh:property [ sh:path :p ; sh:minCount 1 ] }} GRAPH :secret {{ :SECRET_FOCUS a :A }}",
        config.shapes_graph
    );
    let commit = "GRAPH :open { :A rdfs:subClassOf :B }";
    rejection(config, ReasoningMode::Owl2Rl, &data, commit, requester)
}

/// N12: a SHACL gate rejection doesn't name a focus node that only a graph the committer
/// can't read mentions, nor count results that depend on such graphs.
#[test]
fn shacl_rejection_does_not_name_private_focus() {
    let shown = format!("{:?}", shacl(&open_reader()));
    assert!(!shown.contains("SECRET_FOCUS"), "{shown}");
    assert!(!shown.contains("1 SHACL result(s)"), "{shown}");
}

/// Control: a reader of every graph sees the focus node and the count.
#[test]
fn shacl_rejection_full_reader_sees_the_focus() {
    let detail = shacl(&Requester::all()).detail;
    assert!(detail.contains("SECRET_FOCUS"), "{detail}");
    assert!(detail.contains("1 SHACL result(s)"), "{detail}");
}
