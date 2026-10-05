//! Sets of texts: strings, IRIs and language-tagged strings, under length facets and
//! patterns (`xsd:pattern` on the text, `rdf:langRange` on the tag).
//!
//! A set is, per *cell*, a set of lengths: a cell is a region of the text (`text`'s chain
//! of string types; IRIs have one) and, for each pattern the set mentions, whether it
//! holds. Union, intersection and complement work per cell; the patterns of two sets are
//! aligned first, and one that no longer splits a cell is dropped after. Without patterns
//! this is the length lines per region it always was. Counting a cell with patterns
//! counts its automaton (`regular::cell`) over its lengths.
//!
//! A tagged string is a text and a tag; the tags of a cell are infinitely many or none
//! (a matching tag stays matching with a subtag more, and one that doesn't, doesn't), so a
//! cell with tagged strings has many or none.

use std::marker::PhantomData;
use std::ops::Bound;
use std::sync::Arc;

use super::line::{Line, integer_range};
use super::regular::{self, Family, Pattern, Target};
use super::set::{Base, Count, count_integers};
use super::text;

/// Patterns one set can combine at most: its cells are `2^patterns` per region. Callers
/// keep to it (the datatype theory approximates an ontology with more).
pub const MAX_PATTERNS: usize = 12;

/// A text value: a string or IRI, or a tagged string.
pub(super) trait Member: Ord + Clone + std::fmt::Debug {
    fn text(&self) -> &str;
    fn tag(&self) -> Option<&str>;
    /// The value of an untagged text (`None` for tagged strings).
    fn of_text(text: String) -> Option<Self>;
}

impl Member for String {
    fn text(&self) -> &str {
        self
    }

    fn tag(&self) -> Option<&str> {
        None
    }

    fn of_text(text: String) -> Option<Self> {
        Some(text)
    }
}

impl Member for (String, String) {
    fn text(&self) -> &str {
        &self.0
    }

    fn tag(&self) -> Option<&str> {
        Some(&self.1)
    }

    fn of_text(_: String) -> Option<Self> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Texts<V> {
    family: Family,
    /// Sorted by id; bit `i` of a cell's signature is whether `patterns[i]` holds.
    patterns: Vec<Arc<Pattern>>,
    /// `cells[signature * regions + region]`: lengths, all non-negative.
    cells: Cells,
    marker: PhantomData<V>,
}

type Cells = Vec<Line<i128>>;

fn lengths() -> Line<i128> {
    Line::interval(Bound::Included(0), Bound::Unbounded)
}

impl<V: Member> Texts<V> {
    fn regions(&self) -> usize {
        self.family.regions()
    }

    pub(super) fn none(family: Family) -> Self {
        Self::from_region(family, family.regions())
    }

    /// The regions from `floor` on, any length.
    pub(super) fn from_region(family: Family, floor: usize) -> Self {
        Self {
            family,
            patterns: Vec::new(),
            cells: (0..family.regions())
                .map(|r| if r >= floor { lengths() } else { Line::empty() })
                .collect(),
            marker: PhantomData,
        }
    }

    /// Every text with a length in `line`.
    pub(super) fn of_lengths(family: Family, line: &Line<i128>) -> Self {
        let line = line.intersection(&lengths());
        Self {
            family,
            patterns: Vec::new(),
            cells: vec![line; family.regions()],
            marker: PhantomData,
        }
    }

    /// Every text the pattern holds of.
    pub(super) fn matching(family: Family, pattern: Arc<Pattern>) -> Self {
        let regions = family.regions();
        Self {
            family,
            patterns: vec![pattern],
            cells: (0..2 * regions)
                .map(|i| {
                    if i >= regions {
                        lengths()
                    } else {
                        Line::empty()
                    }
                })
                .collect(),
            marker: PhantomData,
        }
    }

    /// The same cells for another kind of value (strings' cells for tagged strings).
    pub(super) fn retyped<W>(other: &Texts<W>) -> Self {
        Self {
            family: other.family,
            patterns: other.patterns.clone(),
            cells: other.cells.clone(),
            marker: PhantomData,
        }
    }

    /// Both sets' cells over the union of their patterns.
    fn aligned(&self, o: &Self) -> (Vec<Arc<Pattern>>, Cells, Cells) {
        if self.patterns == o.patterns {
            return (self.patterns.clone(), self.cells.clone(), o.cells.clone());
        }
        let mut patterns: Vec<Arc<Pattern>> =
            self.patterns.iter().chain(&o.patterns).cloned().collect();
        patterns.sort_by_key(|p| p.id);
        patterns.dedup_by_key(|p| p.id);
        assert!(
            patterns.len() <= MAX_PATTERNS,
            "a set of texts combines more than {MAX_PATTERNS} patterns"
        );
        let expand = |s: &Self| {
            let bits: Vec<usize> = s
                .patterns
                .iter()
                .map(|p| patterns.iter().position(|q| q.id == p.id).unwrap_or(0))
                .collect();
            let regions = self.regions();
            let mut cells = Vec::with_capacity((1 << patterns.len()) * regions);
            for signature in 0usize..1 << patterns.len() {
                let own = bits
                    .iter()
                    .enumerate()
                    .fold(0, |acc, (i, &b)| acc | ((signature >> b) & 1) << i);
                cells.extend_from_slice(&s.cells[own * regions..(own + 1) * regions]);
            }
            cells
        };
        let (a, b) = (expand(self), expand(o));
        (patterns, a, b)
    }

