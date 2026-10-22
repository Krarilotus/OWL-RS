//! Maintenance of a transaction's inferred stack on a commit ([`super::maintain`]).

use std::collections::HashSet;

use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId,
    Transaction,
};

use rayon::prelude::*;

use super::{Program, encode, pattern, storable, triple};
use crate::delta::{self, Base, MemoryBase};
use crate::eval::GroundProgram;
use crate::ir::{Triple, Violations};
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

    fn scan_asserted(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Asserted, &pattern(bound, GraphSelector::Any))
        {
            if !self.deletes.contains(&quad) {
                f(triple(quad));
            }
        }
        self.inserts.scan(bound, f);
        let matches = |fact: &Triple| {
            bound
                .iter()
                .zip(fact)
                .all(|(want, have)| want.is_none_or(|want| want == *have))
        };
        for axiom in self.axioms.iter().filter(|axiom| matches(axiom)) {
            f(*axiom);
        }
    }
}

/// What [`maintain`] did to a transaction's inferred stack.
#[derive(Debug, Default)]
pub struct Maintenance {
    /// The consistency violations the change introduced.
    pub violations: Violations,
    pub inserted: u64,
    pub removed: u64,
    pub rounds: usize,
    /// List axioms the change introduced that weren't instantiated.
    pub diagnostics: Vec<ListDiagnostic>,
    /// The ground program after the change, to reuse for the next commit.
    pub program: Option<GroundProgram>,
    /// What the maintenance policy weighed and did ([`delta::policy`]).
    pub work: delta::policy::Counts,
}

/// Keeps the transaction's inferred stack equal to the closure of its asserted state,
/// given that it was before the transaction's changes: runs the delta executor and applies
/// its result.
///
/// `stop` is polled throughout the reasoning; when it fires, [`delta::Interrupted`] is
/// returned and `tx`'s inferred stack may be partly updated: the caller drops `tx`.
pub fn maintain(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: crate::eval::Stop<'_>,
) -> Result<Maintenance, delta::Interrupted> {
    let unlimited = std::sync::Arc::new(nrese_exec::Budget::unlimited());
    maintain_within(program, ground, tx, stop, &unlimited)
}

