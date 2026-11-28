use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::compaction::merge_triples;
use super::{CompactionPolicy, IndexVersion, Layout, Run};
use crate::quad::{AccessPlan, EncodedQuad, QuadPattern};
use crate::term::ChargedArc;
use crate::{Stopped, TermId, Visitor};

type Triple = [u64; 3];
type Pattern = [Option<u64>; 3];
type CompactionResult<T> = Result<Option<T>, BudgetExceeded>;
type MergeResult<T> = Result<T, BudgetExceeded>;

/// An immutable version of a set of encoded triples. Clones share their list, and versions
/// share any runs they have in common. All retained storage is charged once.
#[derive(Debug, Clone)]
pub struct TripleVersion(ChargedArc<Storage>);

type TripleIndex = IndexVersion<ChargedArc<Run>>;

#[derive(Debug)]
struct Storage {
    index: TripleIndex,
    budget: Arc<Budget>,
}

const VERSION_BYTES: usize = ChargedArc::<Storage>::COST + 2 * size_of::<usize>();
const RUN_REF_BYTES: usize = size_of::<ChargedArc<Run>>();
const INLINE_RUN_LIMIT: usize = 2;

/// Holds the end of the owner's window. Writers can still compact the tail after it.
struct CompactionPin(ChargedArc<Run>);

impl CompactionPin {
    fn new(end: &ChargedArc<Run>) -> Self {
        end.compactions().fetch_add(1, Relaxed);
        Self(end.clone())
    }
}

impl Drop for CompactionPin {
    fn drop(&mut self) {
        self.0.compactions().fetch_sub(1, Relaxed);
    }
}

/// Work reserved for a background compaction. Dropping it releases the input window.
pub struct TripleCompaction {
    captured: TripleVersion,
    window: Range<usize>,
    pin: Option<CompactionPin>,
}

impl TripleCompaction {
    /// Build outside the owner's mailbox lock. The splice still needs matching input runs.
    pub fn build(
        self,
    ) -> MergeResult<impl FnOnce(&TripleVersion) -> CompactionResult<TripleVersion> + Send + use<>>
    {
        let Self {
            captured,
            window,
            pin,
        } = self;
        let merged = merge_triples(&captured.0.index.runs[window.clone()], &captured.0.budget)?;
        Ok(move |current: &TripleVersion| {
            let _pin = pin;
            let index = &current.0.index;
            if !index.retains(&captured.0.index, window.clone()) {
                return Ok(None);
            }
            let run_count = index.runs.len() - window.len() + usize::from(merged.is_some());
            TripleVersion::with_index(&current.0.budget, run_count, || {
                index.with_compacted_ref(window, merged)
            })
            .map(Some)
        })
    }
}

impl TripleVersion {
    /// Builds a set from triples in any order, ignoring duplicates.
    pub fn build(
        triples: impl IntoIterator<Item = Triple>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut quads = ChargedVec::new(Arc::clone(budget));
        for triple in triples {
            quads.push(quad(triple))?;
        }
        quads.sort_unstable();
        let (mut quads, scratch) = quads.into_parts();
        quads.dedup();
        let len = quads.len() as u64;
        let run = (!quads.is_empty())
            .then(|| Self::delta(&quads, &[], budget))
            .transpose()?;
        drop(quads);
        drop(scratch);
        Self::with_index(budget, usize::from(run.is_some()), || TripleIndex {
            layout: Layout::DefaultGraph,
            len,
            runs: run.into_iter().collect(),
        })
    }

    /// Appends an exact delta to this version. Both slices must be strictly ascending;
    /// every insert must be absent and every delete present in this version.
    pub fn successor(
        &self,
        inserts: &[Triple],
        deletes: &[Triple],
    ) -> Result<Self, BudgetExceeded> {
        if inserts.is_empty() && deletes.is_empty() {
            return Ok(self.clone());
        }
        assert!(inserts.is_sorted_by(|a, b| a < b) && deletes.is_sorted_by(|a, b| a < b));
        assert!(inserts.iter().all(|&t| !self.contains(t)));
        assert!(deletes.iter().all(|&t| self.contains(t)));
        let budget = &self.0.budget;
        let quads = |triples: &[Triple]| {
            let mut out = ChargedVec::with_capacity(Arc::clone(budget), triples.len())?;
            for &triple in triples {
                out.push(quad(triple))?;
            }
            Ok::<_, BudgetExceeded>(out)
        };
        let (inserts, deletes) = (quads(inserts)?, quads(deletes)?);
        let run = Self::delta(&inserts, &deletes, budget)?;
        let index = &self.0.index;
        Self::with_index(budget, index.runs.len() + 1, || index.with_run_ref(run))
    }

