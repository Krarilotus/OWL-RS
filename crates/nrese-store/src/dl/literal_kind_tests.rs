use super::{query, source, upper::Upper};
use crate::{StoreConfig, StoreService};
use nrese_engine::{GraphSelector, QuadPattern, ReadModel};
use nrese_owl::{Axiom, Ontology};
use nrese_rdf::NamedNodeRef;
use std::sync::Arc;

fn fixture() -> (StoreService, [u64; 3]) {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .update_str_ungated(
            "PREFIX : <urn:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         INSERT DATA { :dp a owl:DatatypeProperty . :a :dp 1 .
         :holder :value <<( :s :p :o )>> . }",
        )
        .unwrap();
    let snapshot = store.engine().snapshot();
    let id = |iri| {
        snapshot
            .lookup(NamedNodeRef::new_unchecked(iri).into())
            .unwrap()
    };
    let triple = snapshot
        .quads_for_pattern_in(
            ReadModel::Materialised,
            &QuadPattern {
                predicate: Some(id("urn:value")),
                graph: GraphSelector::Any,
                ..QuadPattern::all()
            },
        )
        .next()
        .unwrap()
        .object
        .raw();
    let atom = [id("urn:a").raw(), id("urn:dp").raw(), triple];
    (store, atom)
}

// Spec: exact-query/explanation ground atoms classify nonliteral objects as objects,
// never DataPropertyAssertion, even when the RDF object is a stored triple term.
#[test]
fn d066_f1_exact_triple_object_is_not_a_data_assertion() {
    let (store, atom) = fixture();
    let snapshot = store.engine().snapshot();
    let got = query::ground_axiom(
        &mut Ontology::default(),
        &snapshot,
        atom,
        query::Ids::of(&snapshot),
    )
    .unwrap();
    assert!(
        matches!(got, Axiom::ObjectPropertyAssertion(p, s, o) if [s,p,o] == atom),
        "{got:?}"
    );
}

// Spec: Upper::is_assertion only retains data-property changes with literal objects;
// a triple-term object requires rebuilding, rather than taking literal maintenance.
#[test]
fn d066_f1_upper_triple_object_requires_recompile() {
    let (store, atom) = fixture();
    let tx = store.engine().speculative();
    source::intern_vocabulary(&tx);
    let snapshot = tx.base();
    let kept = store.dl().bounds.kept();
    let ontology = source::read_snapshot(snapshot, Arc::clone(kept)).unwrap();
    let normalised = nrese_owl::normalise(&ontology);
    let upper = Upper::build(
        &ontology,
        &normalised,
        snapshot,
        &|t| Some(tx.intern(t)),
        nrese_reasoner::eval::NEVER,
        kept,
    )
    .unwrap();
    assert!(
        !upper.is_assertion(atom),
        "triple terms are not literal data-property updates"
    );
    let literal = snapshot
        .lookup(nrese_rdf::Literal::from(1).as_ref().into())
        .unwrap()
        .raw();
    assert!(upper.is_assertion([atom[0], atom[1], literal]));
}
