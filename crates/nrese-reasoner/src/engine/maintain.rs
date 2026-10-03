//! Maintenance of a transaction's inferred stack on a commit ([`super::maintain`]).

use std::collections::HashSet;

use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, ReadModel, Snapshot, TermId, Transaction,
};

use super::{Program, encode, pattern, storable, triple};
use crate::delta::{self, Base, MemoryBase};
use crate::eval::GroundProgram;
use crate::ir::{Triple, Violation};
use crate::lists::ListDiagnostic;

/// A transaction's state as the delta executor reads it: the committed indexes (asserted
/// and inferred, any graph) without the pending deletes, plus the pending inserts, indexed
/// in memory. Scanning the transaction itself would filter every pending insert on each
/// lookup.
pub struct EngineBase<'a> {
    snapshot: &'a Snapshot,
    inserts: MemoryBase,
    deletes: HashSet<EncodedQuad>,
    /// The ruleset's axioms (sorted): they count as asserted, so no deletion retracts them.
    axioms: &'a [Triple],
}

impl<'a> EngineBase<'a> {
    pub fn new(tx: &'a Transaction<'_>, axioms: &'a [Triple]) -> Self {
        let inserts: Vec<Triple> = tx.inserted().map(triple).collect();
        Self {
            snapshot: tx.base(),
            inserts: MemoryBase::new(&inserts, &[]),
            deletes: tx.deleted().collect(),
            axioms,
        }
    }

    /// Whether `fact` was in the state before the transaction (any graph, either stack).
    fn known_before(&self, fact: Triple) -> bool {
        let [s, p, o] = fact.map(Some);
        self.snapshot
            .quads_for_pattern_in(
                ReadModel::Materialised,
                &pattern([s, p, o], GraphSelector::Any),
            )
            .next()
            .is_some()
    }

    fn committed(&self, model: ReadModel, fact: Triple) -> bool {
        let [s, p, o] = fact.map(Some);
        self.snapshot
            .quads_for_pattern_in(model, &pattern([s, p, o], GraphSelector::Any))
            .any(|quad| !self.deletes.contains(&quad))
    }
}

impl Base for EngineBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any))
        {
            if !self.deletes.contains(&quad) {
                f(triple(quad));
            }
        }
        self.inserts.scan(bound, f);
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let committed = self
            .snapshot
            .count_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any));
        usize::try_from(committed).unwrap_or(usize::MAX) + self.inserts.estimate(bound)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.inserts.contains(fact) || self.committed(ReadModel::Materialised, fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.inserts.contains(fact)
            || self.axioms.binary_search(&fact).is_ok()
            || self.committed(ReadModel::Asserted, fact)
    }
}

/// What [`maintain`] did to a transaction's inferred stack.
#[derive(Debug, Default)]
pub struct Maintenance {
    /// The consistency violations the change introduced.
    pub violations: Vec<Violation>,
    pub inserted: u64,
    pub removed: u64,
    pub rounds: usize,
    /// List axioms the change introduced that weren't instantiated.
    pub diagnostics: Vec<ListDiagnostic>,
    /// A change the inferred stack can't follow commit by commit (an unnamed class left
    /// out became consumable; a change merging or splitting `owl:sameAs` classes while the
    /// stack is kept over representatives): the caller rematerialises after the commit.
    pub needs_rematerialisation: bool,
    /// The ground program after the change, to reuse for the next commit.
    pub program: Option<GroundProgram>,
}

