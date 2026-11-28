use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::compaction::merge_triples;
use super::{CompactionPolicy, IndexVersion, Layout, Run};
use crate::quad::{AccessPlan, EncodedQuad, QuadPattern};
use crate::{Stopped, TermId, Visitor};

type Triple = [u64; 3];
type Pattern = [Option<u64>; 3];

/// An immutable version of a set of encoded triples. Clones share their list, and versions
/// share any runs they have in common. All retained storage is charged once.
#[derive(Debug, Clone)]
pub struct TripleVersion(Arc<Storage>);

#[derive(Debug)]
struct Storage {
    index: IndexVersion,
    budget: Arc<Budget>,
    /// Released after the list it covers.
    _held: Reservation,
}

const ARC_HEADER: usize = 2 * size_of::<usize>();

impl TripleVersion {
    /// Builds a set from triples in any order, ignoring duplicates.
    pub fn build(
        triples: impl IntoIterator<Item = Triple>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let mut held = Reservation::new(Arc::clone(budget));
        held.charge(size_of::<Storage>() + 2 * ARC_HEADER)?;
        let mut quads = ChargedVec::new(Arc::clone(budget));
        for triple in triples {
            quads.push(quad(triple))?;
        }
        quads.sort_unstable();
        let (mut quads, scratch) = quads.into_parts();
        quads.dedup();
        let empty = IndexVersion::empty(Layout::DefaultGraph);
        let index = if quads.is_empty() {
            empty
        } else {
            held.charge(2 * size_of::<Arc<Run>>())?;
            let run = Self::delta(&quads, &[], budget)?;
            let index = empty.with_run(run);
            held.release(size_of::<Arc<Run>>());
            index
        };
        drop(quads);
        drop(scratch);
        Ok(Self(Arc::new(Storage {
            index,
            budget: Arc::clone(budget),
            _held: held,
        })))
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
        self.with_index(self.0.index.runs.len() + 1, || self.0.index.with_run(run))
    }

    fn delta(
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
        budget: &Arc<Budget>,
    ) -> Result<Run, BudgetExceeded> {
        let mut built =
            Run::from_delta_within_parallel(Layout::DefaultGraph, inserts, deletes, budget)?;
        built.0.keep_triples(budget, &mut built.1)?;
        Ok(built.0)
    }

    fn with_index(
        &self,
        max_runs: usize,
        build: impl FnOnce() -> IndexVersion,
    ) -> Result<Self, BudgetExceeded> {
        let budget = Arc::clone(&self.0.budget);
        let mut held = Reservation::new(Arc::clone(&budget));
        held.charge(size_of::<Storage>() + 2 * ARC_HEADER + 2 * size_of::<Arc<Run>>() * max_runs)?;
        let index = build();
        held.release(size_of::<Arc<Run>>() * (2 * max_runs - index.runs.len()));
        Ok(Self(Arc::new(Storage {
            index,
            budget,
            _held: held,
        })))
    }

    /// Builds one compaction of this version. The returned operation splices the result
    /// into a current version if the inputs still match, preserving any later appends.
    ///
    /// A writer using `inline_only` normally builds only small windows. Once there are more
    /// than 16 runs it also takes a deferred window, bounding the backlog when the background
    /// compactor cannot keep up. If that build cannot be charged, its caller can refuse the
    /// staged version and keep the old readers' runs.
    pub fn compact(
        &self,
        policy: CompactionPolicy,
        inline_only: bool,
    ) -> Result<
        Option<impl FnOnce(&Self) -> Result<Option<Self>, BudgetExceeded> + Send + use<>>,
        BudgetExceeded,
    > {
        let Some(plan) = policy.plan(&self.0.index.runs) else {
            return Ok(None);
        };
        if inline_only && self.0.index.runs.len() <= 16 && !policy.is_inline(&plan) {
            return Ok(None);
        }
        let merged = merge_triples(&self.0.index.runs[plan.window.clone()], &self.0.budget)?;
        let captured = self.clone();
        Ok(Some(move |current: &Self| {
            let index = &current.0.index;
            if !index.retains(&captured.0.index, plan.window.clone()) {
                return Ok(None);
            }
            let len = index.runs.len() - plan.window.len() + usize::from(merged.entries() != 0);
            current
                .with_index(len, || index.with_compacted(plan.window, merged))
                .map(Some)
        }))
    }

    /// Whether the policy has a merge for this version.
    pub fn needs_compaction(&self, policy: CompactionPolicy) -> bool {
        policy.plan(&self.0.index.runs).is_some()
    }

    pub fn len(&self) -> usize {
        self.0.index.len() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
