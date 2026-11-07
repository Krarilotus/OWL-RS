//! The consistency violations an evaluation finds, charged to its budget as they grow.
//!
//! A rule over independent atoms can fire n³ times on n facts each, so the report is
//! sized by the data, and is charged like any other product allocation: before it is
//! allocated, released when it drops. It costs as little as it can: one budget call per
//! chunk, never per value. One flat [`ChargedVec`] of words per rule, doubled as it fills,
//! so a violation costs a capacity compare and its words, and the budget calls grow with
//! log(violations).
//!
//! A budget without room ends the list there, and says so ([`Violations::complete`]); the
//! state still counts as inconsistent, since a refusal happens only when a violation is
//! recorded. Refusal never makes data look consistent: every reader decides through
//! [`Violations::consistent`] or [`Violations::at_least`], never through what was listed.
//! Recording keeps that itself, so the recording verbs are public (an evaluation's, and
//! measured tests'): nothing outside can mark a list complete that refused.

use std::sync::Arc;

use nrese_exec::{Budget, ChargedVec, Reservation};
use smallvec::SmallVec;

use super::ir::Violation;

/// A budget without a limit, for a report nothing charges yet.
pub(crate) fn unlimited() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// The words a group's buffer starts with.
const FIRST_WORDS: usize = 64;

/// The violations of one rule (by name and arity): `records` of `stride` words each, flat.
#[derive(Debug)]
struct Group {
    name: String,
    /// The name's bytes, charged when the group is made.
    _name: Reservation,
    stride: usize,
    /// Sections keep their order: an evaluation's violations, then the datatype checks'.
    section: u32,
    records: usize,
    words: ChargedVec<u64>,
    /// Whether `words` is sorted and without duplicates.
    compact: bool,
}

impl Group {
    fn record(&self, i: usize) -> &[u64] {
        &self.words[i * self.stride..(i + 1) * self.stride]
    }

    /// Sorts the records and removes duplicates, in place: no allocation, so it can't be
    /// refused.
    fn compact(&mut self) {
        if self.compact {
            return;
        }
        self.compact = true;
        if self.stride == 0 {
            self.records = self.records.min(1);
            return;
        }
        sort_chunks(&mut self.words, self.stride);
        let stride = self.stride;
        let mut kept = 0;
        for i in 0..self.records {
            if kept > 0
                && self.words[(kept - 1) * stride..kept * stride]
                    == self.words[i * stride..(i + 1) * stride]
            {
                continue;
            }
            if kept != i {
                self.words
                    .copy_within(i * stride..(i + 1) * stride, kept * stride);
            }
            kept += 1;
        }
        self.records = kept;
        self.words.truncate(kept * stride);
    }
}

/// Sorts `words` as chunks of `stride` words, compared as slices: a heapsort, in place.
fn sort_chunks(words: &mut [u64], stride: usize) {
    let n = words.len() / stride;
    let less = |words: &[u64], a: usize, b: usize| {
        words[a * stride..(a + 1) * stride] < words[b * stride..(b + 1) * stride]
    };
    let swap = |words: &mut [u64], a: usize, b: usize| {
        if a != b {
            let (low, high) = (a.min(b), a.max(b));
            let (left, right) = words.split_at_mut(high * stride);
            left[low * stride..(low + 1) * stride].swap_with_slice(&mut right[..stride]);
        }
    };
    let sift = |words: &mut [u64], mut root: usize, end: usize| loop {
        let mut child = 2 * root + 1;
        if child >= end {
            return;
        }
        if child + 1 < end && less(words, child, child + 1) {
            child += 1;
        }
        if !less(words, root, child) {
            return;
        }
        swap(words, root, child);
        root = child;
    };
    for root in (0..n / 2).rev() {
        sift(words, root, n);
    }
    for end in (1..n).rev() {
        swap(words, 0, end);
        sift(words, 0, end);
    }
}

