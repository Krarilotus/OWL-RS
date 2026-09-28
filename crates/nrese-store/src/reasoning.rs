//! Reasoner v2 in the store (reasoner-v2 design §4.3, R4): the configured ruleset's closure
//! over the asserted data is materialised into the engine's inferred stack, so every read
//! under the default `Materialised` model sees it.
//!
//! - **After bulk loads and at startup:** [`StoreService::rematerialise`] computes the
//!   closure with the batch executor and replaces the inferred stack in one revision
//!   ([`Rematerialisation`](nrese_engine::Rematerialisation)).
//! - **On commits:** the mutation pipeline recomputes the closure over the transaction's
//!   state and applies the difference to the inferred stack inside the same transaction
//!   ([`apply_to_transaction`]), so asserted and inferred changes publish atomically. That
//!   costs O(dataset) per commit until the delta executor (design §4.2) replaces it.
//!
//! Rules match over the union of all graphs and inferences go to the default graph
//! (design §6.2). OWL 2 RL derives some generalised triples, such as a literal typed by a
//! datatype property's range; RDF can't store a literal subject or a non-IRI predicate, so
//! those are dropped.

use std::time::{Duration, Instant};

use nrese_engine::{
    EncodedQuad, EncodedTriple, QuadPattern, ReadModel, TermId, TermKind, Transaction,
};
use nrese_reasoner::v2::batch::{self, Phases, Schema};
use nrese_reasoner::v2::ir::Vocabulary;
use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::naive::Violation;
use nrese_reasoner::v2::rulesets::Ruleset;
use oxrdf::{LiteralRef, NamedNodeRef, TermRef};

/// A closure computed over engine term ids.
#[derive(Debug, Default)]
pub struct Closure {
    /// The storable inferred statements (in the default graph).
    pub inferred: Vec<EncodedTriple>,
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<String>,
    pub rounds: usize,
    pub phases: Phases,
}

/// What a rematerialisation or a commit-path run did.
#[derive(Debug, Clone, Default)]
pub struct MaterialisationReport {
    pub ruleset: &'static str,
    /// The revision holding the new inferred stack (for commits: the commit's).
    pub revision: u64,
    pub asserted: u64,
    pub inferred: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub violations: usize,
    pub rounds: usize,
    pub elapsed: Duration,
}

/// Interns the rules' constants into the engine dictionary.
struct Constants<'a> {
    intern: &'a dyn Fn(TermRef<'_>) -> TermId,
}

impl Vocabulary for Constants<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        (self.intern)(NamedNodeRef::new_unchecked(iri).into()).raw()
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        let literal = LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype));
        (self.intern)(literal.into()).raw()
    }
}

/// Whether RDF can store the triple: an IRI or blank node subject and an IRI predicate.
fn storable([subject, predicate, _]: [u64; 3]) -> bool {
    matches!(
        TermId::from_raw(subject).kind(),
        TermKind::Iri | TermKind::BlankNode
    ) && TermId::from_raw(predicate).kind() == TermKind::Iri
}

/// The closure of `asserted` (any graphs) under `ruleset`; `intern` gives ids to constants.
pub fn materialise(
    ruleset: Ruleset,
    asserted: impl Iterator<Item = EncodedQuad>,
    intern: &dyn Fn(TermRef<'_>) -> TermId,
) -> Closure {
    let mut constants = Constants { intern };
    let rules = ruleset
        .rules(&mut constants)
        .expect("the built-in rulesets parse (tested)");
    let lists = ruleset
        .has_list_rules()
        .then(|| ListVocabulary::new(&mut constants));
    let schema = Schema::owl(&mut constants);
    let facts: Vec<[u64; 3]> = asserted
        .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
        .collect();
    let result = batch::materialise(&facts, &rules, lists.as_ref(), &schema);
    Closure {
        inferred: result
            .derived
            .into_iter()
            .filter(|&t| storable(t))
            .map(|[s, p, o]| {
                EncodedTriple::new(
                    TermId::from_raw(s),
                    TermId::from_raw(p),
                    TermId::from_raw(o),
                )
            })
            .collect(),
        violations: result.violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        phases: result.phases,
    }
}

/// Recomputes the closure over the transaction's asserted state and makes its inferred
/// stack equal to it. Returns the closure (whose `inferred` is emptied) and the counts.
pub fn apply_to_transaction(
    ruleset: Ruleset,
    tx: &mut Transaction<'_>,
) -> (Closure, MaterialisationReport) {
    let started = Instant::now();
    let asserted: Vec<EncodedQuad> = tx
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .collect();
    let asserted_count = asserted.len() as u64;
    let mut closure = {
        let tx: &Transaction<'_> = tx;
        materialise(ruleset, asserted.into_iter(), &|term| tx.intern(term))
    };
    let mut new = std::mem::take(&mut closure.inferred);
    new.sort_unstable();
    new.dedup();
    let mut current: Vec<EncodedTriple> = tx
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .map(EncodedTriple::from)
        .collect();
    current.sort_unstable();
    let (mut inserted, mut deleted) = (0, 0);
    for &triple in &current {
        if new.binary_search(&triple).is_err() && tx.remove_inferred(triple) {
            deleted += 1;
        }
    }
    for &triple in &new {
        if current.binary_search(&triple).is_err() && tx.insert_inferred(triple) {
            inserted += 1;
        }
    }
    let report = MaterialisationReport {
        ruleset: ruleset.name(),
        revision: tx.base().revision() + 1,
        asserted: asserted_count,
        inferred: new.len() as u64,
        inferred_inserted: inserted,
        inferred_deleted: deleted,
        violations: closure.violations.len(),
        rounds: closure.rounds,
        elapsed: started.elapsed(),
    };
    (closure, report)
}
