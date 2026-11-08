//! A round's derivations as an evaluation collects them (SYN-21, B-703): each charged to
//! the evaluation's working budget before it is allocated, and each (fact, set) kept once
//! as they come, so a round holds its facts' distinct sets instead of one entry per rule
//! instance (a complete transitive relation of n² facts has n³ instances and a few distinct
//! sets per fact).
//!
//! Exact: [`super::minimal`] sees every distinct set it saw before, the copies of a set
//! found more than once being one, and a set's marks become one (a fact's are only ever read
//! together: whether any of its derivations is marked). Nothing is capped or minimised here:
//! a fact's sets are all kept until `minimal` chooses among them, so a smaller set found
//! later still removes larger ones found first (at cap 2, A={1,2}, B={1,3}, C={4,5,6},
//! then D={1}: D and C, the B16 review's example).
//!
//! Not an antichain reduction: with every set kept and all of a fact's sets minimal (a
//! product of two graph families), comparing each set with the minimal ones kept is
//! quadratic per fact for nothing, nine times the round's time (timing
//! abba-run-0982b795-43be5b97, support-product-1024, 7 Nov 2026).

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
use rayon::slice::ParallelSliceMut;

use super::GraphSet;
use crate::ir::Triple;

/// Entries a buffer takes before its first reduction; after one, twice what it kept.
const REDUCE_AT: usize = 1 << 16;

/// Entries past which a reduction sorts them in parallel (a round's merged buffer).
const PARALLEL_SORT: usize = 1 << 14;

/// One derivation: the fact, one of its sets, and whether that set may lack one the cap
/// left out.
pub(super) type Entry = (Triple, GraphSet, bool);

/// A task's derivations, charged before they are allocated, each (fact, set) kept once.
pub(super) struct Found<'a> {
    entries: ChargedVec<Entry, &'a Budget>,
    /// The words of the entries' sets past 128 graphs: charged before a set is copied in.
    words: Reservation<&'a Budget>,
    /// The length at which the next reduction runs: twice what the last one kept, so
    /// reductions cost O(n log n) over a task, however many entries repeat.
    reduce_at: usize,
    /// What both are charged to, borrowed: a task's buffer takes no reference count (one
    /// shared atomic per fact otherwise, on the update path). Last: fields drop in order, the
    /// items before their charges.
    budget: &'a Budget,
}

impl<'a> Found<'a> {
    pub(super) fn new(budget: &'a Budget) -> Self {
        Self {
            entries: ChargedVec::new(budget),
            words: Reservation::new(budget),
            reduce_at: REDUCE_AT,
            budget,
        }
    }

    /// `set` found for `fact`, or `Err` without allocating where the budget has no room.
    pub(super) fn add(
        &mut self,
        fact: Triple,
        set: &GraphSet,
        marked: bool,
    ) -> Result<(), BudgetExceeded> {
        if self.entries.len() >= self.reduce_at {
            self.reduce();
        }
        self.entries.reserve(1)?;
        // The copy's words, past 128 graphs: at most the original's, charged first, then
        // trimmed to what the copy holds.
        // Most sets (up to 128 graphs) hold nothing on the heap: no charge, so no shared
        // atomic per derivation (timing abba-run-bbd88634-72df7d2b, 7 Nov).
        let bound = set.heap_bytes();
        if bound == 0 {
            self.entries
                .push((fact, set.clone(), marked))
                .expect("room reserved");
            return Ok(());
        }
        self.words.charge(bound)?;
        let copy = set.clone();
        debug_assert!(
            copy.heap_bytes() <= bound,
            "a copy holds no more than its original"
        );
        self.words.release(bound.saturating_sub(copy.heap_bytes()));
        self.entries
            .push((fact, copy, marked))
            .expect("room reserved");
        Ok(())
    }

