//! The ontology the DL engines reason over: the store's asserted statements of every
//! graph, read by `nrese-owl`'s reverse RDF mapping over the store's term ids (no string
//! is decoded but the literals the data axioms need).

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, Transaction};
use nrese_owl::{Ontology, Statement, TermKind, Terms};
use nrese_rdf::{NamedNodeRef, Term};

/// The OWL vocabulary's IRIs, interned so the reader finds every term it looks for
/// (`rdfs:Literal` in a data cardinality, which the data may not name: without its id the
/// axiom would be lost).
pub(crate) fn intern_vocabulary(tx: &Transaction<'_>) {
    for (_, iri) in nrese_owl::Vocabulary::iris() {
        tx.intern(NamedNodeRef::new_unchecked(&iri).into());
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

/// The ontology of a transaction's asserted statements, after its changes. O(asserted).
pub(crate) fn read_pending(tx: &Transaction<'_>) -> Ontology {
    let statements: Vec<Statement> = tx
        .quads_for_pattern_in(ReadModel::Asserted, &ANY)
        .map(statement)
        .collect();
    let decode = |id| tx.decode(id);
    let lookup = |iri: &str| tx.lookup(NamedNodeRef::new_unchecked(iri).into());
    nrese_owl::read(
        &statements,
        &StoreTerms {
            decode: &decode,
            lookup: &lookup,
        },
    )
}

/// The ontology of a snapshot's asserted statements. O(asserted).
pub(crate) fn read_snapshot(snapshot: &Snapshot) -> Ontology {
    let statements: Vec<Statement> = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &ANY)
        .map(statement)
        .collect();
    let decode = |id| snapshot.decode(id);
    let lookup = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    nrese_owl::read(
        &statements,
        &StoreTerms {
            decode: &decode,
            lookup: &lookup,
        },
    )
}