/// The listed violations in order ([`Violations::iter`]): the groups of one name and
/// section (sorted next to each other by [`Violations::finish`]) merged by bindings.
#[derive(Debug)]
pub struct Iter<'v> {
    groups: &'v [Group],
    /// The groups being merged: `groups[start..end]`, one name in one section.
    start: usize,
    end: usize,
    /// The next record of each of them.
    at: SmallVec<[usize; 4]>,
}

impl<'v> Iterator for Iter<'v> {
    type Item = (&'v str, &'v [u64]);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let run = &self.groups[self.start..self.end];
            let mut best: Option<(usize, &'v [u64])> = None;
            for (i, group) in run.iter().enumerate() {
                if self.at[i] < group.records {
                    let record = group.record(self.at[i]);
                    if best.is_none_or(|(_, smallest)| record < smallest) {
                        best = Some((i, record));
                    }
                }
            }
            if let Some((i, record)) = best {
                self.at[i] += 1;
                return Some((run[i].name.as_str(), record));
            }
            // The run is done: the next one starts where it ended.
            self.start = self.end;
            let first = self.groups.get(self.start)?;
            self.end = self.start
                + self.groups[self.start..]
                    .iter()
                    .take_while(|g| g.section == first.section && g.name == first.name)
                    .count();
            self.at.clear();
            self.at.resize(self.end - self.start, 0);
        }
    }
}

/// Which group a rule records into ([`Violations::group`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupId(usize);

/// Consistency violations, charged to a budget as they grow. Sorted by rule name, then
/// bindings, within each section once [`Violations::finish`]ed; the datatype checks'
/// section ([`Violations::append`]) follows the evaluation's.
#[derive(Debug)]
pub struct Violations {
    budget: Arc<Budget>,
    groups: ChargedVec<Group>,
    section: u32,
    refused: bool,
    /// [`Violations::finish`] reordered the groups: no [`GroupId`] made before holds.
    finished: bool,
}

impl Violations {
    /// An empty report charged to `budget`: the operation's own part, since a refusal
    /// marks it ([`Budget::refused`]).
    pub fn new(budget: Arc<Budget>) -> Self {
        Self {
            groups: ChargedVec::new(Arc::clone(&budget)),
            budget,
            section: 0,
            refused: false,
            finished: false,
        }
    }

    /// An empty report without a limit: for oracles, tests and callers that charge nothing.
    pub fn unlimited() -> Self {
        Self::new(unlimited())
    }

    /// The report's budget, for a later section ([`Violations::append`]).
    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    /// The group of the rule `name` with `stride` variables, made if new; `None` once the
    /// report has refused (it records nothing more). Not after [`Violations::finish`].
    pub fn group(&mut self, name: &str, stride: usize) -> Option<GroupId> {
        assert!(!self.finished, "a finished list records nothing more");
        if self.refused {
            return None;
        }
        if let Some(i) = self
            .groups
            .iter()
            .position(|g| g.section == self.section && g.stride == stride && g.name == name)
        {
            return Some(GroupId(i));
        }
        let made = (|| {
            let mut charge = Reservation::new(Arc::clone(&self.budget));
            charge.charge(name.len()).ok()?;
            self.groups.reserve(1).ok()?;
            Some(Group {
                name: name.to_owned(),
                _name: charge,
                stride,
                section: self.section,
                records: 0,
                words: ChargedVec::new(Arc::clone(&self.budget)),
                compact: true,
            })
        })();
        match made {
            Some(group) => {
                self.groups.push(group).expect("room reserved");
                Some(GroupId(self.groups.len() - 1))
            }
            None => {
                self.refused = true;
                None
            }
        }
    }

    /// Records a violation of `group` with `bindings` (its group's stride of them); `false`
    /// once the report has refused, this one included. Not after [`Violations::finish`].
    pub fn record(&mut self, group: GroupId, bindings: impl IntoIterator<Item = u64>) -> bool {
        assert!(!self.finished, "a finished list records nothing more");
        if self.refused {
            return false;
        }
        let g = &mut self.groups[group.0];
        if g.words.len() + g.stride > g.words.capacity() && !grow(g) {
            self.refused = true;
            return false;
        }
        let start = g.words.len();
        for word in bindings.into_iter().take(g.stride) {
            g.words.push(word).expect("room reserved");
        }
        debug_assert_eq!(
            g.words.len(),
            start + g.stride,
            "a record has its stride's words"
        );
        g.records += 1;
        g.compact = false;
        true
    }

