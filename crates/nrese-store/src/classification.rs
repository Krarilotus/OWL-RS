//! OWL 2 EL classification of the store's asserted statements (all graphs), on request
//! ([`nrese_reasoner::v2::classify`]).

use std::collections::BTreeMap;
use std::time::Instant;

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TermKind};
use nrese_rdf::{LiteralRef, NamedNodeRef};
use nrese_reasoner::v2::classify::classify;
use nrese_reasoner::v2::ir::Vocabulary;

use crate::{StoreResult, StoreService};

/// The class hierarchy of the store's asserted ontology.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassificationReport {
    /// `(sub, super)` IRIs, sorted; equivalent classes both ways; `owl:Thing` left out.
    pub subsumptions: Vec<(String, String)>,
    /// Classes that can have no instance.
    pub unsatisfiable: Vec<String>,
    /// Axioms outside OWL 2 EL that were skipped, by kind.
    pub skipped: BTreeMap<&'static str, usize>,
    pub micros: u64,
}

/// Ids of the vocabulary the classifier looks for; a term the store doesn't hold gets an
/// id no statement has.
struct Lookup<'a> {
    snapshot: &'a Snapshot,
    missing: u64,
}

impl Vocabulary for Lookup<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        self.snapshot
            .lookup(NamedNodeRef::new_unchecked(iri).into())
            .map_or_else(
                || {
                    self.missing -= 1;
                    self.missing
                },
                TermId::raw,
            )
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        let literal = LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype));
        self.snapshot.lookup(literal.into()).map_or_else(
            || {
                self.missing -= 1;
                self.missing
            },
            TermId::raw,
        )
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        let literal = LiteralRef::new_language_tagged_literal_unchecked(lexical, language);
        self.snapshot.lookup(literal.into()).map_or_else(
            || {
                self.missing -= 1;
                self.missing
            },
            TermId::raw,
        )
    }
}

impl StoreService {
    /// Classifies the asserted statements of every graph under OWL 2 EL.
    pub fn classify(&self) -> StoreResult<ClassificationReport> {
        let started = Instant::now();
        let snapshot = self.engine().snapshot();
        let mut triples: Vec<[u64; 3]> = snapshot
            .quads_for_pattern_in(
                ReadModel::Asserted,
                &QuadPattern {
                    subject: None,
                    predicate: None,
                    object: None,
                    graph: GraphSelector::Any,
                },
            )
            .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
            .collect();
        triples.sort_unstable();
        triples.dedup();
        let mut vocabulary = Lookup {
            snapshot: &snapshot,
            missing: u64::MAX,
        };
        let result = classify(&triples, &mut vocabulary, &|id| {
            TermId::from_raw(id).kind() == TermKind::Iri
        });
        let text = |id: u64| match snapshot.decode(TermId::from_raw(id)) {
            Some(nrese_rdf::Term::NamedNode(n)) => n.into_string(),
            other => format!("{other:?}"),
        };
        let mut skipped = BTreeMap::new();
        for (_, kind) in &result.skipped {
            *skipped.entry(*kind).or_default() += 1;
        }
        let mut subsumptions: Vec<(String, String)> = result
            .subsumptions
            .iter()
            .map(|&(a, b)| (text(a), text(b)))
            .collect();
        subsumptions.sort();
        let mut unsatisfiable: Vec<String> =
            result.unsatisfiable.iter().map(|&c| text(c)).collect();
        unsatisfiable.sort();
        Ok(ClassificationReport {
            subsumptions,
            unsatisfiable,
            skipped,
            micros: started.elapsed().as_micros() as u64,
        })
    }
}