    fn delta(
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
        budget: &Arc<Budget>,
    ) -> Result<ChargedArc<Run>, BudgetExceeded> {
        let mut built =
            Run::from_delta_within_parallel(Layout::DefaultGraph, inserts, deletes, budget)?;
        built.0.keep_triples(budget, &mut built.1)?;
        ChargedArc::new(built.0, budget)
    }

    fn with_index(
        budget: &Arc<Budget>,
        run_count: usize,
        build: impl FnOnce() -> TripleIndex,
    ) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(Arc::clone(budget));
        // The list's working buffer may overlap its shared allocation during collection.
        held.charge(VERSION_BYTES + 2 * RUN_REF_BYTES * run_count)?;
        let index = build();
        debug_assert_eq!(index.runs.len(), run_count);
        held.release(RUN_REF_BYTES * run_count);
        Ok(Self(ChargedArc::with_reserved(
            Storage {
                index,
                budget: Arc::clone(budget),
            },
            held,
        )))
    }

    /// Builds one compaction of this version. The returned operation splices the result
    /// into a current version if the inputs still match, preserving any later appends.
    ///
    /// The writer takes small windows and caps the list at two runs.
    /// While the background owner holds its two input runs, a third run is allowed and
    /// the writer merges only the tail after the owner's window. If that merge cannot
    /// be charged, the writer can refuse the staged version and keep the old readers' runs.
    pub fn compact(
        &self,
        policy: CompactionPolicy,
    ) -> CompactionResult<impl FnOnce(&Self) -> CompactionResult<Self> + Send + use<>> {
        let runs = &self.0.index.runs;
        let floor = runs
            .iter()
            .rposition(|run| run.compactions().load(Relaxed) != 0)
            .map_or(0, |at| at + 1);
        let sized = policy
            .plan(&runs[floor..])
            .filter(|plan| policy.is_inline(plan));
        let forced = runs.len() > INLINE_RUN_LIMIT && runs.len() - floor > 1;
        let start = sized.map_or(runs.len(), |plan| plan.window.start + floor);
        let start = if forced {
            start.min(floor.max(INLINE_RUN_LIMIT - 1))
        } else {
            start
        };
        if start == runs.len() {
            return Ok(None);
        }
        TripleCompaction {
            captured: self.clone(),
            window: start..runs.len(),
            pin: None,
        }
        .build()
        .map(Some)
    }

    /// Reserve an owner's window before releasing its mailbox lock. The returned work
    /// builds the merge outside that lock, and its splice can retain subsequent appends.
    pub fn plan_compaction(&self, policy: CompactionPolicy) -> Option<TripleCompaction> {
        let runs = &self.0.index.runs;
        let window = if runs.len() > INLINE_RUN_LIMIT {
            // A previous owner can leave a third run on refusal. Fold the base first.
            0..INLINE_RUN_LIMIT
        } else {
            policy.plan(runs)?.window
        };
        Some(TripleCompaction {
            captured: self.clone(),
            pin: Some(CompactionPin::new(&runs[window.end - 1])),
            window,
        })
    }

    /// Whether the size tiers select a merge for this version.
    pub fn needs_compaction(&self, policy: CompactionPolicy) -> bool {
        policy.plan(&self.0.index.runs).is_some()
    }

    pub fn len(&self) -> usize {
        self.0.index.len() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(feature = "witness")]
    #[doc(hidden)]
    pub fn packed_bytes(&self) -> usize {
        self.0
            .index
            .runs
            .iter()
            .map(|run| run.memory_bytes() as usize)
            .sum()
    }

    pub fn contains(&self, triple: Triple) -> bool {
        self.0.index.contains(&quad(triple))
    }

    /// The exact number of visible triples with this predicate, one summary lookup per run.
    pub fn count_predicate(&self, predicate: u64) -> usize {
        let count: i64 = self
            .0
            .index
            .runs
            .iter()
            .map(|run| run.predicate_delta(predicate))
            .sum();
        debug_assert!(count >= 0);
        count as usize
    }

    pub fn scan(&self, bound: Pattern) -> impl Iterator<Item = Triple> {
        self.0.index.scan(&pattern(bound)).map(triple)
    }

    /// Counts the seeks, merges and cancelled signs, including work before the first match.
    pub fn scan_counted<'a, 'v>(
        &'a self,
        bound: Pattern,
        visit: Visitor<'v>,
    ) -> Result<impl Iterator<Item = Result<Triple, Stopped>> + use<'a, 'v>, Stopped> {
        let plan = AccessPlan::for_pattern(&pattern(bound));
        let mut scan = self.0.index.scan_plan_counted(&plan, visit)?;
        let mut stopped = false;
        Ok(std::iter::from_fn(move || {
            if stopped {
                return None;
            }
            match scan.next_counted(visit) {
                Ok(next) => next.map(|quad| Ok(triple(quad))),
                Err(error) => {
                    stopped = true;
                    Some(Err(error))
                }
            }
        }))
    }

    /// Counts only as far as the first match; a stopped probe has no truth value.
    pub fn any_counted(&self, bound: Pattern, visit: Visitor<'_>) -> Result<bool, Stopped> {
        if let [Some(s), Some(p), Some(o)] = bound {
            return self.0.index.contains_counted(&quad([s, p, o]), visit);
        }
        Ok(self
            .scan_counted(bound, visit)?
            .next()
            .transpose()?
            .is_some())
    }
}

