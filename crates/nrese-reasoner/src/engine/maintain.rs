//! Maintenance of a transaction's inferred stack on a commit ([`super::maintain`]).

use std::collections::HashSet;
use std::sync::Arc;

use nrese_engine::{
    CountedSnapshot, EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, TermId,
    Transaction,
};
use nrese_exec::Budget;

use rayon::prelude::*;

use super::{Program, encode, pattern, storable, triple};
use crate::delta::{self, Base, MemoryBase, VisitsBase};
use crate::eval::{GroundProgram, OperationStop, Poller, Probe, Stopped, Visit};
use crate::ir::{Triple, Violations};
use crate::lists::ListDiagnostic;

/// A transaction's state as the delta executor reads it: the committed indexes (asserted
/// and inferred, any graph) without the pending deletes, plus the pending inserts, indexed
/// in memory. Scanning the transaction itself would filter every pending insert on each
/// lookup.
///
/// The committed state is read only counted ([`CountedSnapshot`]): every read's work in
/// the engine (its runs, merge steps, deleted copies, equality copies and class build)
/// counts on the reading task's poller, so a stop is seen within its interval whatever the
/// engine passes over (REAS-3, sweeps SW016–SW019).
pub struct EngineBase<'a> {
    snapshot: CountedSnapshot,
    inserts: MemoryBase,
    deletes: HashSet<EncodedQuad>,
    /// The ruleset's axioms (sorted): they count as asserted, so no deletion retracts them.
    axioms: &'a [Triple],
}

impl<'a> EngineBase<'a> {
    pub fn new(tx: &'a Transaction<'_>, axioms: &'a [Triple]) -> Self {
        let inserts: Vec<Triple> = tx.inserted().map(triple).collect();
        Self {
            snapshot: tx.base().counted(),
            inserts: MemoryBase::new(&inserts, &[]),
            deletes: tx.deleted().collect(),
            axioms,
        }
    }

    /// Whether `fact` was in the state before the transaction (any graph, either stack): one
    /// lookup, counted on `poll` before it is made (review r1 F5); [`Stopped`] once `poll`
    /// has stopped, never a "not known" that would keep the fact (fix check r1, rule 2).
    fn known_before(&self, fact: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
        #[cfg(test)]
        crate::witness::add(crate::witness::Site::KnownBefore, 1);
        if poll.tick() {
            return Err(Stopped);
        }
        let [s, p, o] = fact.map(Some);
        // Its work in the engine counted too (REAS-3, SW016).
        let first = self
            .snapshot
            .quads(
                ReadModel::Materialised,
                &pattern([s, p, o], GraphSelector::Any),
                &|n| poll.visit(n),
            )
            .next()
            .transpose()?;
        Ok(first.is_some())
    }

    /// Whether `fact` is committed in `model` and not deleted by the change: each graph's
    /// copy it visits counted on `probe`, the deleted ones included (REAS-2, sol-loop r1
    /// S2); [`Stopped`] once that has stopped before a copy was found.
    fn committed(
        &self,
        model: ReadModel,
        fact: Triple,
        probe: Probe<'_, '_>,
    ) -> Result<bool, Stopped> {
        let [s, p, o] = fact.map(Some);
        // The engine's work below each copy counted too (REAS-3, SW017).
        let visit = |n| probe.visited(n);
        for quad in self
            .snapshot
            .quads(model, &pattern([s, p, o], GraphSelector::Any), &visit)
        {
            let quad = quad?;
            if !probe.units(1) {
                return Err(Stopped);
            }
            if !self.deletes.contains(&quad) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Reads count as [`Base`] says: each quad a scan visits is a position, the deleted ones it
/// passes over skipped, and so is each axiom it checks; the inserts' on their store's. The
/// engine's own work below each quad counts on the same poller ([`CountedSnapshot`]); a scan
/// it stopped ends, which the scan's caller reads as stopped ([`Poller::ended`]).
impl VisitsBase for EngineBase<'_> {
    fn visit(&self, bound: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        let poll = visit.poller();
        let reads = |n| poll.visit(n);
        for quad in self.snapshot.quads(
            ReadModel::Materialised,
            &pattern(bound, GraphSelector::Any),
            &reads,
        ) {
            let Ok(quad) = quad else {
                return;
            };
            let going = match self.deletes.contains(&quad) {
                true => visit.skip(),
                false => visit.fact(triple(quad)),
            };
            if !going {
                return;
            }
        }
        self.inserts.visit(bound, visit);
    }

    /// The engine's count's work counted on `probe` as it goes (review r1 F2: under compact
    /// equality it scans the stored matches); [`Stopped`] where the count stopped.
    fn size(&self, bound: [Option<u64>; 3], probe: Probe<'_, '_>) -> Result<usize, Stopped> {
        let committed = self
            .snapshot
            .count(
                ReadModel::Materialised,
                &pattern(bound, GraphSelector::Any),
                &|n| probe.visited(n),
            )?
            .at_least();
        // An expanded count past `u64` reads as its maximum (`Snapshot::count_in`): the sum
        // saturates too, never wrapping to a smaller estimate.
        Ok(usize::try_from(committed)
            .unwrap_or(usize::MAX)
            .saturating_add(self.inserts.size(bound, probe)?))
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.inserts.holds(fact, probe)?
            || self.committed(ReadModel::Materialised, fact, probe)?)
    }

    fn asserts(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.inserts.holds(fact, probe)?
            || self.axioms.binary_search(&fact).is_ok()
            || self.committed(ReadModel::Asserted, fact, probe)?)
    }

