//! The quad index: an immutable [`IndexVersion`] is a stack of sorted runs (oldest first).
//!
//! - **Reads** ([`IndexVersion::scan`]) pick one permutation per pattern
//!   ([`AccessPlan`]), binary-search the key range in every run and k-way merge the ranges
//!   with sign-sum visibility ([`merge`]). O(r log n + k·r) for r runs and k results.
//! - **Writes** never modify a version: [`IndexVersion::with_run`] returns a new version
//!   that shares all existing runs. Old versions stay valid for as long as a snapshot holds
//!   them, which is the whole MVCC mechanism.
//! - **Compaction** replaces a window of runs by their merge
//!   ([`IndexVersion::with_compacted`]); the window choice is in [`compaction`].
//! - **Layouts** ([`Layout`]): the asserted stack holds arbitrary quads in the six pattern
//!   permutations plus GPSO (subject-sorted scans per predicate, for star joins); the
//!   inferred stack holds default-graph quads only and needs four.
//! - **Counts** ([`IndexVersion::count_plan`]) are exact without scanning when the runs hold
//!   no tombstones in range.
//!
//! Ownership: this module owns the physical layout and visibility rule. It knows nothing
//! about terms, transactions or durability.

pub(crate) mod compaction;
#[cfg(test)]
mod compression_study;
pub(crate) mod cursor;
mod derive;
pub(crate) mod keys;
mod merge;
pub(crate) mod run;

use std::ops::Range;
use std::sync::Arc;

use rayon::prelude::*;

use std::borrow::Cow;

use keys::PackedKeys;
use merge::SignedMerge;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
pub(crate) use run::Forms;
use run::{PermutationRun, Run};

use crate::quad::{AccessPlan, EncodedQuad, Permutation, QuadPattern};
use crate::term::TermId;
use crate::visit::{Account, Free, Ledger};

pub use compaction::CompactionPolicy;
pub use keys::{IndexEncoding, set_index_encoding};

/// Which quads a stack of runs may hold, and therefore which permutations it maintains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Layout {
    /// Any quads, in the six pattern permutations plus GPSO.
    #[default]
    Quads,
    /// Default-graph quads only (the inferred stack, and the asserted one until it holds a
    /// quad in a named graph), in SPOG, POSG, OSPG and PSOG. A graph-first order over one
    /// constant graph sorts exactly like the matching graph-last order, so the graph-first
    /// permutations would be redundant copies.
    DefaultGraph,
}

impl Layout {
    /// The layout that maintains exactly `permutations`, if one does.
    pub(crate) fn of(permutations: &[Permutation]) -> Option<Self> {
        [Self::Quads, Self::DefaultGraph]
            .into_iter()
            .find(|layout| {
                let wanted = layout.permutations();
                wanted.len() == permutations.len()
                    && wanted.iter().all(|p| permutations.contains(p))
            })
    }

    /// The smaller layout that can hold `quads`: the default-graph one unless one of them is
    /// in a named graph.
    pub(crate) fn holding(quads: &[EncodedQuad]) -> Self {
        match quads.iter().any(|quad| !quad.graph.is_default_graph()) {
            true => Self::Quads,
            false => Self::DefaultGraph,
        }
    }

    pub(crate) const fn permutations(self) -> &'static [Permutation] {
        match self {
            Self::Quads => &[
                Permutation::Spog,
                Permutation::Posg,
                Permutation::Ospg,
                Permutation::Gspo,
                Permutation::Gpos,
                Permutation::Gosp,
                Permutation::Gpso,
            ],
            Self::DefaultGraph => &[
                Permutation::Spog,
                Permutation::Posg,
                Permutation::Ospg,
                Permutation::Psog,
            ],
        }
    }

    /// True if scans in `permutation` can be answered: the layout maintains it, or (for the
    /// single-graph layout) its graph-last equivalent.
    pub(crate) fn supports(self, permutation: Permutation) -> bool {
        let maintained = |p: Permutation| self.permutations().contains(&p);
        maintained(permutation)
            || (self == Self::DefaultGraph && permutation.graph_last().is_some_and(maintained))
    }

    /// The plan that answers `plan` over this layout, or `None` if no quad the layout can
    /// hold matches it. The plan's permutation must be [`supported`](Self::supports).
    pub(crate) fn adapt(self, plan: &AccessPlan) -> Option<AccessPlan> {
        if self == Self::Quads {
            return Some(*plan);
        }
        if plan.exclude_default_graph {
            return None;
        }
        let Some(permutation) = plan.permutation.graph_last() else {
            return Some(*plan); // graph-last plans never bind the graph
        };
        let default = TermId::DEFAULT_GRAPH.raw();
        // Only the default graph can match, so the graph range must include it. Rotate the
        // graph from the front to the back.
        if !(plan.low[0] <= default && default <= plan.high[0]) {
            return None; // an exact named graph, or "all named graphs"
        }
        if plan.low[0] == plan.high[0] {
            return Some(AccessPlan {
                permutation,
                low: [plan.low[1], plan.low[2], plan.low[3], default],
                high: [plan.high[1], plan.high[2], plan.high[3], default],
                exclude_default_graph: false,
            });
        }
        // An unbound graph (executors' sorted scans): the prefix stops at the graph, so
        // nothing after it is bound, and every quad of this layout matches.
        Some(AccessPlan {
            permutation,
            low: [0, 0, 0, default],
            high: [u64::MAX, u64::MAX, u64::MAX, default],
            exclude_default_graph: false,
        })
    }
}

/// Keys per part of a group walk over a large range, walked in parallel: a multiple of
/// the block size.
pub(crate) const GROUP_PART: usize = 1 << 16;

/// The groups each parallel part of a group walk writes into its block of the run's one
/// slab, charged once before the parts start: a part that finds no more makes no budget
/// call, and only one past its block grows a vector of its own, charged as it grows. On
/// DBpedia's q10 the parts' own vectors made about 1,416 of a walk's 1,450 budget calls:
/// about 1,178 for their growths, in parallel, and 238 for their releases, serially in the
/// merge; by those counts at most 11 of its 119 parts hold more than 64 groups (12 Nov).
/// The slab is 1 KiB a part.
const PART_BLOCK: usize = 64;

/// The most bytes a group walk over `keys` keys charges, into an output that starts without
/// capacity, its runs walked in `parts` parallel parts in all (or serially), bounded by the
/// keys alone: a group has at least one. Let `S` be the keys walked before a run of `k` keys
/// in `p` parts, and `C` [`PART_BLOCK`]. The output then holds at most `2S + 4` groups; the
/// run's slab `pC` beside it; a part of `g > C` groups starts its own vector at `2C < 2g`
/// and doubles from there, so the parts' own vectors grow to at most `2k`, `3k` while they
/// reallocate. While the output reallocates, its old capacity is under `S + k`, its new one
/// at most `2(S + k) + 4`, the slab and the finished parts at most `pC + 2k`. That is
/// `3S + 5k + pC + 4` groups, within `5·keys + (4 + C)·parts + 4`, as each run's slab and
/// parts are dropped before the next run is walked (its `4·parts`, the parts' first vectors
/// before the slab, is now a margin).
pub(crate) fn group_walk_peak(keys: usize, parts: usize) -> usize {
    keys.saturating_mul(5)
        .saturating_add(parts.saturating_mul(4 + PART_BLOCK))
        .saturating_add(4)
        .saturating_mul(size_of::<(u64, i64)>())
}

