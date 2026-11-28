use nrese_engine::{EncodedQuad, EncodedTriple, Engine, EngineConfig};
use nrese_rdf::{Literal, vocab::xsd};
use nrese_sparql::GraphAccess;

use super::*;
use crate::{ReadScope, SparqlQueryRequest};

fn id(tx: &nrese_engine::Transaction<'_>, iri: &str) -> TermId {
    tx.intern(NamedNodeRef::new_unchecked(iri).into())
}

fn query(text: &str, access: Option<GraphAccess>) -> PreparedQuery {
    PreparedQuery::parse(&SparqlQueryRequest::new(
        text,
        ReadScope::of(access.map(Arc::new)),
    ))
    .unwrap()
}

const OBJECTS: &str = "SELECT DISTINCT ?o WHERE { <urn:s> <urn:p> ?o }";

/// The spelling lookup reads a renamed predicate's representative in the default graph.
/// The same predicate in another graph is not an equality copy.
#[test]
fn spelling_evidence_follows_aliases_in_the_default_graph_only() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let representative = id(&tx, "urn:rep");
    let alias = id(&tx, "urn:p");
    let subject = id(&tx, "urn:s");
    let same_as = id(&tx, "http://www.w3.org/2002/07/owl#sameAs");
    let graph = id(&tx, "urn:g");
    let literal = Literal::new_typed_literal("7", xsd::INTEGER);
    let literal = tx.intern(literal.as_ref().into());
    tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: representative,
    });
    tx.insert_encoded(EncodedQuad::new(subject, representative, literal, graph));
    tx.read_equality_as(Some(same_as));
    tx.commit().unwrap();

    let prepared = query(OBJECTS, None);
    let (query, reads) = prepared.reads();
    let named_only = engine.snapshot();
    assert!(spelling_reason(query, reads, Some(&named_only)).is_none());

    let mut tx = engine.transaction();
    tx.insert_encoded(EncodedQuad::new(
        subject,
        representative,
        literal,
        TermId::DEFAULT_GRAPH,
    ));
    tx.commit().unwrap();
    let default = engine.snapshot();
    assert!(
        spelling_reason(query, reads, Some(&default))
            .unwrap()
            .contains("two spellings of one value")
    );
    assert!(spelling_reason(query, reads, Some(&named_only)).is_none());
}

/// A reader of some graphs gets the same structural, unchecked reason whether or not
/// its snapshot holds a literal under the compared predicate.
#[test]
fn a_restricted_spelling_reason_does_not_inspect_hidden_data() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let subject = id(&tx, "urn:s");
    let predicate = id(&tx, "urn:p");
    let iri = id(&tx, "urn:o");
    let public = id(&tx, "urn:public");
    tx.insert_encoded(EncodedQuad::new(subject, predicate, iri, public));
    tx.commit().unwrap();
    let visible = engine.snapshot();
    let mut tx = engine.transaction();
    let graph = id(&tx, "urn:hidden");
    let literal = Literal::new_typed_literal("7", xsd::INTEGER);
    let literal = tx.intern(literal.as_ref().into());
    tx.insert_encoded(EncodedQuad::new(subject, predicate, literal, graph));
    tx.commit().unwrap();

    let prepared = query(
        OBJECTS,
        Some(GraphAccess {
            graphs: vec!["urn:public".into()],
            inferred: true,
            ..GraphAccess::default()
        }),
    );
    let (query, reads) = prepared.reads();
    let claim = |snapshot: &Snapshot| spelling_reason(query, reads, Some(snapshot));
    let reason = claim(&visible);
    assert!(
        reason
            .as_ref()
            .is_some_and(|r| r.contains("literal evidence could not be checked")),
        "the restricted comparison needs the generic reason: {reason:?}"
    );
    assert_eq!(
        claim(&engine.snapshot()),
        reason,
        "hidden literals changed the spelling reason"
    );
}

/// Subject and predicate positions alone cannot bind a literal. Neither a filter
/// nor DISTINCT over those variables should request the cold equality classes.
#[test]
fn spelling_evidence_skips_subject_and_predicate_comparisons() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let subject = id(&tx, "urn:s");
    let alias = id(&tx, "urn:alias");
    let predicate = id(&tx, "urn:p");
    let same_as = id(&tx, "http://www.w3.org/2002/07/owl#sameAs");
    let literal = Literal::new_typed_literal("7", xsd::INTEGER);
    let literal = tx.intern(literal.as_ref().into());
    tx.insert_encoded(EncodedQuad::new(
        subject,
        predicate,
        literal,
        TermId::DEFAULT_GRAPH,
    ));
    tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: literal,
    });
    tx.read_equality_as(Some(same_as));
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    for access in [
        None,
        Some(GraphAccess {
            inferred: true,
            ..GraphAccess::default()
        }),
    ] {
        let prepared = query(
            "SELECT DISTINCT ?s ?p WHERE { ?s ?p ?o FILTER(?s = <urn:s> && ?p = <urn:p>) }",
            access,
        );
        let (query, reads) = prepared.reads();
        #[cfg(debug_assertions)]
        let before = nrese_engine::witness::get(nrese_engine::witness::Site::RunProbes);
        assert!(spelling_reason(query, reads, Some(&snapshot)).is_none());
        #[cfg(debug_assertions)]
        assert_eq!(
            nrese_engine::witness::get(nrese_engine::witness::Site::RunProbes),
            before
        );
    }
}

/// A comparison whose predicate has no value literals need only seek its own ranges,
/// even if other predicates contain many typed values.
#[cfg(debug_assertions)]
#[test]
fn spelling_evidence_seeks_the_compared_predicate() {
    use nrese_engine::witness::{Site, get};
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let subject = id(&tx, "urn:s");
    let compared = id(&tx, "urn:p");
    let other = id(&tx, "urn:other");
    let iri = id(&tx, "urn:o");
    tx.insert_encoded(EncodedQuad::new(
        subject,
        compared,
        iri,
        TermId::DEFAULT_GRAPH,
    ));
    for value in 0..2048 {
        let literal = Literal::new_typed_literal(value.to_string(), xsd::INTEGER);
        let literal = tx.intern(literal.as_ref().into());
        tx.insert_encoded(EncodedQuad::new(
            subject,
            other,
            literal,
            TermId::DEFAULT_GRAPH,
        ));
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let prepared = query(OBJECTS, None);
    let (query, reads) = prepared.reads();
    let before = get(Site::ScanSteps);
    assert!(spelling_reason(query, reads, Some(&snapshot)).is_none());
    let steps = get(Site::ScanSteps) - before;
    assert!(
        steps <= 3,
        "the spelling check scanned {steps} keys outside its ranges"
    );
}
