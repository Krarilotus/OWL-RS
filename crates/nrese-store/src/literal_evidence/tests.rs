use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, vocab::xsd};
use nrese_sparql::{Completeness, Regime};

use super::*;
use crate::{PreparedQuery, SparqlQueryRequest, StoreConfig, StoreService};

const QUERY: &str = "SELECT DISTINCT ?o WHERE { <urn:evidence:s> <urn:evidence:p> ?o }";

fn coverage(snapshot: &Snapshot) -> Completeness {
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(QUERY)).unwrap();
    let mut claim = Completeness::under(Regime::Owl2Rl);
    crate::dl::query::certify_closure(&mut claim, prepared.reads(), "rules", Some(snapshot));
    claim
}

/// A commit's literal evidence belongs to the exact publication, including across deletion
/// of the last literal, while an older reader keeps the evidence for its own data.
#[test]
fn a_literal_claim_keeps_the_evidence_of_its_publication() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .update_str_ungated("INSERT DATA { <urn:evidence:s> <urn:evidence:p> 7 }")
        .unwrap();
    let old = store.engine().snapshot();
    assert!(
        coverage(&old)
            .header()
            .contains("two spellings of one value"),
        "the literal insert was not reflected in its publication"
    );
    store
        .update_str_ungated("DELETE DATA { <urn:evidence:s> <urn:evidence:p> 7 }")
        .unwrap();
    let new = store.engine().snapshot();
    let claim = coverage(&new);
    assert!(
        claim.complete,
        "the deletion must clear the literal evidence: {claim:?}"
    );
    assert!(
        coverage(&old)
            .header()
            .contains("two spellings of one value")
    );
}

/// An absent or refused record warrants only the generic spelling reason, even where
/// there is no literal in the query's actual answers.
#[test]
fn missing_literal_evidence_cannot_certify_the_answer() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store
        .update_str_ungated(
            "INSERT DATA { <urn:evidence:s> <urn:evidence:p> <urn:iri> ; <urn:other> 7 }",
        )
        .unwrap();
    let tx = store.engine().transaction();
    tx.install_certificate(store.unreasoned_basis(None));
    drop(tx);
    let snapshot = store.engine().snapshot();
    let check = || {
        let claim = coverage(&snapshot);
        assert!(
            !claim.complete,
            "missing literal evidence certified the answer: {claim:?}"
        );
        assert!(
            claim
                .header()
                .contains("literal evidence could not be checked"),
            "{claim:?}"
        );
    };
    check();
    Builder::new(Arc::new(Budget::new(1))).fill(&snapshot);
    assert!(matches!(
        ReadBasis::of(&snapshot).unwrap().literal_evidence.get(),
        Some(None)
    ));
    check();
}

fn typed_quad(n: usize) -> Quad {
    Quad::new(
        NamedNode::new_unchecked("urn:evidence:s"),
        NamedNode::new_unchecked(format!("urn:evidence:p:{n}")),
        Literal::new_typed_literal(n.to_string(), xsd::INTEGER),
        if n.is_multiple_of(2) {
            GraphName::DefaultGraph
        } else {
            NamedNode::new_unchecked(format!("urn:evidence:g:{n}")).into()
        },
    )
}

/// The store charges build scratch and persistent paths before allocation. A successor
/// deleting a scoped literal can share unaffected paths while a reader holds its base.
#[test]
fn literal_evidence_is_charged_through_its_last_publication() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut initial = engine.transaction();
    for n in 0..1024 {
        assert!(initial.insert(typed_quad(n).as_ref()));
    }
    initial.commit().unwrap();
    let mut tx = engine.transaction();
    let quad = typed_quad(17);
    let encoded = tx.base().lookup_quad(quad.as_ref()).unwrap();
    assert!(tx.remove(quad.as_ref()));

    let budget = Arc::new(Budget::unlimited());
    let probe = key(Some(encoded.predicate), Some(encoded.graph));
    let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
        let base = LiteralEvidence::build(tx.base(), &budget).unwrap();
        let next = base.after(&tx, &budget).unwrap();
        assert_eq!(base.counts.get(probe), [1, 0]);
        assert_eq!(next.counts.get(probe), [0, 0]);
        let held = base.clone();
        drop(base);
        drop(next);
        assert_eq!(held.counts.get(probe), [1, 0]);
        assert!(budget.used() > 0);
        drop(held);
    });
    assert_eq!((excess, budget.used()), (0, 0));
}

/// A quad asserted over the same inferred quad changes which stack can speak for it.
/// Deleting the assertion then removes it when no inference was kept.
#[test]
fn assertion_takeover_retracts_the_inferred_literal_count() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let quad = typed_quad(0);
    let mut seed = engine.transaction();
    let encoded = EncodedQuad::new(
        seed.intern(quad.subject.as_ref().into()),
        seed.intern(quad.predicate.as_ref().into()),
        seed.intern(quad.object.as_ref()),
        TermId::DEFAULT_GRAPH,
    );
    assert!(seed.insert_inferred(encoded.into()));
    seed.commit().unwrap();
    let budget = Arc::new(Budget::unlimited());
    let mut asserted = engine.transaction();
    let before = LiteralEvidence::build(asserted.base(), &budget).unwrap();
    assert!(asserted.insert(quad.as_ref()));
    let after = before.after(&asserted, &budget).unwrap();
    asserted.commit().unwrap();
    let probe = key(Some(encoded.predicate), Some(encoded.graph));
    assert_eq!(before.counts.get(probe), [0, 1]);
    assert_eq!(after.counts.get(probe), [1, 0]);
    let mut deleted = engine.transaction();
    assert!(deleted.remove(quad.as_ref()));
    assert_eq!(
        after.after(&deleted, &budget).unwrap().counts.get(probe),
        [0, 0]
    );
}