/// The groups up to which a walk whose large runs could go in parallel parts is walked
/// serially all the same: a search per group costs less than waking the parts. Compared with
/// [`IndexVersion::parted_groups`]' estimate.
pub(crate) const SERIAL_GROUPS: usize = 256;

/// Whether a group walk over `keys` keys, `parts` parallel parts in its large runs, runs in
/// parts: where it has any, `budget`'s room covers the whole walk's peak, and its large runs
/// hold more than [`SERIAL_GROUPS`] groups by the estimate `groups` gives (asked last, only
/// where the rest holds). One decision for the whole walk, before its output grows; serially
/// is the reference, so an estimate can cost a walk speed, never an answer.
pub(crate) fn walks_in_parts(
    budget: &Budget,
    keys: usize,
    parts: usize,
    groups: impl FnOnce() -> usize,
) -> bool {
    parts > 0 && budget.remaining() >= group_walk_peak(keys, parts) && groups() > SERIAL_GROUPS
}

/// [`IndexVersion::group_counts`]'s walk past its capacity, out of its hot loop: `group`
/// pushed after a growth charged first, or dropped where the walk was refused (that
/// refusal kept, the first one).
#[cold]
#[inline(never)]
fn grow_then_push(
    out: &mut ChargedVec<(u64, i64), &Budget>,
    group: (u64, i64),
    refused: &mut Option<BudgetExceeded>,
) {
    if refused.is_some() {
        return;
    }
    match out.reserve(1) {
        Ok(()) => {
            let pushed = out.push_within(group);
            debug_assert!(pushed.is_ok(), "room after a growth");
        }
        Err(error) => *refused = Some(error),
    }
}

/// A parallel part's `group` past its full `block` of the slab, out of the hot loop: the
/// part's own vector made with room for twice the block, charged exactly, the block's
/// groups copied into it and `group` pushed; once it has room, grown as
/// [`grow_then_push`] grows a vector.
#[cold]
#[inline(never)]
fn overflow_then_push(
    own: &mut ChargedVec<(u64, i64), &Budget>,
    block: &[(u64, i64)],
    group: (u64, i64),
    refused: &mut Option<BudgetExceeded>,
) {
    if own.capacity() > 0 || refused.is_some() {
        grow_then_push(own, group, refused);
        return;
    }
    let made = own
        .reserve_exact(2 * block.len())
        .and_then(|()| own.extend_from_slice(block))
        .and_then(|()| own.push(group));
    if let Err(error) = made {
        *refused = Some(error);
    }
}

/// A parallel part's groups, as the walk collects them: the first `len` of its block of the
/// slab, or its own vector once it passed the block. No larger than the part's vector the
/// walk collected before the slab, so the uncharged collection of the parts does not grow.
enum Part<'b> {
    Inline(usize),
    Owned(ChargedVec<(u64, i64), &'b Budget>),
}

const _: () = assert!(GROUP_PART.is_multiple_of(keys::BLOCK));
const _: () =
    assert!(size_of::<Part<'static>>() <= size_of::<ChargedVec<(u64, i64), &'static Budget>>());

#[derive(Debug, Clone, Default)]
pub(crate) struct IndexVersion {
    layout: Layout,
    runs: Arc<[Arc<Run>]>,
    len: u64,
}

impl IndexVersion {
    pub(crate) fn empty(layout: Layout) -> Self {
        Self {
            layout,
            ..Self::default()
        }
    }

    /// A version holding one base run built from packed permutations (a checkpoint): every
    /// permutation of a layout exactly once, all with the same number of keys. The layout
    /// is the one they make up.
    pub(crate) fn from_packed(packed: Vec<(Permutation, PackedKeys)>) -> Result<Self, String> {
        let permutations: Vec<Permutation> = packed.iter().map(|(p, _)| *p).collect();
        let layout = Layout::of(&permutations)
            .ok_or("the checkpoint doesn't hold every permutation of a layout once")?;
        let len = packed[0].1.len();
        if packed.iter().any(|(_, keys)| keys.len() != len) {
            return Err("checkpoint permutations differ in size".into());
        }
        let mut perms: [PermutationRun; Permutation::COUNT] = Default::default();
        for (permutation, keys) in packed {
            perms[permutation as usize] = PermutationRun {
                keys,
                tombstones: Box::default(),
            };
        }
        Ok(Self::empty(layout).with_run(Run::from_permutations(layout, perms)))
    }