/// [`maintain`], the violations it finds charged to `budget` (the commit's operation:
/// [`Violations`]).
pub fn maintain_within(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: crate::eval::Stop<'_>,
    budget: &std::sync::Arc<nrese_exec::Budget>,
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
        Some(super::materialise_within(
            program,
            &tx.pending_snapshot(),
            stop,
            budget,
        )?)
    } else {
        None
    };
    // A first load (no asserted fact before, nothing deleted): its closure is the whole
    // closure, so it is materialised once, by the batch executor, and its violations are
    // checked as the delta executor checks them, through the new facts only
    // ([`delta::introduced_violations`]): an axiom's violation from before stays tolerated.
    // The delta executor built one job per ground rule and copied each job's matches
    // before the round ran: a 250-class subclass chain with 400 individuals held 757 MiB in
    // a round that derived nothing, where a materialisation holds about 140 (DMW's A/B,
    // fleet pm-dmwab-3, 21 Oct 2026). Not over a compact stack: its closure lists
    // equalities over representatives, which the datatype checks don't read as the delta's
    // expanded ones (Sol r1 F1). Nor where the closure concludes a fact no store holds (a
    // literal subject): whether the change derives such a fact anew can't be told from a
    // state that can't hold it, so the delta executor, which follows the derivations,
    // decides those (Sol r2 F1, r3 F1).
    let mut first_load = !revived
        && !program.stores_representatives()
        && program
            .plan
            .is_none_or(|plan| plan == delta::policy::Plan::Rebuild)
        && tx.base().len_in(ReadModel::Asserted) == 0
        && tx.deleted().next().is_none()
        && tx.inserted().next().is_some();
    if first_load {
        let closure = super::materialise_within(program, &tx.pending_snapshot(), stop, budget)?;
        if closure.unstorable.is_empty() {
            rebuilt = Some(closure);
        } else {
            first_load = false;
        }
    }
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
        if let Some(closure) = rebuilt.as_ref().filter(|_| !first_load) {
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
        // A rebuild reads the asserted facts and writes the closure, both about the size
        // of the stored state; the policy offers it to a change that inserts nothing
        // ([`delta::policy::Plan::Rebuild`]).
        let policy = delta::policy::Policy {
            force: program.plan,
            rebuild: inserted
                .is_empty()
                .then(|| tx.base().len_in(ReadModel::Materialised)),
            split_rebuilds: program.stores_representatives(),
        };
        if inserted.is_empty() && deleted.is_empty() {
            delta::Update {
                violations: Violations::new(std::sync::Arc::clone(budget)),
                ..delta::Update::default()
            }
        } else if let Some(closure) = rebuilt.as_ref().filter(|_| first_load) {
            // The new inferred facts, as the delta executor reports them (its `insert`): every
            // one storable (above), so the state tells which it held before.
            let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
            let derived: Vec<Triple> = closure
                .inferred
                .iter()
                .map(|t| triple(t.in_default_graph()))
                .filter(|&f| {
                    !base.is_asserted(f) && !(base.contains(f) && !inserted_set.contains(&f))
                })
                .collect();
            delta::Update {
                violations: delta::introduced_violations(
                    &base, &inserted, &derived, rules, ground, stop, budget,
                )?,
                insert: derived,
                policy: delta::policy::Counts {
                    plan: delta::policy::Plan::Rebuild,
                    ..Default::default()
                },
                ..Default::default()
            }
        } else {
            delta::update_within(
                &base, &inserted, &deleted, rules, ground, stop, policy, budget,
            )?
        }
    };
    let mut update = update;
    super::datatypes::violations(
        &update.insert,
        program.rdf_type,
        program.same_as,
        &|id| tx.decode(TermId::from_raw(id)),
        &mut update.violations,
    );
    update.violations.finish();
    let (mut inserted, mut removed) = (0, 0);
    // Over representatives, a change that may split a class is rebuilt (B3a: the
    // translation can't follow it; the representative view for the delta executor is B3b).
    let mut rebuild = revived || update.policy.plan == delta::policy::Plan::Rebuild;
    if !rebuild && program.stores_representatives() {
        match store_over_representatives(program, &update, &missing_axioms, tx) {
            Some((added, dropped)) => (inserted, removed) = (added, dropped),
            None => rebuild = true,
        }
    } else if !rebuild {
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
    if rebuild {
        let mut closure = match rebuilt.take() {
            Some(closure) => closure,
            None => super::materialise_within(program, &tx.pending_snapshot(), stop, budget)?,
        };
        // An interruption drops the transaction, these changes with it.
        if stop() {
            return Err(delta::Interrupted);
        }
        // Only the difference is applied: O(stored + rebuilt closure) to sort and walk,
        // and a transaction update per fact that changed. A stored snapshot shares the old
        // runs and avoids expanding compact equality.
        let mut before: Vec<EncodedTriple> = tx
            .pending_snapshot()
            .stored()
            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
            .map(EncodedTriple::from)
            .collect();
        before.par_sort_unstable();
        before.dedup();
        closure.inferred.par_sort_unstable();
        closure.inferred.dedup();
        // One merge walk over both (a binary search per fact cost 130 ms of a 1.4 M-fact
        // stack's rebuild, against 70 for its materialisation).
        let (old, new) = (&before, &closure.inferred);
        let (mut i, mut j) = (0, 0);
        while i < old.len() || j < new.len() {
            if j == new.len() || (i < old.len() && old[i] < new[j]) {
                removed += u64::from(tx.remove_inferred(old[i]));
                i += 1;
            } else if i == old.len() || new[j] < old[i] {
                inserted += u64::from(tx.insert_inferred(new[j]));
                j += 1;
            } else {
                (i, j) = (i + 1, j + 1);
            }
        }
        update.rounds += closure.rounds;
        // The delta cache may still contain heads rewritten for a formerly hidden
        // class. Ground the final prospective state, and publish this cache only if
        // the caller commits. Existing unrelated violations remain tolerated.
        let after = program.ground_program(&tx.pending_snapshot());
        if update.policy.plan == delta::policy::Plan::Rebuild {
            // No delta ran: the list axioms the change left unusable, as it reports them.
            let before = match ground {
                Some(ground) => std::borrow::Cow::Borrowed(ground),
                None => std::borrow::Cow::Owned(program.ground_program(tx.base())),
            };
            update.diagnostics = after
                .list_diagnostics
                .iter()
                .filter(|d| !before.list_diagnostics.contains(d))
                .copied()
                .collect();
        }
        update.program = Some(after);
        if stop() {
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
        work: update.policy,
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
        // What the commit deletes stays deleted: its asserted statements, and the inferred
        // facts the update removes (over every identity, so their stored form too).
        let deleted: HashSet<EncodedQuad> = tx.deleted().collect();
        let removal: HashSet<Triple> = update.remove.iter().copied().collect();
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
                        .filter(|quad| !deleted.contains(quad))
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
            if !removal.contains(&fact) {
                moved.push(rewrite(fact));
            }
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

    /// B3a: random commits that merge and split `owl:sameAs` classes (asserted equalities,
    /// functional and inverse-functional properties) leave the inferred stack equal to a
    /// fresh closure, under each plan of the maintenance policy forced and the cheapest, in
    /// both store modes (every copy, or over representatives). Varied by `NRESE_FUZZ_SEED`.
    #[test]
    fn every_maintenance_plan_keeps_the_stack_equal_to_recomputation() {
        use crate::delta::policy::Plan;
        let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        let plans = [
            None,
            Some(Plan::Dred),
            Some(Plan::Recompute),
            Some(Plan::Rebuild),
        ];
        let mut failures: Vec<String> = Vec::new();
        for compact in [false, true] {
            for plan in plans {
                let mut taken: HashSet<Plan> = HashSet::new();
                for seed in 1..=6u64 {
                    let mut state = (seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)).max(1);
                    let mut below = move |n: usize| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        (state % n as u64) as usize
                    };
                    let engine = Engine::new(EngineConfig {
                        background_maintenance: false,
                        ..EngineConfig::default()
                    })
                    .unwrap();
                    let mut tx = engine.transaction();
                    let program =
                        Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| {
                            tx.intern(t)
                        })
                        .storing_representatives(compact)
                        .maintaining_by(plan);
                    if compact {
                        engine.set_equality(program.same_as.map(TermId::from_raw));
                    }
                    let iri = |s: &str| {
                        let name = format!("http://example.com/{s}");
                        tx.intern(NamedNodeRef::new_unchecked(&name).into()).raw()
                    };
                    let entities: Vec<u64> = (0..8).map(|i| iri(&format!("e{i}"))).collect();
                    let [p, q, f, i, c, d] = ["p", "q", "f", "i", "C", "D"].map(iri);
                    let same_as = program.same_as.unwrap();
                    let vocabulary =
                        |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
                    let rdf_type = vocabulary("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
                    let functional = vocabulary("http://www.w3.org/2002/07/owl#FunctionalProperty");
                    let inverse =
                        vocabulary("http://www.w3.org/2002/07/owl#InverseFunctionalProperty");
                    let sub_property =
                        vocabulary("http://www.w3.org/2000/01/rdf-schema#subPropertyOf");
                    let sub_class = vocabulary("http://www.w3.org/2000/01/rdf-schema#subClassOf");
                    let schema = [
                        [f, rdf_type, functional],
                        [i, rdf_type, inverse],
                        [q, sub_property, p],
                        [c, sub_class, d],
                    ];
                    // Equalities half the time: classes form, merge and split.
                    let fact = |below: &mut dyn FnMut(usize) -> usize| -> Triple {
                        let predicate = [same_as, same_as, same_as, p, q, f, i, rdf_type];
                        let predicate = predicate[below(predicate.len())];
                        let object = match predicate == rdf_type {
                            true => c,
                            false => entities[below(entities.len())],
                        };
                        [entities[below(entities.len())], predicate, object]
                    };
                    let mut asserted: Vec<Triple> = schema.to_vec();
                    for _ in 0..14 {
                        asserted.push(fact(&mut below));
                    }
                    for &fact in &asserted {
                        tx.insert_encoded(encode(fact).in_default_graph());
                    }
                    tx.commit().unwrap();
                    let remat = engine.rematerialisation();
                    let closure = super::super::materialise(&program, remat.base());
                    remat.finish(closure.inferred).unwrap();
                    let mut ground = program.ground_program(&engine.snapshot());
                    for step in 0..12 {
                        // Every other commit deletes only (what a rebuild is offered for).
                        let changes: Vec<(Triple, bool)> = (0..1 + below(3))
                            .map(|_| match step % 2 == 0 && !asserted.is_empty() {
                                true => (asserted[below(asserted.len())], false),
                                false => {
                                    let fact = fact(&mut below);
                                    (fact, !asserted.contains(&fact))
                                }
                            })
                            .collect();
                        let mut tx = engine.transaction();
                        for &(fact, insert) in &changes {
                            let quad = encode(fact).in_default_graph();
                            if insert {
                                tx.insert_encoded(quad);
                                asserted.push(fact);
                            } else {
                                tx.remove_encoded(quad);
                                asserted.retain(|&a| a != fact);
                            }
                        }
                        let done =
                            maintain(&program, Some(&ground), &mut tx, crate::eval::NEVER).unwrap();
                        assert!(done.violations.is_empty(), "no inconsistency is possible");
                        let took = done.work.plan;
                        taken.insert(took);
                        tx.commit().unwrap();
                        let snapshot = engine.snapshot();
                        if let Some(program) = done.program {
                            ground = program;
                        }
                        let stored: HashSet<EncodedTriple> = snapshot
                            .stored()
                            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
                            .map(Into::into)
                            .collect();
                        let fresh: HashSet<EncodedTriple> =
                            super::super::materialise(&program, &snapshot)
                                .inferred
                                .into_iter()
                                .filter(|t| {
                                    !snapshot
                                        .contains_in(ReadModel::Asserted, &t.in_default_graph())
                                })
                                .collect();
                        if stored != fresh {
                            let text = |t: &EncodedTriple| {
                                let [s, p, o] = triple(t.in_default_graph()).map(|id| {
                                    super::super::decoded(snapshot.decode(TermId::from_raw(id)), id)
                                });
                                format!("{s} {p} {o}")
                            };
                            let extra: Vec<String> = stored.difference(&fresh).map(text).collect();
                            let missing: Vec<String> =
                                fresh.difference(&stored).map(text).collect();
                            let changes: Vec<String> = changes
                                .iter()
                                .map(|(fact, insert)| {
                                    let sign = if *insert { "+" } else { "-" };
                                    format!("{sign}{}", text(&encode(*fact)))
                                })
                                .collect();
                            failures.push(format!(
                                "compact {compact}, plan {plan:?} (took {took:?}), seed {seed}, step {step}: \
                                 {changes:?}; extra {extra:?}; missing {missing:?}"
                            ));
                            break;
                        }
                    }
                }
                if let Some(plan) = plan {
                    assert!(
                        taken.contains(&plan),
                        "compact {compact}: {plan:?} never taken"
                    );
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{}",
            failures.join(
                "
"
            )
        );
    }

    /// A rebuild runs no delta, and still reports the list axioms a deletion leaves
    /// unusable, as the delta does.
    #[test]
    fn a_rebuild_reports_the_list_axioms_a_deletion_breaks() {
        use crate::delta::policy::Plan;
        let mut reported = Vec::new();
        for plan in [Plan::Dred, Plan::Rebuild] {
            let engine = Engine::new(EngineConfig {
                background_maintenance: false,
                ..EngineConfig::default()
            })
            .unwrap();
            let mut tx = engine.transaction();
            let program =
                Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t))
                    .maintaining_by(Some(plan));
            let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
            let [x, c, d] = ["X", "C", "D"].map(|s| iri(&format!("http://example.com/{s}")));
            let intersection = iri("http://www.w3.org/2002/07/owl#intersectionOf");
            let first = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
            let rest = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest");
            let nil = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil");
            let [l, m] = ["l", "m"].map(|s| tx.intern(BlankNodeRef::new_unchecked(s).into()).raw());
            let tail = [m, rest, nil];
            for fact in [
                [x, intersection, l],
                [l, first, c],
                [l, rest, m],
                [m, first, d],
                tail,
            ] {
                tx.insert_encoded(encode(fact).in_default_graph());
            }
            tx.commit().unwrap();
            let remat = engine.rematerialisation();
            let closure = super::super::materialise(&program, remat.base());
            remat.finish(closure.inferred).unwrap();
            let ground = program.ground_program(&engine.snapshot());
            let mut tx = engine.transaction();
            tx.remove_encoded(encode(tail).in_default_graph());
            let done = maintain(&program, Some(&ground), &mut tx, crate::eval::NEVER).unwrap();
            assert_eq!(done.work.plan, plan);
            reported.push(done.diagnostics);
        }
        assert!(!reported[0].is_empty(), "the list lost its end");
        assert_eq!(reported[0], reported[1]);
    }

    /// A first load is materialised once, not run through the delta executor: its stack
    /// is the recomputed closure, an inconsistent one is still reported, and the next
    /// commit, onto a loaded state, takes the delta.
    #[test]
    fn a_first_load_is_materialised_and_the_next_commit_takes_the_delta() {
        use crate::delta::policy::Plan;
        for consistent in [true, false] {
            let engine = Engine::new(EngineConfig {
                background_maintenance: false,
                ..EngineConfig::default()
            })
            .unwrap();
            let mut tx = engine.transaction();
            let program =
                Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t));
            let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
            let rdf_type = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
            let subclass = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
            let disjoint = iri("http://www.w3.org/2002/07/owl#disjointWith");
            // A subclass chain of 20 with 10 individuals at the bottom (DMW's chain, small).
            let class = |n: usize| iri(&format!("http://example.com/C{n}"));
            let mut facts: Vec<Triple> = (1..20)
                .map(|n| [class(n), subclass, class(n - 1)])
                .collect();
            facts.extend((0..10).map(|i| {
                [
                    iri(&format!("http://example.com/i{i}")),
                    rdf_type,
                    class(19),
                ]
            }));
            if !consistent {
                facts.push([class(0), disjoint, class(19)]);
            }
            let next = [iri("http://example.com/j"), rdf_type, class(10)];
            for &fact in &facts {
                tx.insert_encoded(encode(fact).in_default_graph());
            }
            let done = maintain(&program, None, &mut tx, crate::eval::NEVER).unwrap();
            assert_eq!(done.work.plan, Plan::Rebuild, "consistent {consistent}");
            assert_eq!(
                done.violations.is_empty(),
                consistent,
                "{:?}",
                done.violations
            );
            if !consistent {
                continue;
            }
            tx.commit().unwrap();
            let snapshot = engine.snapshot();
            let stored: HashSet<EncodedTriple> = snapshot
                .stored()
                .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
                .map(Into::into)
                .collect();
            let fresh: HashSet<EncodedTriple> = super::super::materialise(&program, &snapshot)
                .inferred
                .into_iter()
                .filter(|t| !snapshot.contains_in(ReadModel::Asserted, &t.in_default_graph()))
                .collect();
            assert_eq!(stored, fresh);
            let ground = done.program.expect("the ground program after the load");
            let mut tx = engine.transaction();
            tx.insert_encoded(encode(next).in_default_graph());
            let done = maintain(&program, Some(&ground), &mut tx, crate::eval::NEVER).unwrap();
            assert_ne!(
                done.work.plan,
                Plan::Rebuild,
                "a loaded state takes the delta"
            );
        }
    }

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
            // grounding; no timing or race.
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
                drop(tx);
                assert_eq!(engine.snapshot().revision(), before.revision());
            }
        }
    }
}
