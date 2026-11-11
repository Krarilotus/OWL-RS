//! The ontology the DL engines reason over: the store's asserted statements of every
//! graph, read by `nrese-owl`'s reverse RDF mapping over the store's term ids (no string
//! is decoded but the literals the data axioms need).

use nrese_engine::{
    GraphSelector, PendingWrite, QuadPattern, ReadModel, Snapshot, TermId, Transaction,
};

use crate::StoreResult;
use nrese_owl::{Ontology, Statement, TermKind, Terms};
use nrese_rdf::{NamedNodeRef, Term};

/// The individuals entailment tests add to an ontology (`¬(C ⊑ D)` as `(C ⊓ ¬D)(x)`):
/// apart from every term the data can hold (`nrese_dl::internal`), the same in every store.
pub(crate) const FRESH: usize = 8;

/// The text of the `i`th of the [`FRESH`] individuals.
pub(crate) fn fresh_iri(i: usize) -> String {
    nrese_dl::internal::individual(i)
}

/// The OWL vocabulary's IRIs and the fresh individuals of entailment tests.
fn vocabulary() -> impl Iterator<Item = String> {
    nrese_owl::Vocabulary::iris()
        .into_iter()
        .map(|(_, iri)| iri)
        .chain((0..FRESH).map(fresh_iri))
}

/// The OWL vocabulary's IRIs, interned so the reader finds every term it looks for
/// (`rdfs:Literal` in a data cardinality, which the data may not name: without its id the
/// axiom would be lost), and the fresh individuals of entailment tests. Interned in the
/// commits of a primary, so its replicas, which intern nothing, find them in the records.
pub(crate) fn intern_vocabulary(tx: &Transaction<'_>) {
    for iri in vocabulary() {
        tx.intern(NamedNodeRef::new_unchecked(&iri).into());
    }
}

/// [`intern_vocabulary`] outside a writer, for a read: system vocabulary (fixed names, the
/// lifecycle contract's limit L2), appended to the dictionary so the ids are every later
/// reader's. A primary's only; a replica interns nothing.
pub(crate) fn intern_system_vocabulary(engine: &nrese_engine::Engine) -> StoreResult<()> {
    let iris: Vec<String> = vocabulary().collect();
    #[allow(clippy::disallowed_methods)]
    engine.intern_system_vocabulary(
        iris.iter()
            .map(|iri| NamedNodeRef::new_unchecked(iri).into()),
    )?;
    Ok(())
}

/// The id of one of U1's own terms (fresh classes, Skolem constants, the clash: generated
/// from the published ontology and fixed names, never from a request), which the bounds keep
/// for later reads: appended as system vocabulary on a primary, looked up on a read replica
/// in `latest`, the latest snapshot ([`resolve`]'s rule). `None` for a term no record has
/// brought the replica yet. The append is never refused today; if it were, the term would
/// be missing, which U1 reports as it reports a replica's ([`super::upper::Interner`]).
pub(crate) fn resolve_system(
    replica: bool,
    engine: &nrese_engine::Engine,
    latest: &Snapshot,
    term: nrese_rdf::TermRef<'_>,
) -> Option<TermId> {
    match replica {
        true => latest.lookup(term),
        #[allow(clippy::disallowed_methods)]
        false => engine
            .intern_system_vocabulary([term])
            .ok()
            .and_then(|ids| ids.first().copied()),
    }
}

/// A private transaction for one request's own terms, charged to `budget`, whose ids are
/// compared by number with `published`'s (the DL tests' fresh names and conclusions): opened
/// over the latest snapshot, which must extend `published`
/// ([`nrese_engine::PrivateTransaction::extends`]; checked in debug builds).
pub(crate) fn private_over<'e>(
    engine: &'e nrese_engine::Engine,
    published: &Snapshot,
    budget: std::sync::Arc<nrese_exec::Budget>,
) -> nrese_engine::PrivateTransaction<'e> {
    let tx = engine.speculative(budget);
    debug_assert!(
        tx.extends(published),
        "a published input compared with a private transaction's ids is a prefix of its vocabulary"
    );
    tx
}

/// The id of `term` for the DL engines' own use in one request, in `tx`'s vocabulary:
/// interned on a primary, looked up on a read replica, whose dictionary must continue its
/// primary's (it interns nothing; `nrese_engine`'s replication). `Ok(None)` there for a term
/// no record has brought yet; `Err` where `tx`'s budget refused the term, apart from that.
pub(crate) fn resolve<T: PendingWrite>(
    replica: bool,
    tx: &T,
    term: nrese_rdf::TermRef<'_>,
) -> StoreResult<Option<TermId>> {
    match replica {
        true => Ok(tx.lookup(term)),
        false => Ok(Some(tx.try_intern(term)?)),
    }
}

/// The store's terms as `nrese-owl` reads them.
pub(crate) struct StoreTerms<'a> {
    pub decode: &'a dyn Fn(TermId) -> Option<Term>,
    pub lookup: &'a dyn Fn(&str) -> Option<TermId>,
}

