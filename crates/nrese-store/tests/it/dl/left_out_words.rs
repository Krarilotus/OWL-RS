//! The sentence that tells a user the reader left an axiom out (`dl::consistency::left_out`)
//! names it as the data writes it, so the user can find it: an IRI as `<iri>`, a blank node
//! by a statement of the data that points to it or one of its own, never a term's id or a
//! debug form. Its named form is made at the commit, for the verdict and the record readers
//! of every graph are told from; a reader of only some graphs is told no name or statement
//! from a graph it can't read; a read that makes the sentence itself names nothing.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::GraphAccess;
use nrese_store::{
    DlAnswers, MutationCommand, MutationError, MutationTicket, ReadScope, Requester,
    SparqlQueryRequest, SparqlUpdateRequest, StoreError, WriteScope,
};

use super::queries::query_with;
use super::{PREFIXES, insert, pipeline};

const EX: &str = "http://example.com/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// The left-out sentence among `reasons`, if one is.
fn sentence(reasons: impl IntoIterator<Item = String>) -> Option<String> {
    reasons.into_iter().find(|r| r.contains("were left out"))
}

/// Whether `text` holds neither a debug form nor a term's id.
fn in_words(text: &str) -> bool {
    !text.contains('{') && !text.contains("term ")
}

/// (case, data, whether a read is marked by it, the texts one of which names it): one row
/// per kind of left-out part a store can hold. The first five are the parts the rules may
/// still read, which mark every read; the last two leave only the verdict unknown.
fn cases() -> Vec<(&'static str, String, bool, Vec<String>)> {
    let q = ":q a owl:ObjectProperty .";
    vec![
        (
            "reserved name",
            "rdfs:member rdfs:domain :C . :x rdfs:member :v .".into(),
            true,
            vec![format!("<{RDFS}member>")],
        ),
        (
            "OWL outside the mapping",
            format!(
                ":R owl:onProperty :q ; owl:someValuesFrom :D . :R rdfs:subClassOf :C . :x :q :v . {q}"
            ),
            true,
            vec![
                format!("<{EX}R> <{OWL}onProperty> <{EX}q>"),
                format!("<{EX}R> <{OWL}someValuesFrom> <{EX}D>"),
            ],
        ),
        (
            "malformed restriction",
            format!(
                "[ owl:onProperty :q ; owl:someValuesFrom :D, :F ] rdfs:subClassOf :C . :F a owl:Class . :x :q :v . {q}"
            ),
            true,
            vec![
                format!("<{OWL}onProperty> <{EX}q>"),
                format!("<{OWL}someValuesFrom> <{EX}D>"),
                format!("<{OWL}someValuesFrom> <{EX}F>"),
                format!("<{RDFS}subClassOf> <{EX}C>"),
            ],
        ),
        (
            "broken list",
            format!(
                ":A a owl:Class ; owl:intersectionOf _:l . _:l <{RDF}first> :C ; <{RDF}rest> _:m . \
                 _:m <{RDF}first> :D . :x a :A ."
            ),
            true,
            vec![format!("<{EX}A> <{OWL}intersectionOf>")],
        ),
        (
            "syntax not read",
            ":C rdfs:subClassOf [ owl:onProperties ( :d ) ; owl:someValuesFrom xsd:integer ] . \
             :d a owl:DatatypeProperty . :x a :C ."
                .into(),
            true,
            vec![format!("<{EX}C> <{RDFS}subClassOf>")],
        ),
        (
            "ambiguous property",
            ":p a owl:ObjectProperty , owl:DatatypeProperty . :x :p :v .".into(),
            false,
            vec![format!("<{EX}p>")],
        ),
        (
            "non-simple property",
            ":r a owl:TransitiveProperty . :C rdfs:subClassOf [ owl:onProperty :r ; \
             owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] . :x a :C ."
                .into(),
            false,
            vec![format!("<{EX}r>")],
        ),
    ]
}

