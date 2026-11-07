//! Classification and realisation of the store's asserted statements (all graphs), on
//! request, by the OWL 2 DL engines ([`crate::dl::classification`]): complete for OWL 2
//! DL within `dl.timeout`, and saying so when a budget or an unsupported construct leaves
//! a result incomplete (what it contains is entailed either way).

use std::time::Instant;

use nrese_engine::{Snapshot, TermId};

use crate::dl::Verdict;
use crate::{StoreResult, StoreService};

/// The class hierarchy of the store's asserted ontology. An inconsistent ontology lists
/// nothing: every class is unsatisfiable then, and "this class can have no instance"
/// describes a consistent ontology only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationReport {
    /// `(sub, super)` IRIs, sorted; equivalent classes both ways; `owl:Thing` left out.
    pub subsumptions: Vec<(String, String)>,
    /// Classes that can have no instance.
    pub unsatisfiable: Vec<String>,
    /// Classes equivalent to `owl:Thing`.
    pub equivalent_to_thing: Vec<String>,
    /// Whether the ontology has a model; `Unknown`: what is listed holds either way.
    pub consistency: Verdict,
    /// The engine that classified: `context-core` (the Horn stage took the ontology) or
    /// `tableau` (the hypertableau driver).
    pub engine: &'static str,
    /// Why the hierarchy may lack subsumptions (empty: it is complete).
    pub incomplete: Vec<String>,
    pub micros: u64,
}

impl ClassificationReport {
    /// Nothing left out, and the consistency decided.
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty() && self.consistency.decided().is_some()
    }
}

/// The named individuals' types under the store's asserted ontology; none for an
/// inconsistent one (as [`ClassificationReport`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealisationReport {
    /// Per named individual (IRI, sorted): every named class it is an instance of.
    pub types: Vec<(String, Vec<String>)>,
    pub consistency: Verdict,
    /// The engine that classified (as [`ClassificationReport::engine`]).
    pub engine: &'static str,
    /// Why types may be missing (empty: complete).
    pub incomplete: Vec<String>,
    pub micros: u64,
}

impl RealisationReport {
    /// Nothing left out, and the consistency decided.
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty() && self.consistency.decided().is_some()
    }
}

/// An IRI's text; `None` for any other term.
fn iri(snapshot: &Snapshot, id: u64) -> Option<String> {
    match snapshot.decode(TermId::from_raw(id)) {
        Some(nrese_rdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    }
}

/// A report's reasons: the engines', and an undecided verdict's own where they list none,
/// so an answer without a verdict never reads as complete in any form.
fn reasons(consistency: &Verdict, mut incomplete: Vec<String>) -> Vec<String> {
    if let Some(why) = consistency.reason()
        && incomplete.is_empty()
    {
        incomplete.push(format!("consistency not decided: {why}"));
    }
    incomplete
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
        let listed = c.consistency != Verdict::Inconsistent;
        let mut subsumptions: Vec<(String, String)> = c
            .subsumptions
            .iter()
            .filter(|_| listed)
            .filter_map(|&(a, b)| Some((iri(&snapshot, a)?, iri(&snapshot, b)?)))
            .collect();
        subsumptions.sort();
        let list = |ids: &[u64]| match listed {
            true => sorted_iris(&snapshot, ids),
            false => Vec::new(),
        };
        Ok(ClassificationReport {
            subsumptions,
            unsatisfiable: list(&c.unsatisfiable),
            equivalent_to_thing: list(&c.top),
            consistency: c.consistency.clone(),
            engine: taxonomy.profile.path,
            incomplete: reasons(&c.consistency, taxonomy.incomplete.clone()),
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
        let consistency = r.taxonomy.classification.consistency.clone();
        let mut types: Vec<(String, Vec<String>)> = r
            .individuals
            .iter()
            .zip(&r.types)
            .filter(|_| consistency != Verdict::Inconsistent)
            .filter_map(|(&a, classes)| Some((iri(&snapshot, a)?, sorted_iris(&snapshot, classes))))
            .collect();
        types.sort();
        let mut incomplete = r.taxonomy.incomplete.clone();
        incomplete.extend(r.incomplete.iter().cloned());
        Ok(RealisationReport {
            types,
            incomplete: reasons(&consistency, incomplete),
            consistency,
            engine: r.taxonomy.profile.path,
            micros: started.elapsed().as_micros() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report's reasons are never empty while its verdict is undecided: the verdict's
    /// own reason stands in where the engines listed none (N4 design, G4-1: every form
    /// carries the verdict or its absence).
    #[test]
    fn an_undecided_verdict_always_carries_a_reason() {
        let budget = Verdict::Unknown("gave up: the node budget".to_owned());
        assert_eq!(
            reasons(&budget, Vec::new()),
            ["consistency not decided: gave up: the node budget"]
        );
        let listed = vec!["consistency not decided (unsupported): x".to_owned()];
        assert_eq!(reasons(&budget, listed.clone()), listed);
        for decided in [Verdict::Consistent, Verdict::Inconsistent] {
            assert!(reasons(&decided, Vec::new()).is_empty());
        }
    }
}