    /// Every set of `sets` found for `fact`.
    pub(super) fn add_all<'s>(
        &mut self,
        fact: Triple,
        sets: impl IntoIterator<Item = &'s GraphSet>,
        marked: bool,
    ) -> Result<(), BudgetExceeded> {
        sets.into_iter()
            .try_for_each(|set| self.add(fact, set, marked))
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Room for `additional` more entries, charged first.
    pub(super) fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        self.entries.reserve_exact(additional)
    }

    /// The bytes it holds charged.
    #[cfg(test)]
    pub(super) fn charged(&self) -> usize {
        self.entries.charged() + self.words.bytes()
    }

    /// Reduced: sorted by fact, then set, each (fact, set) once, with their charges.
    pub(super) fn finish(mut self) -> Self {
        self.reduce();
        self
    }

    /// The entries, in the order they came if it isn't [`Self::finish`]ed.
    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Moves `other`'s entries in after its own (room charged first, beside `other`'s
    /// until it drops), with the charge of their words, which is handed over, not taken
    /// again: it lives as long as the words do (the budget owner, 2 Nov).
    pub(super) fn append(&mut self, other: Found<'a>) -> Result<(), BudgetExceeded> {
        self.entries.reserve(other.entries.len())?;
        let Found { entries, words, .. } = other;
        // `Reservation::absorb` once the memory stack lands (and a per-task `Grant` for the
        // words, `into_reservation` at the task's end); until then the same hand-over.
        let mine = std::mem::replace(&mut self.words, Reservation::new(self.budget));
        self.words = Reservation::adopt(self.budget, mine.transfer() + words.transfer());
        let (items, held) = entries.into_parts();
        self.extend_within(items);
        drop(held);
        Ok(())
    }

    /// `items` appended within the room reserved for them.
    fn extend_within(&mut self, items: Vec<Entry>) {
        let n = items.len();
        self.entries
            .fill(n, |room| items.into_iter().for_each(|item| room.push(item)))
            .expect("room reserved");
    }

    /// Sorts by (fact, set) and keeps each (fact, set) once, its marks in one, in place;
    /// the dropped copies' words released.
    fn reduce(&mut self) {
        let entries = &mut self.entries[..];
        let key = |a: &Entry, b: &Entry| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1));
        match entries.len() > PARALLEL_SORT {
            true => entries.par_sort_unstable_by(key),
            false => entries.sort_unstable_by(key),
        }
        let mut write = 0;
        for i in 0..entries.len() {
            if write > 0
                && entries[write - 1].0 == entries[i].0
                && entries[write - 1].1 == entries[i].1
            {
                entries[write - 1].2 |= entries[i].2;
                continue;
            }
            entries.swap(write, i);
            write += 1;
        }
        let dropped: usize = entries[write..].iter().map(|e| e.1.heap_bytes()).sum();
        self.entries.truncate(write);
        self.words.release(dropped);
        self.reduce_at = REDUCE_AT.max(2 * write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn set(graphs: &[u32]) -> GraphSet {
        graphs
            .iter()
            .fold(GraphSet::default(), |s, &g| s.union(&GraphSet::of(g)))
    }

    fn budget() -> Arc<Budget> {
        Arc::new(Budget::unlimited())
    }

    /// The sets kept for `fact` after `chunks` are found in that order (each its own
    /// task's, reduced at every entry), and merged with `kept` by `minimal`.
    fn through_found(
        kept: &[GraphSet],
        chunks: &[Vec<(GraphSet, bool)>],
        cap: usize,
        own: &dyn Fn(&GraphSet) -> bool,
    ) -> super::super::Annotation {
        let budget = budget();
        let mut round = Found::new(&budget);
        for chunk in chunks {
            let mut task = Found::new(&budget);
            for (s, marked) in chunk {
                task.add([1, 2, 3], s, *marked).unwrap();
                task.reduce();
            }
            round.append(task.finish()).unwrap();
        }
        let round = round.finish();
        merged(
            kept,
            round.entries().iter().map(|e| (e.1.clone(), e.2)),
            cap,
            own,
        )
    }

    /// `kept` and `found` merged as a round merges a fact's derivations.
    fn merged(
        kept: &[GraphSet],
        found: impl Iterator<Item = (GraphSet, bool)>,
        cap: usize,
        own: &dyn Fn(&GraphSet) -> bool,
    ) -> super::super::Annotation {
        let mut all = kept.to_vec();
        let mut marks = false;
        for (s, marked) in found {
            marks |= marked;
            all.push(s);
        }
        let mut next = super::super::minimal(all, cap, own);
        next.truncated |= marks;
        next.beyond_own |= marks;
        next
    }

    /// The B16 review's example (notes/2026-10-26-codebase-review/B/B16-reasoner-audit.md,
    /// B-703): at cap 2, A={1,2}, B={1,3}, C={4,5,6} found before D={1}. Capping as they
    /// come would keep A and B and lose C; minimising them all keeps D and C, and so must
    /// the reduction, with D in a later task and with all four in one task reduced at
    /// every entry.
    #[test]
    fn reduction_keeps_the_b16_selection() {
        let (a, b, c, d) = (set(&[1, 2]), set(&[1, 3]), set(&[4, 5, 6]), set(&[1]));
        let none = |_: &GraphSet| false;
        let expected = vec![d.clone(), c.clone()];
        let split = [
            vec![(a.clone(), false), (b.clone(), false), (c.clone(), false)],
            vec![(d.clone(), false)],
        ];
        let one = [vec![(a, false), (b, false), (c, false), (d, false)]];
        for chunks in [&split[..], &one[..]] {
            let got = through_found(&[], chunks, 2, &none);
            assert_eq!(got.sets, expected);
            assert!(!got.truncated && !got.beyond_own);
        }
    }

    /// Against `minimal` over everything found, marks included: random families of sets
    /// over five graphs, split into random tasks, reduced at random points, beside random
    /// kept sets, caps 1 to 3, a random set of the fact's own graphs.
    #[test]
    fn reduction_equals_minimal() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for _ in 0..20_000 {
            let random_set = |next: &mut dyn FnMut(u64) -> u64| {
                let bits = next(32) as u32;
                set(&(0..5).filter(|g| bits & (1 << g) != 0).collect::<Vec<_>>())
            };
            let kept: Vec<GraphSet> = (0..next(3)).map(|_| random_set(&mut next)).collect();
            let found: Vec<(GraphSet, bool)> = (0..next(12))
                .map(|_| (random_set(&mut next), next(8) == 0))
                .collect();
            let own_graphs = next(32) as u32;
            let own = move |s: &GraphSet| {
                let mut g = s.graphs();
                matches!((g.next(), g.next()), (Some(x), None) if own_graphs & (1 << x) != 0)
            };
            let cap = 1 + next(3) as usize;
            let expected = merged(&kept, found.iter().cloned(), cap, &own);
            // Random tasks, each reduced at random points.
            let budget = budget();
            let mut round = Found::new(&budget);
            let mut task = Found::new(&budget);
            for (s, marked) in &found {
                task.add([7, 8, 9], s, *marked).unwrap();
                if next(3) == 0 {
                    task.reduce();
                }
                if next(4) == 0 {
                    round
                        .append(std::mem::replace(&mut task, Found::new(&budget)).finish())
                        .unwrap();
                }
            }
            round.append(task.finish()).unwrap();
            let round = round.finish();
            let got = merged(
                &kept,
                round.entries().iter().map(|e| (e.1.clone(), e.2)),
                cap,
                &own,
            );
            assert_eq!(got, expected, "kept {kept:?} found {found:?} cap {cap}");
        }
    }

    /// Facts stay apart; a set found again for the same fact is kept once, its marks in
    /// one, and the copy dropped releases its words' charge; nothing is held once it drops.
    #[test]
    fn reduction_is_per_fact_and_releases_what_it_drops() {
        let budget = Arc::new(Budget::new(1 << 20));
        // Past 128 graphs: their words are charged. `wider` is found twice per fact: one
        // copy dropped.
        let (wide, wider) = (set(&[1, 200]), set(&[1, 2, 200]));
        let mut found = Found::new(&budget);
        for fact in [[1, 1, 1], [2, 2, 2]] {
            found.add(fact, &wider, false).unwrap();
            found.add(fact, &wide, false).unwrap();
            found.add(fact, &set(&[1]), true).unwrap();
            found.add(fact, &wider, true).unwrap();
        }
        assert_eq!(
            found.words.bytes(),
            2 * (wide.heap_bytes() + 2 * wider.heap_bytes())
        );
        let found = found.finish();
        let kept: Vec<_> = found
            .entries()
            .iter()
            .map(|e| (e.0, e.1.clone(), e.2))
            .collect();
        let mut expected = vec![
            ([1, 1, 1], set(&[1]), true),
            ([1, 1, 1], wide.clone(), false),
            ([1, 1, 1], wider.clone(), true),
            ([2, 2, 2], set(&[1]), true),
            ([2, 2, 2], wide.clone(), false),
            ([2, 2, 2], wider.clone(), true),
        ];
        expected.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        assert_eq!(kept, expected);
        assert_eq!(
            found.words.bytes(),
            2 * (wide.heap_bytes() + wider.heap_bytes())
        );
        assert_eq!(budget.used(), found.charged());
        drop(found);
        assert_eq!(budget.used(), 0);
    }

    /// The budget owner's row (2 Nov): a round merged from tasks, whose reduction drops
    /// spilled copies, ends charged for what it keeps (its entries' room and the kept sets'
    /// words), not for what the tasks found; the words' charge moves with them, never taken
    /// twice, and goes when they do.
    #[test]
    fn a_merged_round_is_charged_for_what_it_keeps() {
        let budget = Arc::new(Budget::new(1 << 20));
        let (wide, wider) = (set(&[1, 200]), set(&[1, 2, 200]));
        let task = |sets: &[&GraphSet]| {
            let mut task = Found::new(&budget);
            for s in sets {
                task.add([1, 1, 1], s, false).unwrap();
            }
            task.finish()
        };
        // `wider` found by both tasks: the round keeps it once.
        let (first, second) = (task(&[&wider]), task(&[&wide, &wider, &set(&[1])]));
        let found = first.words.bytes() + second.words.bytes();
        assert_eq!(found, wide.heap_bytes() + 2 * wider.heap_bytes());
        let mut round = Found::new(&budget);
        round.reserve(first.len() + second.len()).unwrap();
        round.append(first).unwrap();
        round.append(second).unwrap();
        // Handed over, not taken again: what the tasks held, and the round's room.
        assert_eq!(round.words.bytes(), found);
        assert_eq!(budget.used(), round.charged());
        let round = round.finish();
        assert_eq!(round.len(), 3, "{{1}}, {{1,200}} and {{1,2,200}} once");
        assert_eq!(round.words.bytes(), wide.heap_bytes() + wider.heap_bytes());
        assert_eq!(budget.used(), round.charged());
        drop(round);
        assert_eq!(budget.used(), 0);
    }

    /// Where the budget has no room, `add` fails before it allocates, and holds what it held.
    #[test]
    fn a_refused_entry_is_not_added() {
        let budget = Arc::new(Budget::new(size_of::<Entry>() * 4));
        let mut found = Found::new(&budget);
        let mut added = 0;
        while found.add([added, 0, 0], &set(&[1]), false).is_ok() {
            added += 1;
        }
        assert!((1..=4).contains(&added), "{added}");
        assert_eq!(found.len(), added as usize);
        assert!(budget.used() <= budget.limit());
    }
}
