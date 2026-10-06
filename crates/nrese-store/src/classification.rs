//! Classification and realisation of the store's asserted statements (all graphs), on
//! request, by the OWL 2 DL engines ([`crate::dl::classification`]): complete for OWL 2
//! DL within `dl.timeout`, and saying so when a budget or an unsupported construct leaves
//! a result incomplete (what it contains is entailed either way).

use std::time::Instant;

use nrese_engine::{Snapshot, TermId};

use crate::{StoreResult, StoreService};

/// The class hierarchy of the store's asserted ontology.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassificationReport {
    /// `(sub, super)` IRIs, sorted; equivalent classes both ways; `owl:Thing` left out.
    pub subsumptions: Vec<(String, String)>,
    /// Classes that can have no instance.
    pub unsatisfiable: Vec<String>,
    /// Classes equivalent to `owl:Thing`.
    pub equivalent_to_thing: Vec<String>,
    /// False if the ontology has no model: then every class is unsatisfiable.
    pub consistent: bool,
    /// The engine that classified: `context-core` (the Horn stage took the ontology) or
    /// `tableau` (the hypertableau driver).
    pub engine: &'static str,
    /// Why the hierarchy may lack subsumptions (empty: it is complete).
    pub incomplete: Vec<String>,
    pub micros: u64,
}

impl ClassificationReport {
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty()
    }
}

/// The named individuals' types under the store's asserted ontology.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RealisationReport {
    /// Per named individual (IRI, sorted): every named class it is an instance of.
    pub types: Vec<(String, Vec<String>)>,
    pub consistent: bool,
    /// The engine that classified (as [`ClassificationReport::engine`]).
    pub engine: &'static str,
    /// Why types may be missing (empty: complete).
    pub incomplete: Vec<String>,
    pub micros: u64,
}

impl RealisationReport {
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty()
    }
}

/// An IRI's text; `None` for any other term.
fn iri(snapshot: &Snapshot, id: u64) -> Option<String> {
    match snapshot.decode(TermId::from_raw(id)) {
        Some(nrese_rdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    }
}

fn sorted_iris(snapshot: &Snapshot, ids: &[u64]) -> Vec<String> {
    let mut out: Vec<String> = ids.iter().filter_map(|&c| iri(snapshot, c)).collect();
    out.sort();
    out
}

impl StoreService {
    /// Classifies the asserted statements of every graph under OWL 2 DL; for a scope that
    /// reads every graph.
    pub fn classify(&self, scope: &crate::ReadScope) -> StoreResult<ClassificationReport> {
        scope.require_all("classification")?;
        let started = Instant::now();
        let snapshot = self.engine().snapshot();
        let taxonomy = crate::dl::classification::taxonomy(self, &snapshot);
        let c = &taxonomy.classification;
        let mut subsumptions: Vec<(String, String)> = c
            .subsumptions
            .iter()
            .filter_map(|&(a, b)| Some((iri(&snapshot, a)?, iri(&snapshot, b)?)))
            .collect();
        subsumptions.sort();
        Ok(ClassificationReport {
            subsumptions,
            unsatisfiable: sorted_iris(&snapshot, &c.unsatisfiable),
            equivalent_to_thing: sorted_iris(&snapshot, &c.top),
            consistent: c.consistent,
            engine: taxonomy.profile.path,
            incomplete: taxonomy.incomplete.clone(),
            micros: started.elapsed().as_micros() as u64,
        })
    }

    /// Realises the asserted statements of every graph under OWL 2 DL: each named
    /// individual's types; for a scope that reads every graph.
    pub fn realise(&self, scope: &crate::ReadScope) -> StoreResult<RealisationReport> {
        scope.require_all("realisation")?;
        let started = Instant::now();
        let snapshot = self.engine().snapshot();
        let r = crate::dl::classification::realisation(self, &snapshot);
        let mut types: Vec<(String, Vec<String>)> = r
            .individuals
            .iter()
            .zip(&r.types)
            .filter_map(|(&a, classes)| Some((iri(&snapshot, a)?, sorted_iris(&snapshot, classes))))
            .collect();
        types.sort();
        let mut incomplete = r.taxonomy.incomplete.clone();
        incomplete.extend(r.incomplete.iter().cloned());
        Ok(RealisationReport {
            types,
            consistent: r.taxonomy.classification.consistent,
            engine: r.taxonomy.profile.path,
            incomplete,
            micros: started.elapsed().as_micros() as u64,
        })
    }
}
