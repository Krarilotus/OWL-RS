//! Maintenance of a transaction's inferred stack on a commit ([`super::maintain`]).

use std::collections::HashSet;

use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId,
    Transaction,
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
    // Revived memberships must participate in the delta executor's introduced-only
    // consistency checks. Keep the batch result for installation below: no second run.
    let mut rebuilt = if revived {
        Some(super::materialise_until(
            program,
            &tx.pending_snapshot(),
            stop,
        )?)
    } else {
        None
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
        if let Some(closure) = &rebuilt {
            inserted.extend(
                closure
                    .inferred
                    .iter()
                    .map(|t| triple(t.in_default_graph()))
                    .filter(|&fact| !base.known_before(fact)),
            );
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
    if revived {
        recompute = true;
    } else if program.stores_representatives() {
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
    if recompute {
        let closure = match rebuilt.take() {
            Some(closure) => closure,
            None => super::materialise_until(program, &tx.pending_snapshot(), stop)?,
        };
        // O(stored closure + rebuilt closure) visits, with indexed transaction updates.
        // A stored snapshot shares the old runs and avoids expanding compact equality.
        // Keep the pre-maintenance inferred view for rollback if the final grounding
        // (which does not poll stop internally) crosses cancellation or a memory limit.
        if stop() {
            return Err(delta::Interrupted);
        }
        let before = tx.pending_snapshot().stored();
        for quad in before.quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all()) {
            tx.remove_inferred(quad.into());
        }
        for fact in closure.inferred {
            tx.insert_inferred(fact);
        }
        let counts = tx.inferred_pending();
        (inserted, removed) = (counts.0 as u64, counts.1 as u64);
        update.rounds += closure.rounds;
        // The delta cache may still contain heads rewritten for a formerly hidden
        // class. Ground the final prospective state, and publish this cache only if
        // the caller commits. Existing unrelated violations remain tolerated.
        update.program = Some(program.ground_program(&tx.pending_snapshot()));
        if stop() {
            let pending = tx.pending_snapshot().stored();
            for quad in pending.quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all()) {
                tx.remove_inferred(quad.into());
            }
            for quad in before.quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all()) {
                tx.insert_inferred(quad.into());
            }
            return Err(delta::Interrupted);
        }
    }
    Ok(Maintenance {
        violations: update.violations,
        inserted,
        removed,
        rounds: update.rounds,
        diagnostics: update.diagnostics,
        program: update.program,
    })
}