    /// Sorts each section by rule name and each group's records, without duplicates; in
    /// place, so it can't be refused.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        for group in self.groups.iter_mut() {
            group.compact();
        }
        self.groups.sort_unstable_by(|a, b| {
            (a.section, &a.name, a.stride).cmp(&(b.section, &b.name, b.stride))
        });
    }

    /// `later` as the next section, after this report's (the datatype checks after an
    /// evaluation's); both are finished first.
    pub fn append(&mut self, mut later: Violations) {
        self.finish();
        later.finish();
        let section = self.groups.iter().map(|g| g.section + 1).max().unwrap_or(0);
        self.refused |= later.refused;
        // The groups' buffer is freed before its charge is released (its holder drops
        // last).
        let (groups, _held) = later.groups.into_parts();
        if self.groups.reserve(groups.len()).is_err() {
            // The later section's groups can't be listed: its violations still count.
            self.refused |= groups.iter().any(|g| g.records > 0);
            drop(groups);
            return;
        }
        for mut group in groups {
            group.section += section;
            self.groups.push(group).expect("room reserved");
        }
        self.section = section;
    }

    /// No violation found, and none missed.
    pub fn consistent(&self) -> bool {
        !self.refused && self.groups.iter().all(|g| g.records == 0)
    }

    /// Every violation is listed: the budget had room for all of them.
    pub fn complete(&self) -> bool {
        !self.refused
    }

    /// How many distinct violations there are at least (finished): exactly that many when
    /// [`Self::complete`], else a lower bound, at least 1. 0 exactly when consistent.
    pub fn at_least(&self) -> usize {
        let listed = self.listed();
        if self.refused { listed.max(1) } else { listed }
    }

    /// How many are listed.
    fn listed(&self) -> usize {
        self.groups.iter().map(|g| g.records).sum()
    }

    /// The listed violations, in order: each rule's name and bindings, by name and then
    /// bindings within a section. One name can come with several widths (grounded
    /// instances of one rule have different variable counts): their groups are merged by
    /// bindings, so `[2] < [2, 2] < [2, 4] < [4]` as the bindings' vectors compare.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            groups: &self.groups,
            start: 0,
            end: 0,
            at: SmallVec::new(),
        }
    }

    /// The first listed violation, owned (for its explanation).
    pub fn first(&self) -> Option<Violation> {
        self.iter().next().map(|(rule, bindings)| Violation {
            rule: rule.to_owned(),
            bindings: bindings.to_vec(),
        })
    }

    /// The listed violations, owned: for oracles and tests.
    pub fn to_vec(&self) -> Vec<Violation> {
        self.iter()
            .map(|(rule, bindings)| Violation {
                rule: rule.to_owned(),
                bindings: bindings.to_vec(),
            })
            .collect()
    }
}

impl Default for Violations {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Two reports list the same violations and say the same about completeness.
impl PartialEq for Violations {
    fn eq(&self, other: &Self) -> bool {
        self.refused == other.refused && self.iter().eq(other.iter())
    }
}

/// Room for `g`'s next record: its buffer doubled where the budget has room; else its
/// duplicates removed, which goes on only if that freed at least half its capacity, so a
/// compaction is followed by at least that many records and never repeats per record.
fn grow(g: &mut Group) -> bool {
    let (len, capacity) = (g.words.len(), g.words.capacity());
    let wanted = (2 * capacity).max(len + g.stride).max(FIRST_WORDS);
    if g.words.reserve_exact(wanted - len).is_ok() {
        return true;
    }
    g.compact();
    g.words.len() <= capacity / 2 && g.words.len() + g.stride <= capacity
}