/// Each case, committed by a writer of every graph: the verdict's reason, and where the
/// part marks reads, a read's reason and an exact read's refusal, say in words what was
/// left out and name it as the data writes it.
#[test]
fn a_left_out_part_is_named_as_the_data_writes_it() {
    let mut wrong = Vec::new();
    for (case, data, marks, names) in cases() {
        let dl = pipeline();
        insert(&dl, &format!(":C a owl:Class . :D a owl:Class . {data}")).expect("committed");
        let named = |text: &str| in_words(text) && names.iter().any(|n| text.contains(n));
        let status = dl.store().dl().status().expect("a verdict");
        let verdict = status.consistency.verdict.reason().map(str::to_owned);
        if !sentence(verdict.clone()).is_some_and(|s| named(&s)) {
            wrong.push(format!("{case}, the verdict: {verdict:?}"));
        }
        if !marks {
            continue;
        }
        let read = query_with(&dl, "ASK { :x a :C }", None).expect("read");
        let reasons = read.1.reasons();
        if !sentence(reasons.clone()).is_some_and(|s| named(&s)) {
            wrong.push(format!("{case}, the read: {reasons:?}"));
        }
        match query_with(&dl, "ASK { :x a :C }", Some(DlAnswers::Exact)) {
            Err(StoreError::Incomplete(answer)) if named(&answer.to_string()) => {}
            other => wrong.push(format!("{case}, the exact read: {other:?}")),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A reader of the default graph and `open` only.
fn open_only() -> Arc<GraphAccess> {
    Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    })
}

/// A reserved name in a graph the reader can't read and a broken list in one it can: a
/// reader of `open` only is told no name or statement of the first anywhere, and no left-out
/// sentence that names either, through its read, its exact read and its own commit's
/// rejection (which shows its own graphs' statements). A reader of every graph is told the
/// first, named.
#[test]
fn a_reader_of_some_graphs_is_told_no_name_from_the_left_out_sentence() {
    let dl = pipeline();
    insert(
        &dl,
        &format!(
            "GRAPH :secret {{ rdfs:member rdfs:domain :Hidden . :h rdfs:member :hv . }} \
             GRAPH :open {{ :A a owl:Class ; owl:intersectionOf _:l . _:l <{RDF}first> :C ; \
             <{RDF}rest> _:m . _:m <{RDF}first> :D . :x a :A . }}"
        ),
    )
    .expect("committed");
    let store = dl.store();
    let secret = ["Hidden", "example.com/h", "rdf-schema#member"];
    // A left-out sentence's first part, up to the text's next quote, naming a term.
    let names = |text: &str| {
        let parts = text.split("left out; first: ").skip(1);
        parts
            .filter_map(|rest| rest.split('"').next())
            .any(|s| s.contains('<'))
    };
    let leaks = |text: &str| secret.iter().any(|s| text.contains(*s)) || names(text);
    let mut told = Vec::new();
    for mode in [None, Some(DlAnswers::Exact)] {
        let mut request = SparqlQueryRequest::new(
            format!("{PREFIXES}ASK {{ :x a :C }}"),
            ReadScope::Graphs(open_only()),
        );
        request.dl_answers = mode;
        let prepared = store.prepare_query(&request).expect("prepared");
        let mut out = Vec::new();
        told.push(match super::run_dl(store, &prepared, &mut out) {
            Ok(status) => format!("{status:?}"),
            Err(error) => error.to_string(),
        });
    }
    let committer = Requester::new(
        ReadScope::Graphs(open_only()),
        WriteScope::Graphs(open_only()),
    );
    let rejected = dl.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}INSERT DATA {{ GRAPH :open {{ :C owl:disjointWith :D . :y a :C , :D }} }}"
        ))),
        &committer,
        &MutationTicket::new(),
    );
    let Err(MutationError::Rejected(reject)) = rejected else {
        panic!("expected a rejection, got {rejected:?}");
    };
    told.push(format!("{reject:?}"));
    let wrong: Vec<_> = told.iter().filter(|t| leaks(t)).collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    let all = query_with(&dl, "ASK { :x a :C }", None)
        .expect("read")
        .1
        .reasons();
    let all = sentence(all.clone()).filter(|s| in_words(s) && s.contains('<'));
    assert!(all.is_some(), "a reader of every graph: {all:?}");
}

/// A read that makes the sentence itself (a revision no commit here recorded: the store
/// reopened) says what was left out in words and names nothing: it decodes no term.
#[test]
fn a_read_without_the_commits_record_names_nothing() {
    let on_disk = |dir: &std::path::Path| {
        let store = nrese_store::StoreService::new(nrese_store::StoreConfig::on_disk(dir));
        let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
        let reasoner = Arc::new(ReasonerService::new(config));
        nrese_store::MutationPipeline::new(Arc::new(store.expect("store")), reasoner)
    };
    let dir = tempfile::tempdir().expect("dir");
    let data = ":C a owl:Class . rdfs:member rdfs:domain :C . :x rdfs:member :v .";
    insert(&on_disk(dir.path()), data).expect("committed");
    let dl = on_disk(dir.path());
    let reasons = query_with(&dl, "ASK { :x a :C }", None)
        .expect("read")
        .1
        .reasons();
    let told = sentence(reasons.clone());
    let nameless = told
        .as_ref()
        .is_some_and(|s| in_words(s) && !s.contains('<'));
    assert!(nameless, "{reasons:?}");
}