    /// The visible keys of `permutation` as one packed array: the run's own array when the
    /// version is a single run without tombstones, else packed from the merged scan.
    pub(crate) fn packed(&self, permutation: Permutation) -> Cow<'_, PackedKeys> {
        if let [run] = &*self.runs {
            let perm = run.permutation(permutation);
            if perm.tombstones.is_empty() {
                return Cow::Borrowed(&perm.keys);
            }
        }
        let parts = self.runs.iter().map(|run| {
            let perm = run.permutation(permutation);
            (perm, 0, perm.keys.len())
        });
        Cow::Owned(PackedKeys::from_sorted_iter(
            SignedMerge::new(parts)
                .filter(|&(_, sign)| sign > 0)
                .map(|(key, _)| key),
        ))
    }

    /// A version holding exactly the (deduplicated) `quads` in one base run.
    pub(crate) fn from_quads(layout: Layout, quads: Vec<EncodedQuad>) -> Self {
        Self::empty(layout).with_run(Run::from_quads(layout, quads))
    }

    /// Number of visible quads. O(1).
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }

    /// The layout `self` takes with the delta `inserts` and `deletes`: the quad layout once a
    /// quad of them is in a named graph.
    fn layout_with(&self, inserts: &[EncodedQuad], deletes: &[EncodedQuad]) -> Layout {
        let named = |quads: &[EncodedQuad]| Layout::holding(quads) == Layout::Quads;
        match self.layout == Layout::DefaultGraph && (named(inserts) || named(deletes)) {
            true => Layout::Quads,
            false => self.layout,
        }
    }

    /// `self` with the exact delta `inserts` and `deletes` appended as a run. A default-graph
    /// stack that receives a quad in a named graph turns into a quad stack first, its runs
    /// converted for this version alone: never installed as their shared forms (which would
    /// then go uncounted). For stacks no reader shares a conversion of: an inferred stack
    /// (its statements are in the default graph), a test's. An asserted stack a pending
    /// version reads goes through [`Self::with_delta_in`], which shares and counts it (DUR-6).
    pub(crate) fn with_delta(&self, inserts: &[EncodedQuad], deletes: &[EncodedQuad]) -> Self {
        let base = match self.layout_with(inserts, deletes) == self.layout {
            true => self.clone(),
            false => self.with_quads_layout_alone(),
        };
        let run = Run::from_delta(base.layout, inserts, deletes);
        base.with_run(run)
    }

    /// The same quads in the quad layout, made for this version alone: its runs are its own,
    /// counted as the index's own runs (`Engine::stats`), never installed as shared forms.
    /// For a version that replaces its source for good (a recovery's replay, a bulk load's
    /// publication), where nothing else would share the form.
    pub(crate) fn with_quads_layout_alone(&self) -> Self {
        if self.layout == Layout::Quads {
            return self.clone();
        }
        Self {
            layout: Layout::Quads,
            runs: self
                .runs
                .iter()
                .map(|run| Arc::new(run.with_quads_layout()))
                .collect(),
            len: self.len,
        }
    }

    /// `self` with the exact delta `inserts` and `deletes` appended as a run. A default-graph
    /// stack that receives a quad in a named graph reads its runs in the quad layout, each
    /// run's form shared and counted in `forms` ([`Self::with_quads_layout`], DUR-6).
    pub(crate) fn with_delta_in(
        &self,
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
        forms: &Forms,
    ) -> Self {
        let base = match self.layout_with(inserts, deletes) == self.layout {
            true => self.clone(),
            false => self.with_quads_layout(forms),
        };
        let run = Run::from_delta(base.layout, inserts, deletes);
        base.with_run(run)
    }

    /// What [`Self::with_change_within`] needs charged before it is called for `inserts` and
    /// `deletes` (DUR-6 slice 2): below the parallel threshold, the bound of its run's build
    /// ([`Run::delta_bound`]), working memory included; nothing for an empty delta (nothing is
    /// built). None at or above the threshold: that build charges exactly as it goes, and no
    /// bound charged beside it covers its peak (review r1 on slice 2, DUR6B-R1-01).
    pub(crate) fn change_bound(
        &self,
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
    ) -> Option<usize> {
        match inserts.len() + deletes.len() {
            0 => Some(0),
            n if Run::charged_exactly(n) => None,
            n => Some(Run::delta_bound(self.layout_with(inserts, deletes), n)),
        }
    }

    /// [`Self::with_delta_in`] charged to `held`, a reservation of `budget` (DUR-6): the list
    /// of runs made once (with the base's runs in the quad layout, shared and counted in
    /// `forms`, where the delta needs it, not charged: they are the store's). Below the
    /// parallel threshold, `bounded`, the delta's run is built as [`Run::from_delta`] builds
    /// it, within the [`Self::change_bound`] the caller has charged to `held` first; not
    /// `bounded`, with [`Run::from_delta_within`], every array charged to `held` before it is
    /// made (the reference: refused only where exact charging is). At or above the threshold,
    /// with [`Run::from_delta_within_parallel`], its charge then taken into `held`. The new
    /// version and the bytes its run keeps of that bound (0 for a run charged exactly: `held`
    /// then holds exactly what it keeps), for the caller to release the rest of the bound
    /// once it is built; the list ([`Self::runs_bytes`] of one more) and the run's box are the
    /// caller's to charge first. A refusal leaves nothing made.
    pub(crate) fn with_change_within(
        &self,
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
        forms: &Forms,
        budget: &Arc<nrese_exec::Budget>,
        held: &mut Reservation,
        bounded: bool,
    ) -> Result<(Self, usize), BudgetExceeded> {
        let n = inserts.len() + deletes.len();
        if n == 0 {
            return Ok((self.clone(), 0));
        }
        let layout = self.layout_with(inserts, deletes);
        let (run, kept) = match Run::charged_exactly(n) {
            false if !bounded => (Run::from_delta_within(layout, inserts, deletes, held)?, 0),
            false => {
                let run = Run::from_delta(layout, inserts, deletes);
                let kept = run.memory_bytes() as usize;
                debug_assert!(
                    kept <= Run::delta_bound(layout, n),
                    "a run keeps within its bound"
                );
                (run, kept)
            }
            true => {
                let (run, exact) =
                    Run::from_delta_within_parallel(layout, inserts, deletes, budget)?;
                held.absorb(exact);
                (run, 0)
            }
        };
        let converting = layout != self.layout;
        let extra = (run.entries() > 0).then(|| Arc::new(run));
        let net = extra.as_ref().map_or(0, |run| run.net());
        let runs: Arc<[Arc<Run>]> = self
            .runs
            .iter()
            .map(|run| match converting {
                true => run.quads_layout_shared(forms),
                false => Arc::clone(run),
            })
            .chain(extra)
            .collect();
        let len = self
            .len
            .checked_add_signed(net)
            .expect("exact delta can't make the quad count negative");
        Ok((Self { layout, runs, len }, kept))
    }

    /// [`Self::with_delta`] for default-graph `inserts` (which fit either layout, so no stack
    /// is converted), its run built with [`Run::from_delta_within_parallel`] on `budget`: the
    /// new version and the reservation of what its run keeps (its list of runs is the
    /// caller's to charge: [`Self::runs_bytes`]).
    pub(crate) fn with_delta_within(
        &self,
        inserts: &[EncodedQuad],
        budget: &Arc<nrese_exec::Budget>,
    ) -> Result<(Self, nrese_exec::Reservation), nrese_exec::BudgetExceeded> {
        debug_assert!(inserts.iter().all(|q| q.graph == TermId::DEFAULT_GRAPH));
        Run::from_delta_within_parallel(self.layout, inserts, &[], budget)
            .map(|(run, held)| (self.with_run(run), held))
    }

    /// The same quads in the quad layout: each run's graph-first permutations are its
    /// graph-last ones with the graph moved to the front (all its quads are in the default
    /// graph, so both sort alike). O(n) for n entries, once per run while any holder keeps
    /// its form: each run's form is shared and counted in `forms` from when it is made until
    /// its last holder drops it ([`Run::quads_layout_shared`], DUR-6), so pending versions,
    /// and the commit that publishes one, share it.
    pub(crate) fn with_quads_layout(&self, forms: &Forms) -> Self {
        if self.layout == Layout::Quads {
            return self.clone();
        }
        Self {
            layout: Layout::Quads,
            runs: self
                .runs
                .iter()
                .map(|run| run.quads_layout_shared(forms))
                .collect(),
            len: self.len,
        }
    }

    pub(crate) fn runs(&self) -> &[Arc<Run>] {
        &self.runs
    }

    /// True if `quad` is visible. O(r log n).
    pub(crate) fn contains(&self, quad: &EncodedQuad) -> bool {
        #[cfg(any(test, all(feature = "witness", debug_assertions)))]
        crate::witness::add(crate::witness::Site::RunProbes, self.runs.len() as u64);
        self.runs.iter().map(|run| run.sign_of(quad)).sum::<i64>() > 0
    }

    /// [`Self::contains`], each run's probe and its search's comparisons counted on `visit`
    /// before they are made ([`crate::visit`]; REAS-3, sweep SW009; fix check r1 F1).
    pub(crate) fn contains_counted(
        &self,
        quad: &EncodedQuad,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        // Asked at least once: a stopped visitor gets `Err` however few the runs.
        if self.runs.is_empty() && crate::visit::stopped(visit) {
            return Err(Stopped);
        }
        self.contains_with(quad, &mut Ledger::new(visit))
    }

    /// [`Self::contains_counted`] on a caller's `account`: a probe inside a read that counts all
    /// its work on one account (an equality read's visibility probes, fix check r2 G1).
    pub(crate) fn contains_with(
        &self,
        quad: &EncodedQuad,
        account: &mut impl Account,
    ) -> Result<bool, Stopped> {
        let mut sign = 0;
        for run in self.runs.iter() {
            sign += run.sign_of_counted(quad, account).ok_or(Stopped)?;
            // Looked at once its search was counted, the run with it (`bound_counted`).
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::RunProbes, 1);
        }
        Ok(sign > 0)
    }

    /// [`Self::any_plan`], each run it looks at a unit on `visit`, each search's comparisons
    /// counted before it is made and, where it counts, the count's work
    /// ([`Self::count_plan_visiting`]) (REAS-3, sweep SW010; fix check r1 F1).
    pub(crate) fn any_plan_counted(
        &self,
        plan: &AccessPlan,
        visit: Visitor<'_>,
    ) -> Result<bool, Stopped> {
        // Asked at least once: a stopped visitor gets `Err` however little there is to read.
        let Some(adapted) = self.layout.adapt(plan).filter(|_| !self.runs.is_empty()) else {
            return match crate::visit::stopped(visit) {
                true => Err(Stopped),
                false => Ok(false),
            };
        };
        // Whether a run holds tombstones (then the exact count decides): each run looked at a
        // unit, counted a chunk at a time before it is looked at (REAS-3a sol-loop r1 S2).
        let mut has_tombstones = false;
        for runs in self.runs.chunks(nrese_exec::POLL_UNITS as usize) {
            if has_tombstones {
                break;
            }
            if !visit(runs.len() as u64) {
                return Err(Stopped);
            }
            has_tombstones = runs.iter().any(|run| {
                #[cfg(any(test, all(feature = "witness", debug_assertions)))]
                crate::witness::add(crate::witness::Site::RunProbes, 1);
                !run.permutation(adapted.permutation).tombstones.is_empty()
            });
        }
        if adapted.exclude_default_graph || has_tombstones {
            let count = self.count_plan_visiting(plan, visit);
            return count.map(|count| count > 0).ok_or(Stopped);
        }
        // Each run's bound search counted before it is made, its comparisons too (fix check
        // r1 F1, design attack r2 E2).
        let mut ledger = Ledger::new(visit);
        for run in self.runs.iter() {
            let keys = &run.permutation(adapted.permutation).keys;
            let start = keys
                .bound_counted(&adapted.low, &mut ledger)
                .ok_or(Stopped)?;
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::RunProbes, 1);
            if start < keys.len() && keys.get(start) <= adapted.high {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [`Self::scan_plan`] for a counted read. As the scan is made, each run's range search
    /// and the cursor it adds are counted on `visit` before they are done, the searches'
    /// comparisons too (REAS-3, sweep SW006; fix check r1 F1, design attack r2 E2). Its steps
    /// are counted by [`QuadScan::next_counted`] or [`CountedScan`] on the same ledger.
    pub(crate) fn scan_plan_counted(
        &self,
        plan: &AccessPlan,
        visit: Visitor<'_>,
    ) -> Result<QuadScan<'_>, Stopped> {
        let mut ledger = Ledger::new(visit);
        let mut scan = self.scan_plan_with(plan, &mut ledger)?;
        scan.ledger = (ledger.left, ledger.run);
        Ok(scan)
    }

    /// [`Self::scan_plan_counted`] on a caller's `account`, which then steps it
    /// ([`QuadScan::step_with`]): one account for a read that does other work between the
    /// scan's steps (an equality read's unions, copies and probes; fix check r2 G1). The plain
    /// scan where the account counts nothing.
    pub(crate) fn scan_plan_with<A: Account>(
        &self,
        plan: &AccessPlan,
        account: &mut A,
    ) -> Result<QuadScan<'_>, Stopped> {
        if !A::COUNTED {
            return Ok(self.scan_plan(plan));
        }
        let Some(adapted) = self.layout.adapt(plan) else {
            return Ok(QuadScan::new(
                SignedMerge::new(std::iter::empty()),
                plan.permutation,
                false,
            ));
        };
        // Each range handed to the merge as it is searched, with no list of them first (the
        // plain scan's shape: a list per scan cost the counted reads about 2.7 % on the support
        // update, p2-reas3-cnt4).
        let mut merge = SignedMerge::new(std::iter::empty());
        for perm_run in self.runs.iter() {
            let perm = perm_run.permutation(adapted.permutation);
            let (start, end) = perm
                .keys
                .range_counted(&adapted.low, &adapted.high, account)
                .ok_or(Stopped)?;
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::RunProbes, 1);
            if !merge.add((perm, start, end), account) {
                return Err(Stopped);
            }
        }
        Ok(QuadScan::new(
            merge,
            adapted.permutation,
            adapted.exclude_default_graph,
        ))
    }

    /// The most [`Self::scan_plan_with`] counts making a scan in `plan`'s permutation: each
    /// run's range search, and its cursor added to a merge whose every range is taken
    /// ([`merge::add_cost`]). From the runs' sizes and the permutation the layout reads alone,
    /// a run at a time: `None` once it is over `most`, the runs after that left unread. So it
    /// reads at most `most / 22 + 1` runs' metadata (a run costs at least 22: its two searches
    /// at one level, `levels()`'s least, and its cursor), and searches, decodes and allocates nothing. The
    /// plain scan's making ([`Self::scan_plan`]) does no more than this: the same searches,
    /// and a heap built at once compares no more than its pushes one by one.
    pub(crate) fn scan_cost(&self, plan: &AccessPlan, most: u64) -> Option<u64> {
        let Some(adapted) = self.layout.adapt(plan) else {
            return Some(0);
        };
        let mut cost = 0;
        for (added, run) in self.runs.iter().enumerate() {
            cost += run.permutation(adapted.permutation).keys.range_cost() + merge::add_cost(added);
            if cost > most {
                return None;
            }
        }
        Some(cost)
    }

    /// [`Self::scan`] for a counted read: made and stepped counted on `visit`
    /// ([`Self::scan_plan_counted`], [`QuadScan::next_counted`]), ending at once where `visit`
    /// had stopped (the stop is the visitor's to tell).
    pub(crate) fn scan_counted<'v>(
        &self,
        pattern: &QuadPattern,
        visit: Visitor<'v>,
    ) -> CountedScan<'_, 'v> {
        self.scan_plan_iter_counted(&AccessPlan::for_pattern(pattern), visit)
    }

    /// [`Self::scan_counted`] for a plan.
    pub(crate) fn scan_plan_iter_counted<'v>(
        &self,
        plan: &AccessPlan,
        visit: Visitor<'v>,
    ) -> CountedScan<'_, 'v> {
        let scan = self
            .scan_plan_counted(plan, visit)
            .unwrap_or_else(|Stopped| QuadScan::empty(plan.permutation));
        scan.counted(visit)
    }

    /// All visible quads matching `pattern`, in the order of the chosen permutation.
    pub(crate) fn scan(&self, pattern: &QuadPattern) -> QuadScan<'_> {
        self.scan_plan(&AccessPlan::for_pattern(pattern))
    }

    /// The smallest named graph with a visible quad whose id is greater than `after`
    /// (or the smallest overall). A GSPO seek, O(r log n) per call when the graph is not
    /// fully deleted, so listing g graphs costs O(g·r log n) instead of a full scan.
    pub(crate) fn next_named_graph(&self, after: Option<TermId>) -> Option<TermId> {
        let first = match after {
            Some(graph) => graph.raw().checked_add(1)?,
            None => TermId::DEFAULT_GRAPH.raw() + 1,
        };
        let plan = AccessPlan {
            permutation: Permutation::Gspo,
            low: [first, 0, 0, 0],
            high: [u64::MAX; 4],
            exclude_default_graph: false,
        };
        self.scan_plan(&plan).next().map(|quad| quad.graph)
    }

    /// Exact number of visible quads matching `plan`. By the exact-delta invariant each
    /// tombstone cancels exactly one insert of the same key in an older run, and both lie in
    /// any key range that holds either, so the count is the sum over runs of (entries in range
    /// − 2 × tombstones in range): O(r log n) for tombstone-free runs, plus a popcount over
    /// the range otherwise. Plans with a default-graph post-filter count by scanning.
    pub(crate) fn count_plan(&self, plan: &AccessPlan) -> u64 {
        self.count_plan_visiting(plan, &|_| true)
            .expect("never stopped")
    }

    /// [`Self::count_plan`], calling `visit(n)` (`n` at most `POLL_UNITS`) before the work it
    /// counts. That is each run's range search, with its comparisons (`PackedKeys::range_counted`),
    /// its tombstone words as they are read (`PermutationRun::tombstones_in_visiting`), and
    /// the merge work of a default-graph post-filter scan ([`QuadScan::next_counted`]).
    /// `None` as soon as `visit` returns false, nothing more read (REAS-2 fix check r1,
    /// R2-F1: the count lives in the reader).
    pub(crate) fn count_plan_visiting(&self, plan: &AccessPlan, visit: Visitor<'_>) -> Option<u64> {
        let Some(adapted) = self.layout.adapt(plan) else {
            return Some(0);
        };
        if adapted.exclude_default_graph {
            let mut count = 0u64;
            let mut scan = self.scan_plan_counted(plan, visit).ok()?;
            while scan.next_counted(visit).ok()?.is_some() {
                count += 1;
            }
            return Some(count);
        }
        let mut signed: i64 = 0;
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm
                .keys
                .range_counted(&adapted.low, &adapted.high, &mut { visit })?;
            let tombstones = perm.tombstones_in_visiting(start, end, visit)?;
            signed += (end - start) as i64 - 2 * tombstones as i64;
        }
        Some(u64::try_from(signed).expect("exact deltas never make a range count negative"))
    }

    /// True if some visible quad matches `plan`: one bound search per run, where no run has
    /// tombstones (otherwise the exact count decides).
    pub(crate) fn any_plan(&self, plan: &AccessPlan) -> bool {
        let Some(adapted) = self.layout.adapt(plan) else {
            return false;
        };
        let has_tombstones = self
            .runs
            .iter()
            .any(|run| !run.permutation(adapted.permutation).tombstones.is_empty());
        if adapted.exclude_default_graph || has_tombstones {
            return self.count_plan(plan) > 0;
        }
        self.runs.iter().any(|run| {
            let keys = &run.permutation(adapted.permutation).keys;
            let start = keys.bound_in(0, keys.len(), &adapted.low, false);
            start < keys.len() && keys.get(start) <= adapted.high
        })
    }

    /// [`Self::group_counts`] keeping only the groups a run counts more than `above` times,
    /// pushed as they are walked (one run after another, on this thread): the candidates for
    /// a value counted more than `above` times the runs in all of them together (a sum of
    /// `r` counts past `r * above` has one past `above`). No light group is held, and what
    /// is kept is charged to `out`'s budget as it grows: a refusal ends the walk.
    pub(crate) fn group_candidates(
        &self,
        plan: &AccessPlan,
        position: usize,
        above: i64,
        out: &mut ChargedVec<(u64, i64)>,
    ) -> Result<(), BudgetExceeded> {
        let Some(adapted) = self.layout.adapt(plan) else {
            return Ok(());
        };
        // As in `group_counts`: the default graph's group of a graph-first plan adapted.
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            let count = self.count_plan(plan) as i64;
            return match count > above {
                true => out.push((TermId::DEFAULT_GRAPH.raw(), count)),
                false => Ok(()),
            };
        } else {
            position - 1
        };
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            let mut kept = Ok(());
            perm.keys.groups(start, end, position, |value, i, j| {
                let count = (j - i) as i64 - 2 * perm.tombstones_in(i, j) as i64;
                if count > above && kept.is_ok() {
                    kept = out.push((value, count));
                }
            });
            kept?;
        }
        Ok(())
    }

    /// For each distinct value at key `position` of `plan`'s permutation (the first component
    /// after the plan's bound prefix), the signed number of quads with that value in each
    /// run, appended to `out`: sorted by value within a run, a value once per run that has
    /// it. Each run's range is walked group by group ([`PackedKeys::groups`]): no search per
    /// group, so many small groups cost a scan of the column, few large ones a search each.
    ///
    /// `out`, the parts' slab and each part past its block of it ([`PART_BLOCK`]) are
    /// charged to `budget` (`out`'s), one charge a growth, before they allocate: a reader's
    /// walk can't outgrow its budget (D16-039 F7). Refused, the walk ends with what it
    /// charged released.
    ///
    /// Its large runs are walked in parallel parts only where the budget's room, read once
    /// before anything grows, covers the whole walk's peak ([`group_walk_peak`]); otherwise
    /// every run is walked serially, the reference. So, against a budget no one else draws
    /// from during the walk, a walk is refused only where the fully serial walk would be. A
    /// concurrent consumer can take room after the check: the walk is then refused, typed,
    /// never answered short.
    pub(crate) fn group_counts<'b>(
        &self,
        plan: &AccessPlan,
        position: usize,
        out: &mut ChargedVec<(u64, i64), &'b Budget>,
        budget: &'b Budget,
    ) -> Result<(), BudgetExceeded> {
        let (keys, parts) = self.group_walk_size(plan, position);
        let in_parts = out.capacity() == 0
            && walks_in_parts(budget, keys, parts, || self.parted_groups(plan, position));
        self.group_counts_on(plan, position, out, budget, in_parts)
    }

    /// [`Self::group_counts`] on a path chosen for the whole walk: its large runs in parallel
    /// parts (`in_parts`), or every run serially. The caller's choice keeps the guarantee
    /// only where it is [`Self::group_counts`]'s: parts only where the room covers the peak
    /// of every walk that may run beside this one, `out` without capacity at the start.
    pub(crate) fn group_counts_on<'b>(
        &self,
        plan: &AccessPlan,
        position: usize,
        out: &mut ChargedVec<(u64, i64), &'b Budget>,
        budget: &'b Budget,
        in_parts: bool,
    ) -> Result<(), BudgetExceeded> {
        let Some(adapted) = self.layout.adapt(plan) else {
            return Ok(());
        };
        debug_assert!(
            !adapted.exclude_default_graph,
            "group counts need an exact range"
        );
        // Adapting a graph-first plan to the single-graph layout moves the graph to the end.
        // Grouping by that graph puts every quad of this layout in the default graph's group.
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            return out.push((TermId::DEFAULT_GRAPH.raw(), self.count_plan(plan) as i64));
        } else {
            position - 1
        };
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            // The walk can't stop between groups: a refused group ends what it appends, and
            // the walk only skips over the rest. A group costs a push within the capacity
            // charged; only a growth, logarithmically many, calls the budget.
            let walk = |start: usize,
                        end: usize,
                        out: &mut ChargedVec<(u64, i64), &'b Budget>|
             -> Result<(), BudgetExceeded> {
                let mut refused = None;
                perm.keys.groups(start, end, position, |value, i, j| {
                    let count = (j - i) as i64 - 2 * perm.tombstones_in(i, j) as i64;
                    if let Err(group) = out.push_within((value, count)) {
                        grow_then_push(out, group, &mut refused);
                    }
                });
                refused.map_or(Ok(()), Err)
            };
            // The path is the whole walk's (package fix checks r3, r4): a run walked serially
            // after a merged one would grow from the merge's capacity, not the reference's.
            if !in_parts || end - start < 2 * GROUP_PART {
                walk(start, end, out)?;
                continue;
            }
            // Large ranges in parts on every core, cut at block boundaries; a group that
            // spans a cut is added up where the parts meet.
            let cuts: Vec<usize> = std::iter::once(start)
                .chain((start / GROUP_PART + 1..end.div_ceil(GROUP_PART)).map(|p| p * GROUP_PART))
                .chain(std::iter::once(end))
                .collect();
            // Each part writes its groups into its block of one slab, charged here, serially;
            // only a part past its block grows a vector of its own ([`PART_BLOCK`]). A count
            // of blocks no vector can hold saturates, and the slab is refused, typed.
            let blocks = (cuts.len() - 1).saturating_mul(PART_BLOCK);
            let mut slab = ChargedVec::<(u64, i64), _>::with_capacity(budget, blocks)?;
            slab.resize(blocks, (0, 0))?;
            // The refusal of the first part in cut order that was refused is kept beside the
            // parts, so each part collected is no larger than its vector was ([`Part`]). Which
            // one is returned doesn't hang on the order the refused parts end in: its `shared`
            // tells a request's limit from the server's.
            let refused = std::sync::Mutex::new(None::<(usize, BudgetExceeded)>);
            let mut parts = Vec::new();
            cuts.par_windows(2)
                .zip(slab.par_chunks_mut(PART_BLOCK))
                .enumerate()
                .map(|(at, (cut, block))| {
                    let (mut len, mut own, mut refusal) = (0, ChargedVec::new(budget), None);
                    perm.keys.groups(cut[0], cut[1], position, |value, i, j| {
                        let count = (j - i) as i64 - 2 * perm.tombstones_in(i, j) as i64;
                        if let Some(slot) = block.get_mut(len) {
                            *slot = (value, count);
                            len += 1;
                        } else if let Err(group) = own.push_within((value, count)) {
                            overflow_then_push(&mut own, block, group, &mut refusal);
                        }
                    });
                    if let Some(error) = refusal {
                        let mut first = refused.lock().unwrap_or_else(|p| p.into_inner());
                        if first.as_ref().is_none_or(|&(before, _)| at < before) {
                            *first = Some((at, error));
                        }
                    }
                    match own.is_empty() {
                        true => Part::Inline(len),
                        false => Part::Owned(own),
                    }
                })
                .collect_into_vec(&mut parts);
            // That refusal, once every part has ended; the slab and the parts' own vectors are
            // released as they drop.
            if let Some((_, error)) = refused.into_inner().unwrap_or_else(|p| p.into_inner()) {
                return Err(error);
            }
            // A part's groups: its block's first `len`, or its own vector, never an unused
            // slot.
            let each_part = || {
                parts
                    .iter()
                    .zip(slab.chunks(PART_BLOCK))
                    .map(|(part, block)| match part {
                        Part::Inline(len) => &block[..*len],
                        Part::Owned(own) => &own[..],
                    })
            };
            // The merge's room at once, for exactly the groups it appends: a group spanning
            // a cut is added to the one before it, not appended (sol-loop S2-2).
            let mut last = None;
            let mut appended = 0;
            for part in each_part() {
                if let (Some(&(value, _)), Some(&(end, _))) = (part.first(), part.last()) {
                    appended += part.len() - usize::from(last == Some(value));
                    last = Some(end);
                }
            }
            out.reserve(appended)?;
            let first = out.len();
            for part in each_part() {
                let Some((&(value, count), rest)) = part.split_first() else {
                    continue;
                };
                let spans = out.len() > first && out.last().is_some_and(|l| l.0 == value);
                match out.last_mut() {
                    Some(last) if spans => last.1 += count,
                    _ => out.push((value, count))?,
                }
                out.extend_from_slice(rest)?;
            }
        }
        Ok(())
    }

    /// The keys [`Self::group_counts`] reads for `plan` over this version's runs, and the
    /// parallel parts of the runs large enough to be walked in parts: what bounds its charge
    /// ([`group_walk_peak`]).
    pub(crate) fn group_walk_size(&self, plan: &AccessPlan, position: usize) -> (usize, usize) {
        let Some(adapted) = self.layout.adapt(plan) else {
            return (0, 0);
        };
        if adapted.permutation != plan.permutation && position == 0 {
            return (1, 0);
        }
        self.runs.iter().fold((0, 0), |(keys, parts), run| {
            let (start, end) = run
                .permutation(adapted.permutation)
                .range(&adapted.low, &adapted.high);
            let run_parts = if end - start >= 2 * GROUP_PART {
                end.div_ceil(GROUP_PART) - start / GROUP_PART
            } else {
                0
            };
            (keys + (end - start), parts + run_parts)
        })
    }

    /// An estimate of the groups [`Self::group_counts`] finds in the runs large enough to be
    /// walked in parts ([`Self::group_walk_size`]'s): each such run's keys over the smaller of
    /// its first and last groups, each found by one search from that end of the range
    /// ([`PackedKeys::first_group_len`], [`PackedKeys::last_group_len`]). Exact where the
    /// groups are of one size. A large group at one end is offset by a small one at the
    /// other; only where both ends hold groups larger than the rest does it underestimate.
    pub(crate) fn parted_groups(&self, plan: &AccessPlan, position: usize) -> usize {
        let Some(adapted) = self.layout.adapt(plan) else {
            return 0;
        };
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            return 0; // no run in parts: one group, the default graph's (`group_walk_size`)
        } else {
            position - 1
        };
        self.runs.iter().fold(0usize, |groups, run| {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm.range(&adapted.low, &adapted.high);
            if end - start < 2 * GROUP_PART {
                return groups;
            }
            let first = perm.keys.first_group_len(start, end, position);
            let last = perm.keys.last_group_len(start, end, position);
            groups.saturating_add((end - start).div_ceil(first.min(last).max(1)))
        })
    }

    /// Appends the walks of the distinct values at key `position` of each run's range for
    /// `plan`, sorted per walk (values may repeat across walks: [`count_distinct`] merges
    /// them). Tombstones are ignored: for estimates. Its searches and a count are counted
    /// on `visit`: `Err` once it has stopped.
    pub(crate) fn distinct_walks<'a>(
        &'a self,
        plan: &AccessPlan,
        position: usize,
        visit: Visitor<'_>,
        out: &mut Vec<ValueWalk<'a>>,
    ) -> Result<(), Stopped> {
        let Some(adapted) = self.layout.adapt(plan) else {
            return Ok(());
        };
        let position = if adapted.permutation == plan.permutation {
            position
        } else if position == 0 {
            let count = self.count_plan_visiting(plan, visit).ok_or(Stopped)?;
            out.push(ValueWalk::One(
                (count > 0).then_some(TermId::DEFAULT_GRAPH.raw()),
            ));
            return Ok(());
        } else {
            position - 1
        };
        for run in self.runs.iter() {
            let perm = run.permutation(adapted.permutation);
            let (start, end) = perm
                .keys
                .range_counted(&adapted.low, &adapted.high, &mut { visit })
                .ok_or(Stopped)?;
            out.push(ValueWalk::Groups(
                perm.keys.group_cursor(start, end, position),
            ));
        }
        Ok(())
    }

    /// The scan, a block and a column at a time, of `components` (quad components:
    /// subject 0, predicate 1, object 2, graph 3) among the quads matching `plan`, in the
    /// plan's order: its rows are known before anything is decoded, so a caller charges
    /// them first ([`ColumnScan::rows`], then [`ColumnScan::decode`]). Answers only for a
    /// version of at most one run without tombstones (`None` otherwise: then quads must be
    /// merged and cancelled one by one).
    pub(crate) fn column_scan(
        &self,
        plan: &AccessPlan,
        components: &[usize],
    ) -> Option<ColumnScan<'_>> {
        let nothing = ColumnScan {
            keys: None,
            start: 0,
            end: 0,
            positions: Vec::new(),
        };
        let Some(adapted) = self.layout.adapt(plan) else {
            return Some(nothing); // nothing this layout holds matches
        };
        if adapted.exclude_default_graph {
            return None;
        }
        let run = match &*self.runs {
            [] => return Some(nothing),
            [run] => run,
            _ => return None,
        };
        let perm = run.permutation(adapted.permutation);
        if !perm.tombstones.is_empty() {
            return None;
        }
        let order = adapted.permutation.order();
        let positions: Vec<usize> = components
            .iter()
            .map(|c| {
                order
                    .iter()
                    .position(|x| x == c)
                    .expect("every component has a key position")
            })
            .collect();
        let (start, end) = perm.range(&adapted.low, &adapted.high);
        Some(ColumnScan {
            keys: Some(&perm.keys),
            start,
            end,
            positions,
        })
    }

    pub(crate) fn scan_plan(&self, plan: &AccessPlan) -> QuadScan<'_> {
        let Some(plan) = self.layout.adapt(plan) else {
            return QuadScan::new(
                SignedMerge::new(std::iter::empty()),
                plan.permutation,
                false,
            );
        };
        let parts = self.runs.iter().map(|run| {
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::RunProbes, 1);
            let perm = run.permutation(plan.permutation);
            let (start, end) = perm.range(&plan.low, &plan.high);
            (perm, start, end)
        });
        QuadScan::new(
            SignedMerge::new(parts),
            plan.permutation,
            plan.exclude_default_graph,
        )
    }

    /// `self` with `extra` on top, for reading only: their signs add to the stack's, so
    /// together they must leave each quad at 0 or 1 ([`crate::InferredMask`]).
    pub(crate) fn with_runs(&self, extra: &[Arc<Run>]) -> Self {
        let net: i64 = extra.iter().map(|run| run.net()).sum();
        Self {
            layout: self.layout,
            runs: self.runs.iter().chain(extra).cloned().collect(),
            len: self
                .len
                .checked_add_signed(net)
                .expect("a mask hides only quads the stack holds"),
        }
    }

    /// A new version with `run` appended. The run must be an exact delta against `self`.
    pub(crate) fn with_run(&self, run: Run) -> Self {
        if run.entries() == 0 {
            return self.clone();
        }
        debug_assert_eq!(run.layout(), self.layout, "run built for another stack");
        let len = self
            .len
            .checked_add_signed(run.net())
            .expect("exact delta can't make the quad count negative");
        let runs = self.runs.iter().cloned().chain([Arc::new(run)]).collect();
        Self {
            layout: self.layout,
            runs,
            len,
        }
    }

    /// A new version where the first `covered` runs are replaced by the runs of `base`, which
    /// must hold exactly what those runs make visible (a checkpoint of them).
    pub(crate) fn with_base(&self, covered: usize, base: &IndexVersion) -> Self {
        debug_assert_eq!(base.layout, self.layout, "base built for another stack");
        let runs = base
            .runs
            .iter()
            .chain(&self.runs[covered..])
            .cloned()
            .collect();
        Self {
            layout: self.layout,
            runs,
            len: self.len,
        }
    }

    /// A new version where the runs in `window` are replaced by `merged`, which must be
    /// [`compaction::merge_runs`] of exactly those runs.
    pub(crate) fn with_compacted(&self, window: Range<usize>, merged: Run) -> Self {
        let runs = self.runs[..window.start]
            .iter()
            .cloned()
            .chain((merged.entries() > 0).then(|| Arc::new(merged)))
            .chain(self.runs[window.end..].iter().cloned())
            .collect();
        Self {
            layout: self.layout,
            runs,
            len: self.len,
        }
    }
}