fn quad([s, p, o]: Triple) -> EncodedQuad {
    EncodedQuad::new(
        TermId::from_raw(s),
        TermId::from_raw(p),
        TermId::from_raw(o),
        TermId::DEFAULT_GRAPH,
    )
}

fn triple(quad: EncodedQuad) -> Triple {
    [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()]
}

fn pattern(bound: Pattern) -> QuadPattern {
    let [subject, predicate, object] = bound.map(|t| t.map(TermId::from_raw));
    QuadPattern {
        subject,
        predicate,
        object,
        ..QuadPattern::all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 23;
    const A: Triple = [0, P, 1];
    const T: Triple = [100, P, 1];
    const Z: Triple = [200, P, 1];
    const PREDICATE: Pattern = [None, Some(P), None];

    fn base(budget: &Arc<Budget>) -> TripleVersion {
        TripleVersion::build((0..100).map(|s| [s, P, 1]), budget).unwrap()
    }

    /// A proper suffix cannot cancel a deletion against an older insertion.
    #[test]
    fn triple_compaction_keeps_an_unmatched_deletion_and_the_held_list() {
        let budget = Arc::new(Budget::unlimited());
        let pinned = base(&budget).successor(&[T], &[]).unwrap();
        let current = pinned.successor(&[], &[A, T]).unwrap();
        let policy = CompactionPolicy {
            inline_entry_limit: 0,
            ..Default::default()
        };
        let compact = current.compact(policy).unwrap().expect("forced suffix");
        let latest = compact(&current).unwrap().unwrap();
        assert_eq!((latest.contains(A), latest.contains(T)), (false, false));
        assert_eq!((pinned.contains(A), pinned.contains(T)), (true, true));
        assert_eq!(latest.scan(PREDICATE).count(), 99);
        assert_eq!(latest.count_predicate(P), 99);
    }

    /// The old allocation is shared before a rewrite and retains its charge
    /// while a reader still holds it after the rewrite.
    #[test]
    fn triple_run_charges_follow_the_shared_allocations() {
        let budget = Arc::new(Budget::unlimited());
        let pinned = base(&budget);
        let original = budget.used();
        let alias = pinned.clone();
        assert_eq!(budget.used(), original);
        let current = pinned.successor(&[T], &[]).unwrap();
        assert!(
            budget.used() - original < original,
            "the successor copied the base"
        );
        let policy = CompactionPolicy {
            fanout: 128,
            ..CompactionPolicy::default()
        };
        let splice = current.plan_compaction(policy).unwrap().build().unwrap();
        let latest = splice(&current).unwrap().unwrap();
        drop(current);
        assert_eq!(
            budget.used(),
            original
                + VERSION_BYTES
                + RUN_REF_BYTES * latest.0.index.runs.len()
                + latest.0.index.runs[0].memory_bytes() as usize
                + latest.0.index.runs[0].triple_overhead()
        );
        let both = budget.used();
        drop(pinned);
        assert_eq!(budget.used(), both);
        drop(alias);
        assert_eq!(budget.used(), both - original);
        assert!(latest.contains(T));
        drop(latest);
        assert_eq!(budget.used(), 0);
    }

    /// Signed summaries report zero after a large retract. The writer
    /// enforces its cap even with background compaction delayed.
    #[test]
    fn triple_predicate_counts_are_exact_during_deferred_compaction() {
        let budget = Arc::new(Budget::unlimited());
        let policy = CompactionPolicy::default();
        let facts: Vec<_> = (0..16_385).map(|s| [s, P, 1]).collect();
        let mut latest = TripleVersion::build([[0, P + 1, 1]], &budget).unwrap();
        for _ in 0..4 {
            let pinned = latest.successor(&facts, &[]).unwrap();
            latest = pinned.successor(&[], &facts).unwrap();
            assert_eq!(latest.count_predicate(P), 0);
            assert_eq!(latest.count_predicate(P + 1), 1);
            assert_eq!(pinned.count_predicate(P), facts.len());
            let compact = latest
                .compact(policy)
                .unwrap()
                .expect("the writer enforces the cap");
            latest = compact(&latest).unwrap().unwrap();
            assert!(latest.0.index.runs.len() <= 2);
        }
        assert!(!latest.any_counted(PREDICATE, &|_| true).unwrap());
    }

    /// A new run can exceed the cap without making a size-tiered window.
    #[test]
    fn triple_writer_compacts_at_the_cap_even_without_a_size_tier() {
        let budget = Arc::new(Budget::unlimited());
        let policy = CompactionPolicy::default();
        let base = TripleVersion::build((0..10_000).map(|s| [s, P, 1]), &budget).unwrap();
        let middle: Vec<_> = (10_000..10_100).map(|s| [s, P, 1]).collect();
        let pinned = base.successor(&middle, &[]).unwrap();
        let last = [10_100, P, 1];
        let next = pinned.successor(&[last], &[]).unwrap();
        assert!(!next.needs_compaction(policy));
        let compact = next
            .compact(policy)
            .unwrap()
            .expect("the cap selects a suffix");
        let next = compact(&next).unwrap().unwrap();
        assert_eq!(next.0.index.runs.len(), 2);
        assert_eq!(next.len(), 10_101);
        assert!(next.contains(last));
        assert!(!pinned.contains(last));
    }

    /// A counted one-run scan obeys its upper bound and can stop
    /// partway through a long range.
    #[test]
    fn a_counted_subject_predicate_scan_stops_at_the_bound_or_visitor() {
        let budget = Arc::new(Budget::unlimited());
        let n = 4 * u64::from(nrese_exec::POLL_UNITS);
        let expected: Vec<_> = (1..=n).map(|o| [1, P, o]).collect();
        let version = TripleVersion::build(
            expected.iter().copied().chain([[1, P + 1, 1], [2, P, 1]]),
            &budget,
        )
        .unwrap();
        let bound = [Some(1), Some(P), None];
        let scan = version.scan_counted(bound, &|_| true).unwrap();
        assert_eq!(scan.collect::<Result<Vec<_>, _>>().unwrap(), expected);
        let going = std::cell::Cell::new(true);
        let visit = |_| going.get();
        let mut scan = version.scan_counted(bound, &visit).unwrap();
        assert_eq!(scan.next(), Some(Ok(expected[0])));
        going.set(false);
        let seen = scan.collect::<Result<Vec<_>, _>>();
        assert_eq!(seen, Err(Stopped));
    }

    /// Compaction completes after a writer appends another delta.
    #[test]
    fn triple_compaction_splices_into_the_version_with_a_later_append() {
        let budget = Arc::new(Budget::unlimited());
        let old = TripleVersion::build([A, T], &budget).unwrap();
        let captured = old.successor(&[], &[T]).unwrap();
        let plan = captured
            .plan_compaction(CompactionPolicy::default())
            .unwrap();
        let current = captured.successor(&[Z], &[]).unwrap();
        let splice = plan.build().unwrap();
        let latest = splice(&current).unwrap().unwrap();
        assert!(latest.contains(Z), "compaction dropped the later append");
        assert!(!latest.contains(T));
        assert!(old.contains(T));
        assert!(!old.contains(Z));
    }

    /// A stream of inserts uses the writer's cap without rewriting the runs
    /// that a compactor will splice. The reader can see the merged base and every append.
    #[test]
    fn triple_writer_preserves_a_background_merge_during_monotonic_inserts() {
        let budget = Arc::new(Budget::unlimited());
        let policy = CompactionPolicy {
            inline_entry_limit: 0,
            ..Default::default()
        };
        let old = TripleVersion::build([A], &budget)
            .unwrap()
            .successor(&[T], &[])
            .unwrap();
        let plan = old.plan_compaction(policy).unwrap();
        let mut current = old.clone();
        for s in 200..210 {
            current = current.successor(&[[s, P, 1]], &[]).unwrap();
            if let Some(compact) = current.compact(policy).unwrap() {
                current = compact(&current).unwrap().unwrap();
            }
            assert!(current.0.index.runs.len() <= 3);
        }
        let latest = plan.build().unwrap()(&current)
            .unwrap()
            .expect("writer compaction kept the owner's runs");
        assert!(latest.0.index.runs.len() <= 2);
        assert_eq!(
            latest.scan(PREDICATE).collect::<Vec<_>>(),
            [A, T]
                .into_iter()
                .chain((200..210).map(|s| [s, P, 1]))
                .collect::<Vec<_>>()
        );
        assert_eq!(old.len(), 2);
    }
}