/// Keeps the transaction's inferred stack equal to the closure of its asserted state,
/// given that it was before the transaction's changes: runs the delta executor and applies
/// its result.
///
/// `stop` is polled throughout the reasoning; when it fires, nothing has been applied to
/// `tx`'s inferred stack and [`delta::Interrupted`] is returned.
pub fn maintain(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: crate::eval::Stop<'_>,
) -> Result<Maintenance, delta::Interrupted> {
    // Axioms the state lacks (a store that never rematerialised): added with this commit.
    let mut missing_axioms: Vec<Triple> = Vec::new();
    // Unnamed classes left out: as the commit leaves them; one it makes consumable
    // needs a rematerialisation for the memberships left out so far.
    let (schema, revived) = if program.hide_unnamed_classes {
        let before = program.hidden_classes(tx.base());
        let after = program.hidden_classes(&tx.pending_snapshot());
        let revived = before.keys().any(|class| !after.contains_key(class));
        (program.schema.clone().hiding(after), revived)
    } else {
        (program.schema.clone(), false)
    };
    let rules = delta::Rules {
        schema: &schema,
        ..program.rules()
    };
    let update = {
        let base = EngineBase::new(tx, &program.axioms);
        // Facts new to the state: in no graph and not inferred before the transaction.
        let mut inserted: Vec<Triple> = tx
            .inserted()
            .map(triple)
            .filter(|&t| !base.known_before(t))
            .collect();
        if !(inserted.is_empty() && tx.deleted().next().is_none()) {
            missing_axioms = program
                .axioms
                .iter()
                .copied()
                .filter(|&axiom| !base.known_before(axiom) && !base.inserts.contains(axiom))
                .collect();
            inserted.extend(missing_axioms.iter().copied());
        }
        inserted.sort_unstable();
        inserted.dedup();
        // A triple deleted from one graph but still asserted in another stays a fact.
        let mut deleted: Vec<Triple> = tx
            .deleted()
            .map(triple)
            .filter(|&t| !base.is_asserted(t))
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        if inserted.is_empty() && deleted.is_empty() {
            delta::Update::default()
        } else {
            delta::update_until(&base, &inserted, &deleted, rules, ground, stop)?
        }
    };
    let mut update = update;
    update.violations.extend(super::datatypes::violations(
        &update.insert,
        program.rdf_type,
        program.same_as,
        &|id| tx.decode(TermId::from_raw(id)),
    ));
    let (mut inserted, mut removed) = (0, 0);
    let mut recompute = false;
    if program.stores_representatives() {
        match store_over_representatives(program, &update, &missing_axioms, tx) {
            Some((added, dropped)) => (inserted, removed) = (added, dropped),
            None => recompute = true,
        }
    } else {
        // A statement asserted in any graph is explicit, never also inferred (the engine
        // enforces that for the default graph only).
        let asserted_now: Vec<EncodedTriple> = tx.inserted().map(EncodedTriple::from).collect();
        for fact in update.remove.iter().map(|&t| encode(t)).chain(asserted_now) {
            if tx.remove_inferred(fact) {
                removed += 1;
            }
        }
        for &fact in update
            .insert
            .iter()
            .chain(&missing_axioms)
            .filter(|&&t| storable(t))
        {
            if tx.insert_inferred(encode(fact)) {
                inserted += 1;
            }
        }
    }
    Ok(Maintenance {
        violations: update.violations,
        inserted,
        removed,
        rounds: update.rounds,
        diagnostics: update.diagnostics,
        needs_rematerialisation: revived || recompute,
        program: update.program,
    })
}

/// Applies `update` (over every identity, as the delta executor reads the expanded stack)
/// to an inferred stack kept over representatives (W4 stage B): each fact rewritten to
/// its representatives. Returns the inferred statements added and removed; `None` if the
/// change merges or splits `owl:sameAs` classes, which rewrites every fact about them: the
/// caller recomputes the stack after the commit, and nothing is applied.
fn store_over_representatives(
    program: &Program,
    update: &delta::Update,
    missing_axioms: &[Triple],
    tx: &mut Transaction<'_>,
) -> Option<(u64, u64)> {
    let same_as = program.same_as?;
    let equates = |t: &Triple| t[1] == same_as && t[0] != t[2];
    let asserted_change = tx
        .inserted()
        .chain(tx.deleted())
        .map(triple)
        .any(|t| equates(&t));
    if asserted_change || update.insert.iter().chain(&update.remove).any(equates) {
        return None;
    }
    let classes = tx.base().equality_classes();
    let rewrite = |t: Triple| -> Triple {
        match &classes {
            Some(classes) => t.map(|term| classes.representative(TermId::from_raw(term)).raw()),
            None => t,
        }
    };
    let touches = |t: &Triple| {
        classes.as_ref().is_some_and(|classes| {
            t.iter()
                .any(|&term| classes.class_of(TermId::from_raw(term)).is_some())
        })
    };
    let asserted_somewhere = |tx: &Transaction<'_>, t: Triple| {
        tx.quads_for_pattern_in(
            ReadModel::Asserted,
            &pattern(t.map(Some), GraphSelector::Any),
        )
        .next()
        .is_some()
    };
    let (mut inserted, mut removed) = (0, 0);
    let mut removals: Vec<Triple> = update.remove.iter().map(|&t| rewrite(t)).collect();
    // A statement asserted now needs no inferred copy, unless its terms have identities.
    for quad in tx.inserted().collect::<Vec<_>>() {
        let fact = rewrite(triple(quad));
        if !touches(&fact) {
            removals.push(fact);
        }
    }
    removals.sort_unstable();
    removals.dedup();
    for fact in removals {
        if tx.remove_inferred(encode(fact)) {
            removed += 1;
        }
    }
    let mut additions: Vec<Triple> = update
        .insert
        .iter()
        .chain(missing_axioms)
        .map(|&t| rewrite(t))
        .filter(|&t| storable(t))
        .collect();
    // A statement deleted from the default graph but still asserted in another stays a
    // fact (the delta executor never sees it), and its copies for other identities stay
    // inferred: their representative form must stay stored, which the deleted statement
    // may have been.
    let still_asserted: Vec<Triple> = tx
        .deleted()
        .filter(|quad| quad.graph.is_default_graph())
        .map(triple)
        .collect();
    for fact in still_asserted {
        let representative = rewrite(fact);
        if touches(&representative) && storable(representative) && asserted_somewhere(tx, fact) {
            additions.push(representative);
        }
    }
    additions.sort_unstable();
    additions.dedup();
    for fact in additions {
        if !touches(&fact) && asserted_somewhere(tx, fact) {
            continue;
        }
        if tx.insert_inferred(encode(fact)) {
            inserted += 1;
        }
    }
    Some((inserted, removed))
}