/// Where a structure being built reserves its memory before it allocates it: nowhere for
/// the store's own runs ([`Uncharged`]), a reservation for a canonical view's
/// (`engine::equality`), which the view then holds. Builders take it as a type parameter:
/// the store's path is compiled with [`Uncharged`], whose calls inline to nothing.
pub(crate) trait Charge {
    /// Whether it counts anything: a builder that holds its memory to its charge may
    /// build otherwise where it does (resolved when compiled: nothing on the store's path).
    const COUNTED: bool;
    fn reserve(&mut self, bytes: usize) -> Result<(), BudgetExceeded>;
    fn release(&mut self, bytes: usize);
}

/// Nothing reserved.
pub(crate) struct Uncharged;

impl Charge for Uncharged {
    const COUNTED: bool = false;

    #[inline(always)]
    fn reserve(&mut self, _: usize) -> Result<(), BudgetExceeded> {
        Ok(())
    }

    #[inline(always)]
    fn release(&mut self, _: usize) {}
}

impl Charge for Reservation {
    const COUNTED: bool = true;

    fn reserve(&mut self, bytes: usize) -> Result<(), BudgetExceeded> {
        self.charge(bytes)
    }

    fn release(&mut self, bytes: usize) {
        Reservation::release(self, bytes);
    }
}