    fn zip(&self, o: &Self, op: impl Fn(&Line<i128>, &Line<i128>) -> Line<i128>) -> Self {
        let (patterns, a, b) = self.aligned(o);
        Self {
            family: self.family,
            patterns,
            cells: a.iter().zip(&b).map(|(x, y)| op(x, y)).collect(),
            marker: PhantomData,
        }
        .pruned()
    }

    /// Without the patterns that split no cell.
    fn pruned(mut self) -> Self {
        let regions = self.regions();
        let mut i = self.patterns.len();
        while i > 0 {
            i -= 1;
            let k = self.patterns.len();
            let bit = 1usize << i;
            let splits = (0usize..1 << k).filter(|s| s & bit == 0).any(|s| {
                self.cells[s * regions..(s + 1) * regions]
                    != self.cells[(s | bit) * regions..((s | bit) + 1) * regions]
            });
            if splits {
                continue;
            }
            let mut cells = Vec::with_capacity(self.cells.len() / 2);
            for s in (0usize..1 << k).filter(|s| s & bit == 0) {
                cells.extend_from_slice(&self.cells[s * regions..(s + 1) * regions]);
            }
            self.cells = cells;
            self.patterns.remove(i);
        }
        self
    }

    fn signature(&self, v: &V) -> usize {
        self.patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| p.matches(v.text(), v.tag()))
            .fold(0, |acc, (i, _)| acc | 1 << i)
    }

    /// For a cell: whether each pattern of `target` holds.
    fn holds(&self, signature: usize, target: Target) -> Vec<(&Arc<Pattern>, bool)> {
        self.patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| p.target == target)
            .map(|(i, p)| (p, signature >> i & 1 == 1))
            .collect()
    }

    fn tagged() -> bool {
        V::of_text(String::new()).is_none()
    }

    /// The count of a cell's texts, without patterns.
    fn plain_count(&self, region: usize, line: &Line<i128>) -> Count {
        match self.family {
            Family::Strings => count_integers(line, 0, |len| {
                let (lo, hi) = text::count(region, len as u64);
                Count { lo, hi }
            }),
            _ => count_integers(line, 0, |len| {
                if len == 0 {
                    Count::exact(1)
                } else {
                    Count {
                        lo: 1000,
                        hi: u64::MAX,
                    }
                }
            }),
        }
    }

    /// One cell's count of values: its texts, and for tagged strings as many tags.
    fn cell_count(&self, signature: usize, region: usize) -> Count {
        let line = &self.cells[signature * self.regions() + region];
        if line.is_void() {
            return Count::ZERO;
        }
        let texts = self.holds(signature, Target::Text);
        let count = if texts.is_empty() {
            self.plain_count(region, line)
        } else {
            regular::cell(self.family, region, Target::Text, &texts).count(line)
        };
        if !Self::tagged() || count.is_empty() {
            return count;
        }
        let tags = self.holds(signature, Target::Tag);
        if !tags.is_empty() && regular::cell(Family::Tags, 0, Target::Tag, &tags).is_empty() {
            return Count::ZERO;
        }
        if count.lo > 0 {
            Count::MANY
        } else {
            Count {
                lo: 0,
                hi: u64::MAX,
            }
        }
    }

    fn signatures(&self) -> std::ops::Range<usize> {
        0..1 << self.patterns.len()
    }
}

impl<V: Member> Base for Texts<V> {
    type V = V;

    fn contains(&self, v: &V) -> bool {
        let region = match self.family {
            Family::Strings => text::region(v.text()),
            _ => 0,
        };
        let length = v.text().chars().count() as i128;
        self.cells[self.signature(v) * self.regions() + region].contains(&length)
    }

    fn union(&self, o: &Self) -> Self {
        self.zip(o, Line::union)
    }

    fn intersection(&self, o: &Self) -> Self {
        self.zip(o, Line::intersection)
    }

    fn complement(&self) -> Self {
        let all = lengths();
        Self {
            family: self.family,
            patterns: self.patterns.clone(),
            cells: self
                .cells
                .iter()
                .map(|l| l.complement().intersection(&all))
                .collect(),
            marker: PhantomData,
        }
    }

    fn count(&self) -> Count {
        let mut total = Count::ZERO;
        for signature in self.signatures() {
            for region in 0..self.regions() {
                total = total.plus(self.cell_count(signature, region));
            }
        }
        total
    }

    fn values(&self, limit: u64) -> Option<Vec<V>> {
        let mut out = Vec::new();
        for signature in self.signatures() {
            for region in 0..self.regions() {
                let line = &self.cells[signature * self.regions() + region];
                if self.cell_count(signature, region).is_empty() {
                    continue;
                }
                if Self::tagged() {
                    return None;
                }
                let texts = self.holds(signature, Target::Text);
                let found = if texts.is_empty() {
                    plain_values(self.family, region, line, limit)?
                } else {
                    let room = limit.saturating_sub(out.len() as u64);
                    regular::cell(self.family, region, Target::Text, &texts).strings(line, room)?
                };
                out.extend(found.into_iter().filter_map(V::of_text));
                if out.len() as u64 > limit {
                    return None;
                }
            }
        }
        Some(out)
    }
}

/// The texts of a region with a length in `line`, where they are few and known.
fn plain_values(
    family: Family,
    region: usize,
    line: &Line<i128>,
    limit: u64,
) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for s in line.segments() {
        let (lo, hi) = integer_range(&s);
        for len in lo.unwrap_or(0).max(0)..=hi? {
            match family {
                Family::Strings => out.extend(text::strings(region, len as u64)?),
                _ if len == 0 => out.push(String::new()),
                _ => return None,
            }
            if out.len() as u64 > limit {
                return None;
            }
        }
    }
    Some(out)
}