impl Terms for StoreTerms<'_> {
    fn kind(&self, term: nrese_owl::Term) -> TermKind {
        match TermId::from_raw(term).kind() {
            nrese_engine::TermKind::Iri => TermKind::Iri,
            nrese_engine::TermKind::BlankNode => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: nrese_owl::Term) -> Option<String> {
        match (self.decode)(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<nrese_owl::Term> {
        (self.lookup)(iri).map(TermId::raw)
    }

    fn datatype(&self, term: nrese_owl::Term) -> Option<String> {
        match (self.decode)(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn language(&self, term: nrese_owl::Term) -> Option<String> {
        match (self.decode)(TermId::from_raw(term))? {
            Term::Literal(l) => l.language().map(str::to_owned),
            _ => None,
        }
    }

    fn iri_text(&self, term: nrese_owl::Term) -> Option<String> {
        match (self.decode)(TermId::from_raw(term))? {
            Term::NamedNode(n) => Some(n.into_string()),
            _ => None,
        }
    }
}

const ANY: QuadPattern = QuadPattern {
    subject: None,
    predicate: None,
    object: None,
    graph: GraphSelector::Any,
};

fn statement(q: nrese_engine::EncodedQuad) -> Statement {
    Statement {
        triple: [q.subject.raw(), q.predicate.raw(), q.object.raw()],
        graph: q.graph.raw(),
    }
}

/// Every asserted statement of `quads`, charged to `budget` as they are collected (SYN-61:
/// a store-scale list, the reader's scratch, freed once the ontology is read).
fn statements<B: std::ops::Deref<Target = nrese_exec::Budget>>(
    quads: impl Iterator<Item = nrese_engine::EncodedQuad>,
    budget: B,
) -> Result<nrese_exec::ChargedVec<Statement, B>, nrese_exec::BudgetExceeded> {
    let mut statements = nrese_exec::ChargedVec::new(budget);
    for q in quads {
        statements.push(statement(q))?;
    }
    Ok(statements)
}

/// The ontology of a transaction's asserted statements, after its changes, its scratch
/// charged to `budget` (refused: `Err`, nothing kept). O(asserted).
pub(crate) fn read_pending<B: std::ops::Deref<Target = nrese_exec::Budget>>(
    tx: &Transaction<'_>,
    budget: B,
) -> Result<Ontology, nrese_exec::BudgetExceeded> {
    let statements = statements(tx.quads_for_pattern_in(ReadModel::Asserted, &ANY), budget)?;
    let decode = |id| tx.decode(id);
    let lookup = |iri: &str| tx.lookup(NamedNodeRef::new_unchecked(iri).into());
    Ok(nrese_owl::read(
        &statements,
        &StoreTerms {
            decode: &decode,
            lookup: &lookup,
        },
    ))
}

/// The ontology of a transaction's statements asserted in the graphs `readable` admits,
/// after its changes: what a reader of only those graphs sees (a statement asserted there
/// and elsewhere too is one of them). Its scratch charged to `budget` as [`read_pending`]'s.
pub(crate) fn read_pending_where<B: std::ops::Deref<Target = nrese_exec::Budget>>(
    tx: &Transaction<'_>,
    budget: B,
    readable: impl Fn(nrese_engine::TermId) -> bool,
) -> Result<Ontology, nrese_exec::BudgetExceeded> {
    let quads = tx
        .quads_for_pattern_in(ReadModel::Asserted, &ANY)
        .filter(|quad| readable(quad.graph));
    let statements = statements(quads, budget)?;
    let decode = |id| tx.decode(id);
    let lookup = |iri: &str| tx.lookup(NamedNodeRef::new_unchecked(iri).into());
    Ok(nrese_owl::read(
        &statements,
        &StoreTerms {
            decode: &decode,
            lookup: &lookup,
        },
    ))
}

/// The ontology of a snapshot's asserted statements, its scratch charged to `budget`
/// (refused: `Err`, nothing kept). O(asserted).
pub(crate) fn read_snapshot<B: std::ops::Deref<Target = nrese_exec::Budget>>(
    snapshot: &Snapshot,
    budget: B,
) -> Result<Ontology, nrese_exec::BudgetExceeded> {
    let statements = statements(
        snapshot.quads_for_pattern_in(ReadModel::Asserted, &ANY),
        budget,
    )?;
    let decode = |id| snapshot.decode(id);
    let lookup = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    Ok(nrese_owl::read(
        &statements,
        &StoreTerms {
            decode: &decode,
            lookup: &lookup,
        },
    ))
}

/// Why an ontology wasn't read: its statements didn't fit `budget`'s memory.
pub(crate) fn refused(what: &str) -> String {
    nrese_dl::limits::Limit::Envelope.reached(format!("reading {what}'s statements"))
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