impl IndexVersion {
    /// The most a scan of this version holds while it runs (its merge's cursors and their
    /// decoded keys): what a charged reader reserves before it scans.
    pub(crate) fn scan_bytes(&self) -> usize {
        merge::scan_bytes(self.runs.len())
    }

    /// The bytes [`Self::with_run`] allocates for the list of runs, at most at once.
    pub(crate) fn run_list_bytes(&self) -> usize {
        self.runs_bytes(1)
    }

    /// The bytes a version with `extra` more runs allocates for its list of runs, at most at
    /// once ([`Self::with_run`], [`Self::with_runs`]).
    pub(crate) fn runs_bytes(&self, extra: usize) -> usize {
        2 * (self.runs.len() + extra) * std::mem::size_of::<Arc<Run>>() + 16
    }
}

/// A column scan of one version ([`IndexVersion::column_scan`]): how many rows it decodes,
/// known before it decodes them.
pub(crate) struct ColumnScan<'a> {
    keys: Option<&'a PackedKeys>,
    start: usize,
    end: usize,
    positions: Vec<usize>,
}

impl ColumnScan<'_> {
    /// The rows [`Self::decode`] appends to each column.
    pub(crate) fn rows(&self) -> usize {
        self.end - self.start
    }

    /// Appends each component's values to the matching vector of `out`, which grows by
    /// exactly [`Self::rows`]: the capacity a caller charged for them.
    pub(crate) fn decode(&self, out: &mut [Vec<u64>]) {
        if let Some(keys) = self.keys {
            keys.decode_columns(self.start, self.end, &self.positions, out);
        }
    }
}