/// Applies `update` (over every identity, as the delta executor reads the expanded stack)
/// to an inferred stack kept over representatives (W4 stage B): each fact rewritten to
/// the representatives of the classes after the change. A change that merges classes
/// rewrites, in the same transaction, the stored facts that mention a representative
/// that lost its place, and stores each new identity as `identity sameAs representative`
/// (G5 of the investigation of 6 October 2026: before, such a commit published without
/// its inferences, and the store re-materialised in a second revision). Returns the
/// inferred statements added and removed; `None` if the change deletes an equality,
/// which may split a class (B3): maintenance rebuilds the stack in this transaction, and
/// nothing is applied.
fn store_over_representatives(
    program: &Program,
    update: &delta::Update,
    missing_axioms: &[Triple],
    tx: &mut Transaction<'_>,
) -> Option<(u64, u64)> {
    let same_as = program.same_as?;
    let equates = |t: &Triple| t[1] == same_as && t[0] != t[2];
    if tx.deleted().map(triple).any(|t| equates(&t)) || update.remove.iter().any(equates) {
        return None;
    }
    // The classes after the change: those before, merged by its new equalities.
    let mut classes = crate::representatives::EqualityClasses::default();
    if let Some(before) = tx.base().equality_classes() {
        for (representative, members) in before.iter() {
            for &member in members {
                classes.union(representative, member);
            }
        }
    }
    let equalities: Vec<(u64, u64)> = tx
        .inserted()
        .map(triple)
        .chain(update.insert.iter().copied())
        .filter(equates)
        .map(|[a, _, b]| (a, b))
        .collect();
    let lost: HashSet<u64> = classes.union_all(&equalities).into_iter().collect();
    let rewrite = |t: Triple| -> Triple { classes.rewrite(t) };
    let touches = |t: &Triple| t.iter().any(|&term| classes.class_of(term).is_some());
    let (mut inserted, mut removed) = (0, 0);
    // Stored facts that mention a former representative: the inferred ones move to the
    // new representative, the asserted ones get a copy over it; and each former
    // representative is now an identity of its class.
    let mut moved: Vec<Triple> = Vec::new();
    if !lost.is_empty() {
        let stored = tx.base().stored();
        let mut stale: Vec<Triple> = Vec::new();
        for &term in &lost {
            for position in 0..3 {
                let mut bound = [None; 3];
                bound[position] = Some(term);
                let pattern = pattern(bound, GraphSelector::Any);
                stale.extend(
                    stored
                        .quads_for_pattern_in(ReadModel::Inferred, &pattern)
                        .map(triple),
                );
                moved.extend(
                    stored
                        .quads_for_pattern_in(ReadModel::Asserted, &pattern)
                        .map(|quad| rewrite(triple(quad))),
                );
            }
        }
        // Each identity of a class a merge touched placed in it (a stored placement
        // `identity sameAs former` rewrites to `representative sameAs representative`).
        let merged: HashSet<u64> = lost.iter().map(|&t| classes.representative(t)).collect();
        for representative in merged {
            for &member in classes.members(representative).iter() {
                if member != representative {
                    moved.push([member, same_as, representative]);
                }
            }
        }
        stale.sort_unstable();
        stale.dedup();
        for fact in stale {
            moved.push(rewrite(fact));
            if tx.remove_inferred(encode(fact)) {
                removed += 1;
            }
        }
    }
    let asserted_somewhere = |tx: &Transaction<'_>, t: Triple| {
        tx.quads_for_pattern_in(
            ReadModel::Asserted,
            &pattern(t.map(Some), GraphSelector::Any),
        )
        .next()
        .is_some()
    };
    let mut removals: Vec<Triple> = update.remove.iter().map(|&t| rewrite(t)).collect();
    // A statement asserted now needs no inferred copy, unless its terms have identities:
    // then its form over representatives is stored, which reads expand to the other
    // identities (the delta executor never reports it, as it is asserted, here or in a
    // named graph alone).
    let mut asserted_copies: Vec<Triple> = Vec::new();
    for quad in tx.inserted().collect::<Vec<_>>() {
        let fact = rewrite(triple(quad));
        if touches(&fact) {
            asserted_copies.push(fact);
        } else {
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
        .chain(moved)
        .chain(asserted_copies)
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

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_engine::{Engine, EngineConfig};
    use nrese_rdf::{BlankNodeRef, NamedNodeRef};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cancelling_a_split_or_revival_discards_the_prospective_closure() {
        for revive in [false, true] {
            let engine = Engine::new(EngineConfig {
                background_maintenance: false,
                ..EngineConfig::default()
            })
            .unwrap();
            let mut tx = engine.transaction();
            let program =
                Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t))
                    .hiding_unnamed_classes(true)
                    .storing_representatives(true);
            engine.set_equality(program.same_as.map(TermId::from_raw));
            let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
            let [a, b, p, x, class] =
                ["a", "b", "p", "x", "Class"].map(|s| iri(&format!("http://example.com/{s}")));
            let range = iri("http://www.w3.org/2000/01/rdf-schema#range");
            let subclass = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
            let union = iri("http://www.w3.org/2002/07/owl#unionOf");
            let first = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
            let rest = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest");
            let nil = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil");
            let u = tx.intern(BlankNodeRef::new_unchecked("u").into()).raw();
            let list = tx.intern(BlankNodeRef::new_unchecked("list").into()).raw();
            let equality = [a, program.same_as.unwrap(), b];
            for fact in [
                equality,
                [a, p, x],
                [p, range, u],
                [u, union, list],
                [list, first, class],
                [list, rest, nil],
            ] {
                tx.insert_encoded(encode(fact).in_default_graph());
            }
            tx.commit().unwrap();
            let remat = engine.rematerialisation();
            let closure = super::super::materialise(&program, remat.base());
            remat.finish(closure.inferred).unwrap();
            let before = engine.snapshot();
            let ground = program.ground_program(&before);
            let change = |tx: &mut Transaction<'_>| {
                tx.remove_encoded(encode(equality).in_default_graph());
                if revive {
                    tx.insert_encoded(encode([u, subclass, class]).in_default_graph());
                }
            };
            // Count the cancellation checkpoints, including the final one after
            // grounding, which must roll inferred content back; no timing or race.
            let polls = AtomicUsize::new(0);
            let mut tx = engine.transaction();
            change(&mut tx);
            maintain(&program, Some(&ground), &mut tx, &|| {
                polls.fetch_add(1, Ordering::Relaxed);
                false
            })
            .unwrap();
            drop(tx);
            let total = polls.load(Ordering::Relaxed);
            assert!(total > 1);
            for at in [0, total / 2, total - 1] {
                polls.store(0, Ordering::Relaxed);
                let mut tx = engine.transaction();
                change(&mut tx);
                let result = maintain(&program, Some(&ground), &mut tx, &|| {
                    polls.fetch_add(1, Ordering::Relaxed) >= at
                });
                assert!(result.is_err(), "revive={revive}, checkpoint={at}");
                assert_eq!(tx.inferred_pending(), (0, 0));
                drop(tx);
                assert_eq!(engine.snapshot().revision(), before.revision());
            }
        }
    }
}