    fn visit_asserted(&self, bound: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        let poll = visit.poller();
        let reads = |n| poll.visit(n);
        for quad in self.snapshot.quads(
            ReadModel::Asserted,
            &pattern(bound, GraphSelector::Any),
            &reads,
        ) {
            let Ok(quad) = quad else {
                return;
            };
            let going = match self.deletes.contains(&quad) {
                true => visit.skip(),
                false => visit.fact(triple(quad)),
            };
            if !going {
                return;
            }
        }
        self.inserts.visit(bound, visit);
        let matches = |fact: &Triple| {
            bound
                .iter()
                .zip(fact)
                .all(|(want, have)| want.is_none_or(|want| want == *have))
        };
        for axiom in self.axioms.iter() {
            let going = match matches(axiom) {
                true => visit.fact(*axiom),
                false => visit.skip(),
            };
            if !going {
                return;
            }
        }
    }
}

/// What [`maintain`] did to a transaction's inferred stack.
#[derive(Debug, Default)]
pub struct Maintenance {
    /// The consistency violations the change introduced, charged to its budget.
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
    maintain_within(program, ground, tx, stop, &crate::violations::unlimited())
}

/// [`maintain`], the consistency violations (the rules' and the datatype checks', and a
/// rebuild's) charged to `memory`: the operation's own part, whose refusal ends their list
/// and keeps the change inconsistent ([`Violations`]).
pub fn maintain_within(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: crate::eval::Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Maintenance, delta::Interrupted> {
    maintain_with(program, ground, tx, &OperationStop::new(stop), memory)
}

/// [`maintain_within`] as one operation under `operation`, whose memory limit and room its
/// caller chose ([`OperationStop::within`]): the equality copies are admitted by its room,
/// and once it fires, every part of the maintenance stops.
pub fn maintain_with(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    operation: &OperationStop<'_>,
    memory: &Arc<Budget>,
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
        Some(super::materialise_with(
            program,
            &tx.pending_snapshot(),
            operation,
            memory,
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
    // expanded ones (Sol r1 F1). A fact no store holds (a literal subject) is new to the
    // state where the axioms' own closure concludes none, as the state before held only
    // that closure: such facts of the load's closure are reported with its others. Where
    // the axioms conclude one, whether the change derives it anew can't be told from a
    // state that can't hold it, so the delta executor, which follows the derivations,
    // takes the change; either is decided before any run (Sol r2 F1, r3 F1; review r1 F1).
    let first_load = !revived
        && !program.stores_representatives()
        && program
            .plan
            .is_none_or(|plan| plan == delta::policy::Plan::Rebuild)
        && tx.base().len_in(ReadModel::Asserted) == 0
        && tx.deleted().next().is_none()
        && tx.inserted().next().is_some()
        && program.axioms_conclude_only_storable(tx.base(), operation, memory)?;
    let mut unstorable: Vec<Triple> = Vec::new();
    if first_load {
        rebuilt = Some(super::materialise::materialise_with_unstorable(
            program,
            &tx.pending_snapshot(),
            operation,
            memory,
            &mut unstorable,
        )?);
    }
    let update = {
        let base = EngineBase::new(tx, &program.axioms);
        // The change's own probes of the base (a deleted fact still asserted elsewhere, a
        // derived one known before) count on the commit's stop, as the update's do.
        let stop: crate::eval::Stop<'_> = &|| operation.fired();
        let prep = Poller::new(stop).ok_or_else(|| operation.interrupted())?;
        // Facts new to the state: in no graph and not inferred before the transaction. Each
        // lookup in the state before counts on `prep` (review r1 F5); the first that finds
        // `prep` stopped ends the maintenance there, its partial lists dropped (fix check
        // r1, R2-F3: a stopped lookup read as "not known" kept every fact after it).
        let interrupted = |_: Stopped| operation.interrupted();
        let mut inserted: Vec<Triple> = Vec::new();
        for fact in tx.inserted().map(triple) {
            if !base.known_before(fact, &prep).map_err(interrupted)? {
                inserted.push(fact);
            }
        }
        if !(inserted.is_empty() && tx.deleted().next().is_none()) {
            for &axiom in &program.axioms {
                if base.known_before(axiom, &prep).map_err(interrupted)? {
                    continue;
                }
                if !base.inserts.contains(axiom, &prep).map_err(interrupted)? {
                    missing_axioms.push(axiom);
                }
            }
            inserted.extend(missing_axioms.iter().copied());
        }
        if let Some(closure) = rebuilt.as_ref().filter(|_| !first_load) {
            for fact in closure
                .inferred
                .iter()
                .map(|t| triple(t.in_default_graph()))
            {
                if !base.known_before(fact, &prep).map_err(interrupted)? {
                    inserted.push(fact);
                }
            }
        }
        inserted.sort_unstable();
        inserted.dedup();
        // A triple deleted from one graph but still asserted in another stays a fact. The
        // first probe that finds `prep` stopped ends the maintenance (fix check r1, rule 2).
        let mut deleted: Vec<Triple> = Vec::new();
        for fact in tx.deleted().map(triple) {
            let asserted = base.is_asserted(fact, &prep);
            if !asserted.map_err(|Stopped| delta::Interrupted::default())? {
                deleted.push(fact);
            }
        }
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
            delta::Update::default()
        } else if let Some(closure) = rebuilt.as_mut().filter(|_| first_load) {
            // The batch's own report isn't the one given (it holds an axiom's old violations
            // too): let go of its charge before the introduced-only report takes the same
            // budget, which then holds what the delta executor's single report would.
            drop(std::mem::take(&mut closure.violations));
            // The new inferred facts, as the delta executor reports them (its `insert`): the
            // state tells which storable ones it held before, and none it can't hold (above).
            let inserted_set: HashSet<Triple> = inserted.iter().copied().collect();
            let stopped = |Stopped| delta::Interrupted::default();
            let mut derived: Vec<Triple> = Vec::new();
            for f in closure
                .inferred
                .iter()
                .map(|t| triple(t.in_default_graph()))
            {
                if base.is_asserted(f, &prep).map_err(stopped)? {
                    continue;
                }
                if !(base.contains(f, &prep).map_err(stopped)? && !inserted_set.contains(&f)) {
                    derived.push(f);
                }
            }
            derived.append(&mut unstorable);
            delta::Update {
                violations: delta::introduced_violations(
                    &base, &inserted, &derived, rules, ground, operation, memory,
                )?,
                insert: derived,
                policy: delta::policy::Counts {
                    plan: delta::policy::Plan::Rebuild,
                    ..Default::default()
                },
                ..Default::default()
            }
        } else {
            delta::update_in(
                &base, &inserted, &deleted, rules, ground, operation, policy, memory,
            )?
        }
    };
    let mut update = update;
    // A list that already stopped at its budget makes the change inconsistent: the
    // datatype checks would add nothing to that.
    if update.violations.complete() {
        let datatypes = super::datatypes::violations(
            &update.insert,
            program.rdf_type,
            program.same_as,
            &|id| tx.decode(TermId::from_raw(id)),
            memory,
        );
        update.violations.append(datatypes);
    }
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
            None => super::materialise_with(program, &tx.pending_snapshot(), operation, memory)?,
        };
        // An interruption drops the transaction, these changes with it.
        if operation.fired() {
            return Err(operation.interrupted());
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
        if operation.fired() {
            return Err(operation.interrupted());
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

    /// REAS-2 (master, 8 Nov 05:29: a counted test per leaf that filters): an engine base's
    /// scan counts the quads it passes over (deleted by the change), not only those it
    /// delivers. Over three poll intervals of deleted quads, a poller whose stop has fired
    /// polls within the scan and is stopped by it; uncounted, it would never poll.
    #[test]
    fn an_engine_base_scan_counts_the_deleted_quads_it_passes_over() {
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
        let (p, o) = (iri("http://example.com/p"), iri("http://example.com/o"));
        let facts: Vec<Triple> = (0..n)
            .map(|i| [iri(&format!("http://example.com/s{i}")), p, o])
            .collect();
        for &fact in &facts {
            tx.insert_encoded(encode(fact).in_default_graph());
        }
        tx.commit().unwrap();
        let mut tx = engine.transaction();
        for &fact in &facts {
            tx.remove_encoded(encode(fact).in_default_graph());
        }
        let base = EngineBase::new(&tx, &[]);
        let fired = std::sync::atomic::AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        let mut delivered = 0;
        let scanned = base.scan([None, Some(p), None], &poll, &mut |_| delivered += 1);
        assert!(scanned.is_err(), "a scan its poller stopped says so");
        assert_eq!(delivered, 0, "every quad the scan meets is deleted");
        assert!(
            poll.stopped(),
            "{n} quads passed over without the poller polling"
        );
    }

    /// The same for a membership probe (sol-loop r1 S2): whether a fact asserted in three
    /// poll intervals of graphs, every copy deleted by the change, is still asserted reads
    /// each copy; a poller whose stop has fired polls within that one probe.
    #[test]
    fn an_engine_base_probe_counts_the_deleted_copies_it_passes_over() {
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into());
        let [s, p, o] = ["s", "p", "o"].map(|t| iri(&format!("http://example.com/{t}")));
        let graphs: Vec<_> = (0..n)
            .map(|g| iri(&format!("http://example.com/g{g}")))
            .collect();
        let copies: Vec<EncodedQuad> = graphs
            .iter()
            .map(|&graph| EncodedQuad::new(s, p, o, graph))
            .collect();
        for &copy in &copies {
            tx.insert_encoded(copy);
        }
        tx.commit().unwrap();
        let mut tx = engine.transaction();
        for &copy in &copies {
            tx.remove_encoded(copy);
        }
        let base = EngineBase::new(&tx, &[]);
        let fired = std::sync::atomic::AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        let asserted = base.is_asserted([s.raw(), p.raw(), o.raw()], &poll);
        // Stopped while it reads the deleted copies, the probe says so (fix check r1, rule 2:
        // once "not asserted").
        assert_eq!(asserted, Err(Stopped), "every copy is deleted");
        assert!(
            poll.stopped(),
            "{n} deleted copies read without the poller polling"
        );
    }

    /// The most work a read may do in the engine between two asks of its stop: `POLL_UNITS`
    /// counted units plus one run of at most `POLL_UNITS` in progress (REAS-2's invariant;
    /// REAS-3 design note, "Red witnesses first").
    #[cfg(debug_assertions)]
    const LARGEST_GAP: u64 = 2 * nrese_exec::POLL_UNITS as u64;

    /// REAS-3, group E (sweep SW013). Spec (design note's invariant: a unit is each copy an
    /// equality read makes): an engine base's scan under expanded equality makes a stored
    /// quad's copies within its poll interval. One stored quad whose subject and object each
    /// have 128 identities stands for 128 × 128 copies; between two asks of the scan's stop
    /// (and after the last one) the engine makes at most `LARGEST_GAP` of them. The engine
    /// builds a quad's whole product before it hands on the first copy, unseen by the stop.
    #[test]
    #[cfg(debug_assertions)]
    fn an_engine_base_scan_makes_equality_copies_within_its_poll_interval() {
        use crate::witness::EngineGaps;
        use nrese_engine::witness::Site;
        let k = 128;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
        let same_as = iri("http://www.w3.org/2002/07/owl#sameAs");
        let p = iri("http://example.com/p");
        let s: Vec<u64> = (0..k)
            .map(|i| iri(&format!("http://example.com/s{i}")))
            .collect();
        let o: Vec<u64> = (0..k)
            .map(|i| iri(&format!("http://example.com/o{i}")))
            .collect();
        tx.insert_encoded(encode([s[0], p, o[0]]).in_default_graph());
        for i in 1..k {
            tx.insert_encoded(encode([s[0], same_as, s[i]]).in_default_graph());
            tx.insert_encoded(encode([o[0], same_as, o[i]]).in_default_graph());
        }
        tx.commit().unwrap();
        engine.set_equality(Some(TermId::from_raw(same_as)));
        let tx = engine.transaction();
        let base = EngineBase::new(&tx, &[]);
        let gaps = EngineGaps::new(Site::ExpandCopies);
        let stop = || gaps.ask();
        let poll = Poller::new(&stop).expect("never stops");
        let mut delivered = 0;
        base.scan([None, Some(p), None], &poll, &mut |_| delivered += 1)
            .expect("never stopped");
        assert_eq!(
            delivered,
            k * k,
            "each identity pair is a copy (equality expanded)"
        );
        let largest = gaps.largest();
        assert!(
            largest <= LARGEST_GAP,
            "{largest} equality copies made between two asks of the stop: at most {LARGEST_GAP}"
        );
    }

    /// REAS-3, group E (sweep SW012). Spec (design note's invariant: a unit is each `sameAs`
    /// statement an equality class build reads): an engine base's first read under expanded
    /// equality builds the version's classes within its poll interval. Three poll intervals
    /// of `sameAs` statements (classes of two), committed, then one scan: between two asks of
    /// its stop (and after the last one) the engine takes at most `LARGEST_GAP` merge steps.
    /// A cold build scans every `sameAs` statement before the read's first step, unseen by the
    /// scan's count.
    #[test]
    #[cfg(debug_assertions)]
    fn an_engine_base_scan_builds_equality_classes_within_its_poll_interval() {
        use crate::witness::EngineGaps;
        use nrese_engine::witness::Site;
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
        let same_as = iri("http://www.w3.org/2002/07/owl#sameAs");
        let [p, o] = ["p", "o"].map(|t| iri(&format!("http://example.com/{t}")));
        let pairs: Vec<(u64, u64)> = (0..n)
            .map(|i| {
                (
                    iri(&format!("http://example.com/s{i}")),
                    iri(&format!("http://example.com/t{i}")),
                )
            })
            .collect();
        tx.insert_encoded(encode([pairs[0].0, p, o]).in_default_graph());
        for &(s, t) in &pairs {
            tx.insert_encoded(encode([s, same_as, t]).in_default_graph());
        }
        tx.commit().unwrap();
        engine.set_equality(Some(TermId::from_raw(same_as)));
        let tx = engine.transaction();
        let base = EngineBase::new(&tx, &[]);
        let gaps = EngineGaps::new(Site::ScanSteps);
        let stop = || gaps.ask();
        let poll = Poller::new(&stop).expect("never stops");
        let mut delivered = 0;
        base.scan([None, Some(p), None], &poll, &mut |_| delivered += 1)
            .expect("never stopped");
        assert_eq!(
            delivered, 2,
            "the statement and its copy for the subject's other identity"
        );
        let largest = gaps.largest();
        assert!(
            largest <= LARGEST_GAP,
            "{largest} merge steps between two asks of the scan's stop: at most {LARGEST_GAP}"
        );
    }

    /// REAS-3, group A (sweep SW017). Spec (design note's invariant: a unit is each stored
    /// position a read passes, deleted ones too): an engine base's membership probe passes the
    /// committed deletions of a fact's graph copies within its poll interval. A fact asserted in
    /// three poll intervals of graphs plus one, all but the last copy deleted by a later
    /// commit that stays its own run (a policy that merges no window, as REAS-2's T2: without
    /// a worker a commit compacts in place): the probe finds the last copy, and between two
    /// asks of its stop (and after the last one) the scan takes at most `LARGEST_GAP` merge
    /// steps. The engine passes the deleted copies inside one `next`, unseen by the probe's
    /// count.
    #[test]
    #[cfg(debug_assertions)]
    fn an_engine_base_probe_passes_committed_deletions_within_its_poll_interval() {
        use crate::witness::EngineGaps;
        use nrese_engine::witness::Site;
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            compaction: nrese_engine::CompactionPolicy {
                fanout: 0,
                ..nrese_engine::CompactionPolicy::default()
            },
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into());
        let [s, p, o] = ["s", "p", "o"].map(|t| iri(&format!("http://example.com/{t}")));
        let copies: Vec<EncodedQuad> = (0..=n)
            .map(|g| EncodedQuad::new(s, p, o, iri(&format!("http://example.com/g{g}"))))
            .collect();
        for &copy in &copies {
            tx.insert_encoded(copy);
        }
        tx.commit().unwrap();
        let mut tx = engine.transaction();
        for &copy in &copies[..n] {
            tx.remove_encoded(copy);
        }
        tx.commit().unwrap();
        let tx = engine.transaction();
        let base = EngineBase::new(&tx, &[]);
        let gaps = EngineGaps::new(Site::ScanSteps);
        let stop = || gaps.ask();
        let poll = Poller::new(&stop).expect("never stops");
        let held = base.contains([s.raw(), p.raw(), o.raw()], &poll);
        assert_eq!(held, Ok(true), "the last copy is still committed");
        let largest = gaps.largest();
        assert!(
            largest <= LARGEST_GAP,
            "{largest} merge steps between two asks of the probe's stop: at most {LARGEST_GAP}"
        );
    }

    /// REAS-3, group E (sweep SW006, SW007, SW009). Spec (design note's invariant: a unit is
    /// each run a read searches or probes): an engine base's membership probe searches a
    /// stack's runs within its poll interval. While a background merge holds the compaction
    /// claim, each commit's own compaction is deferred and appends a run (engine/mod.rs, the
    /// claim in `compact`), so the runs a read meets grow with the commits made meanwhile; a
    /// policy that merges no window stands for that here. Three poll intervals of commits,
    /// one statement each: between two asks of the probe's stop (and after the last one) the
    /// read searches at most `LARGEST_GAP` runs. A scan's construction searches every run
    /// before its first step, unseen by the probe's count.
    #[test]
    #[cfg(debug_assertions)]
    fn an_engine_base_probe_searches_the_runs_within_its_poll_interval() {
        use crate::witness::EngineGaps;
        use nrese_engine::witness::Site;
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            compaction: nrese_engine::CompactionPolicy {
                fanout: 0,
                ..nrese_engine::CompactionPolicy::default()
            },
            ..EngineConfig::default()
        })
        .unwrap();
        let tx = engine.transaction();
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into());
        let [p, o] = ["p", "o"].map(|t| iri(&format!("http://example.com/{t}")));
        let subjects: Vec<TermId> = (0..n)
            .map(|i| iri(&format!("http://example.com/s{i}")))
            .collect();
        tx.commit().unwrap();
        for &subject in &subjects {
            let mut tx = engine.transaction();
            tx.insert_encoded(EncodedQuad::new(subject, p, o, TermId::DEFAULT_GRAPH));
            tx.commit().unwrap();
        }
        let tx = engine.transaction();
        let base = EngineBase::new(&tx, &[]);
        let gaps = EngineGaps::new(Site::RunProbes);
        let stop = || gaps.ask();
        let poll = Poller::new(&stop).expect("never stops");
        let held = base.contains([subjects[0].raw(), p.raw(), o.raw()], &poll);
        assert_eq!(held, Ok(true), "the first commit's statement is committed");
        let largest = gaps.largest();
        assert!(
            largest <= LARGEST_GAP,
            "{largest} runs searched between two asks of the probe's stop: at most {LARGEST_GAP}"
        );
    }

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
                        assert!(done.violations.consistent(), "no inconsistency is possible");
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

    /// A list cell equated with `rdf:nil` keeps its members. Under the replacement rules,
    /// every identity as DRed maintains it, `ex:C owl:oneOf ex:l` with `ex:l` the list
    /// `(ex:a)` makes `ex:a` a C, and with C below owl:Nothing the state is inconsistent. A
    /// materialisation by representatives reads every list through the class of `rdf:nil` as
    /// empty, so it found the state consistent. It reports what the replacement rules
    /// report: maintenance equals full rematerialisation (reasoning-semantics.md,
    /// "Maintenance"; Sol's review of DMW-AB-mem, r2 S2).
    #[test]
    fn a_list_cell_equated_with_nil_keeps_its_members() {
        for representatives in [false, true] {
            let engine = Engine::new(EngineConfig {
                background_maintenance: false,
                ..EngineConfig::default()
            })
            .unwrap();
            let mut tx = engine.transaction();
            let program =
                Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t))
                    .by_representatives(representatives);
            let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
            let rdf = |l: &str| iri(&format!("http://www.w3.org/1999/02/22-rdf-syntax-ns#{l}"));
            let (first, rest, nil) = (rdf("first"), rdf("rest"), rdf("nil"));
            let owl = |l: &str| iri(&format!("http://www.w3.org/2002/07/owl#{l}"));
            let (one_of, nothing, same_as) = (owl("oneOf"), owl("Nothing"), owl("sameAs"));
            let sub_class_of = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
            let [c, l, a] = ["C", "l", "a"].map(|n| iri(&format!("http://example.com/{n}")));
            for fact in [
                [c, one_of, l],
                [c, sub_class_of, nothing],
                [l, first, a],
                [l, rest, nil],
                [l, same_as, nil],
            ] {
                tx.insert_encoded(encode(fact).in_default_graph());
            }
            let closure = super::super::materialise(&program, &tx.pending_snapshot());
            assert!(
                closure
                    .violations
                    .iter()
                    .any(|(rule, _)| rule == "cls-nothing2"),
                "by representatives {representatives}: {:?}",
                closure.violations
            );
        }
    }

    /// A first load is materialised once, not run through the delta executor: its stack
    /// is the recomputed closure, an inconsistent one is still reported, and the next
    /// commit, onto a loaded state, takes the delta.
    #[test]
    fn an_engine_base_estimate_counts_the_engine_s_count() {
        // REAS-2 review r1 F2: maintenance's estimates (the policy's, job planning's) ask the
        // engine for a count, which under compact equality scans the stored matches and
        // weighs each by its copies. A representative with 50 identities and 8 × `POLL_UNITS`
        // statements `rep p o`: the estimate of `? p ?` asks its stop at least once every
        // `POLL_UNITS` plus one run (the longest run counted at once) of the stored matches
        // it scans. Uncounted, the whole count was one unit.
        let interval = nrese_exec::POLL_UNITS as usize;
        let n = 8 * interval;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let id =
            |tx: &mut Transaction<'_>, s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into());
        let same_as = id(&mut tx, "http://www.w3.org/2002/07/owl#sameAs");
        let (rep, p) = (
            id(&mut tx, "http://example.com/rep"),
            id(&mut tx, "http://example.com/p"),
        );
        for i in 1..50 {
            let alias = id(&mut tx, &format!("http://example.com/alias{i}"));
            tx.insert_inferred(EncodedTriple {
                subject: alias,
                predicate: same_as,
                object: rep,
            });
        }
        for i in 0..n {
            let o = id(&mut tx, &format!("http://example.com/o{i}"));
            tx.insert_encoded(encode([rep.raw(), p.raw(), o.raw()]).in_default_graph());
        }
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        let tx = engine.transaction();
        let base = EngineBase::new(&tx, &[]);
        let asks = AtomicUsize::new(0);
        let stop = || {
            asks.fetch_add(1, Ordering::Relaxed);
            false
        };
        let poll = Poller::new(&stop).expect("never stopped");
        let estimate = base
            .estimate([None, Some(p.raw()), None], &poll)
            .expect("never stopped");
        assert!(
            estimate >= n,
            "{estimate}: an upper bound on the {n} statements and their copies"
        );
        let run = interval;
        let asks = asks.load(Ordering::Relaxed);
        assert!(
            asks >= n / (interval + run),
            "{asks} asks of the stop for a count over {n} stored matches: more than {interval} \
             plus one run of {run} between two of them"
        );
    }

    #[test]
    fn known_before_probes_ask_their_stop_within_a_run() {
        // REAS-2 review r1 F5: a commit's new assertions are each looked up in the state
        // before it (`EngineBase::known_before`), each lookup counted on the preparation's
        // poller. A loaded state that infers `i a C0` for 16 × `POLL_UNITS` individuals (each
        // asserted `i a C1`, `C1 ⊑ C0`), then a commit asserting every `i a C0`: all known
        // before, so the delta has nothing left to do, and the lookups ask the stop at least
        // once every `POLL_UNITS` plus one run (the longest run counted at once). Uncounted,
        // their number changed nothing.
        let interval = nrese_exec::POLL_UNITS as usize;
        let n = 16 * interval;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let program = Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t));
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
        let rdf_type = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let subclass = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
        let (c0, c1) = (iri("http://example.com/C0"), iri("http://example.com/C1"));
        let individuals: Vec<u64> = (0..n)
            .map(|i| iri(&format!("http://example.com/i{i}")))
            .collect();
        tx.insert_encoded(encode([c1, subclass, c0]).in_default_graph());
        for &i in &individuals {
            tx.insert_encoded(encode([i, rdf_type, c1]).in_default_graph());
        }
        let loaded = maintain(&program, None, &mut tx, crate::eval::NEVER).unwrap();
        tx.commit().unwrap();
        let ground = loaded.program.expect("the ground program after the load");
        let mut tx = engine.transaction();
        for &i in &individuals {
            tx.insert_encoded(encode([i, rdf_type, c0]).in_default_graph());
        }
        let asks = AtomicUsize::new(0);
        let stop = || {
            asks.fetch_add(1, Ordering::Relaxed);
            false
        };
        maintain(&program, Some(&ground), &mut tx, &stop).expect("never stopped");
        let run = interval;
        let asks = asks.load(Ordering::Relaxed);
        assert!(
            asks >= n / (interval + run),
            "{asks} asks of the stop for {n} known-before lookups: more than {interval} plus one \
             run of {run} between two of them"
        );
    }

    /// REAS-2 fix check r1 (R2-F3, F5). Spec (design note, rule 2: a stop is an error the
    /// consumer has to handle, never an answer): a commit's maintenance ends at its stop. A
    /// loaded state with its ground program, then a commit asserting 16 × `POLL_UNITS`
    /// memberships new to the state, the stop firing at its first ask after the first
    /// known-before lookup: the commit is interrupted, and no more than one lookup is made
    /// after the stop fired. Read as "not known", a stopped lookup kept every fact after it,
    /// and the filter looked each one up.
    #[test]
    fn a_commit_s_known_before_lookups_end_at_its_stop() {
        use crate::witness::{self, Site};
        let n = 16 * nrese_exec::POLL_UNITS as usize;
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let program = Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| tx.intern(t));
        let iri = |s: &str| tx.intern(NamedNodeRef::new_unchecked(s).into()).raw();
        let rdf_type = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let subclass = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
        let (c0, c1) = (iri("http://example.com/C0"), iri("http://example.com/C1"));
        let individuals: Vec<u64> = (0..n)
            .map(|i| iri(&format!("http://example.com/i{i}")))
            .collect();
        tx.insert_encoded(encode([c1, subclass, c0]).in_default_graph());
        let loaded = maintain(&program, None, &mut tx, crate::eval::NEVER).unwrap();
        tx.commit().unwrap();
        let ground = loaded.program.expect("the ground program after the load");
        let mut tx = engine.transaction();
        for &i in &individuals {
            tx.insert_encoded(encode([i, rdf_type, c1]).in_default_graph());
        }
        let first = witness::FirstWork::new(Site::KnownBefore);
        let stop = || first.ask();
        let maintained = maintain(&program, Some(&ground), &mut tx, &stop);
        let after = first.after();
        assert!(
            after <= 1,
            "{after} known-before lookups after the stop fired, of {n}: at most the one that \
             saw it"
        );
        assert!(
            maintained.is_err(),
            "a stopped commit's maintenance is interrupted"
        );
    }

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
                done.violations.consistent(),
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