/// Iterator returned by [`IndexVersion::scan`].
pub(crate) struct QuadScan<'a> {
    merge: SignedMerge<'a>,
    permutation: Permutation,
    exclude_default_graph: bool,
    /// A counted scan's ledger between its steps: units asked for and not yet taken, and the
    /// last run asked for ([`Ledger`]).
    ledger: (u64, u64),
}

pub(crate) use crate::visit::{Stopped, Visitor};

impl<'a> QuadScan<'a> {
    fn new(merge: SignedMerge<'a>, permutation: Permutation, exclude_default_graph: bool) -> Self {
        Self {
            merge,
            permutation,
            exclude_default_graph,
            ledger: (0, 0),
        }
    }

    /// A scan of nothing: what a counted read stopped while its scan was being made goes on
    /// with (its stop the visitor's to tell).
    pub(crate) fn empty(permutation: Permutation) -> Self {
        Self::new(SignedMerge::new(std::iter::empty()), permutation, false)
    }

    /// The next visible quad, its merge's work counted on `account` ([`SignedMerge::step`]).
    /// The keys passed over (deleted ones, or the default graph's where it is left out) count
    /// as much as the one handed on (REAS-2 fix check r1, R2-F1: a long deleted run can't be
    /// drained unseen). The plain scan, the counted scan and a count all use it.
    #[inline]
    pub(crate) fn step_with(
        &mut self,
        account: &mut impl Account,
    ) -> Result<Option<EncodedQuad>, Stopped> {
        loop {
            let Some((key, sign)) = self.merge.step(account)? else {
                return Ok(None);
            };
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::ScanSteps, 1);
            if sign <= 0 {
                continue;
            }
            let quad = self.permutation.key_to_quad(&key);
            if self.exclude_default_graph && quad.graph.is_default_graph() {
                continue;
            }
            return Ok(Some(quad));
        }
    }

    /// [`Iterator::next`], its work counted on `visit` before it is done, from the ledger
    /// the scan's making started (REAS-3, sweeps SW005, SW007; fix check r1 F1). `Err` once
    /// `visit` has stopped, and on every call after: nothing more is read. Readers that count
    /// nothing use `next`.
    #[inline]
    pub(crate) fn next_counted(
        &mut self,
        visit: Visitor<'_>,
    ) -> Result<Option<EncodedQuad>, Stopped> {
        let mut ledger = Ledger::new(visit);
        (ledger.left, ledger.run) = self.ledger;
        let next = self.step_with(&mut ledger);
        self.ledger = (ledger.left, ledger.run);
        next
    }

    /// This scan as an iterator for a counted read: [`Self::next_counted`]'s quads, ending
    /// where it ends or stops (the stop is the visitor's to tell: `visit(0)`).
    pub(crate) fn counted<'v>(self, visit: Visitor<'v>) -> CountedScan<'a, 'v> {
        CountedScan { scan: self, visit }
    }
}

impl Iterator for QuadScan<'_> {
    type Item = EncodedQuad;

    /// [`QuadScan::step_with`], counting nothing. A scan that a counted read stopped reads as
    /// ended here ([`SignedMerge`]'s iterator).
    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        self.step_with(&mut Free).unwrap_or(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.merge.remaining()))
    }
}

/// [`QuadScan::counted`]: [`QuadScan::next_counted`]'s quads, the visitor asked once per
/// run of the scan's ledger, not once per step.
pub(crate) struct CountedScan<'a, 'v> {
    scan: QuadScan<'a>,
    visit: Visitor<'v>,
}

impl Iterator for CountedScan<'_, '_> {
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        self.scan.next_counted(self.visit).ok().flatten()
    }
}

/// One source of a distinct count ([`IndexVersion::distinct_walks`]): its values in
/// ascending order, each once.
pub(crate) enum ValueWalk<'a> {
    /// A run's groups at a position.
    Groups(keys::Groups<'a>),
    /// At most one value, known already (the default graph of a layout without the graph
    /// position).
    One(Option<u64>),
}

impl ValueWalk<'_> {
    fn next(&mut self, account: &mut impl Account) -> Result<Option<u64>, Stopped> {
        match self {
            Self::Groups(groups) => Ok(groups.next(account)?.map(|(value, _, _)| value)),
            Self::One(value) => Ok(value.take()),
        }
    }
}

/// The number of distinct values among `walks`: merged in ascending order, each value
/// counted once however many walks hold it, so the count is exact where today's gathered
/// list was (no list is held: a head per walk). The walks' blocks and each merge step are
/// counted on `visit`: `Err` once it has stopped.
pub(crate) fn count_distinct(
    walks: Vec<ValueWalk<'_>>,
    visit: Visitor<'_>,
) -> Result<u64, Stopped> {
    let mut account = Ledger::new(visit);
    let mut heads = Vec::with_capacity(walks.len());
    for mut walk in walks {
        if let Some(value) = walk.next(&mut account)? {
            heads.push((value, walk));
        }
    }
    let mut count = 0u64;
    if let [(_, walk)] = &mut heads[..] {
        count += 1;
        while walk.next(&mut account)?.is_some() {
            count += 1;
        }
        return Ok(count);
    }
    while let Some(least) = heads.iter().map(|&(value, _)| value).min() {
        if !account.take((heads.len() as u64).min(crate::visit::PORTION)) {
            return Err(Stopped);
        }
        count += 1;
        let mut k = 0;
        while k < heads.len() {
            if heads[k].0 != least {
                k += 1;
                continue;
            }
            match heads[k].1.next(&mut account)? {
                Some(value) => {
                    heads[k].0 = value;
                    k += 1;
                }
                None => {
                    heads.swap_remove(k);
                }
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod bound_tests;
#[cfg(test)]
mod count_tests;
#[cfg(test)]
mod model_tests;
