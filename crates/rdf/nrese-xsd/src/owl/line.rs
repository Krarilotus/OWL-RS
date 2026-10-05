//! Sets of points on an ordered line as finite unions of intervals: what facets carve out
//! of an ordered value space (numbers, floating-point ordinals, instants, lengths).
//!
//! A set is its membership at −∞ and the sorted cuts where membership flips. A cut sits
//! just before a point or just after it, so `[a, b]` is the cuts (before a, after b) and
//! `(a, b)` is (after a, before b); a point is in the set iff the number of cuts at or
//! before "just before it" is odd, flipped by the start. Complement flips the start;
//! union and intersection are one merge of the two cut lists. The form is canonical: no
//! two cuts are equal.

use std::ops::Bound;

/// A place between points: just before `at`, or just after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cut<P> {
    pub at: P,
    pub after: bool,
}

/// A set of points of an ordered line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Line<P> {
    /// Membership below every cut (at −∞).
    start: bool,
    cuts: Vec<Cut<P>>,
}

/// One maximal interval of a line: from its lower cut (`None`: from −∞) to its upper cut
/// (`None`: to +∞).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a, P> {
    pub from: Option<&'a Cut<P>>,
    pub to: Option<&'a Cut<P>>,
}

impl<P> Segment<'_, P> {
    /// The single point it is, if it is one (from just before `p` to just after it).
    pub fn point(&self) -> Option<&P>
    where
        P: PartialEq,
    {
        match (self.from, self.to) {
            (Some(a), Some(b)) if !a.after && b.after && a.at == b.at => Some(&a.at),
            _ => None,
        }
    }
}

impl<P: Ord + Clone> Line<P> {
    pub fn empty() -> Self {
        Self {
            start: false,
            cuts: Vec::new(),
        }
    }

    pub fn full() -> Self {
        Self {
            start: true,
            cuts: Vec::new(),
        }
    }

    /// The interval between two bounds (empty if they leave nothing between them).
    pub fn interval(lo: Bound<P>, hi: Bound<P>) -> Self {
        let lo = match lo {
            Bound::Unbounded => None,
            Bound::Included(p) => Some(Cut {
                at: p,
                after: false,
            }),
            Bound::Excluded(p) => Some(Cut { at: p, after: true }),
        };
        let hi = match hi {
            Bound::Unbounded => None,
            Bound::Included(p) => Some(Cut { at: p, after: true }),
            Bound::Excluded(p) => Some(Cut {
                at: p,
                after: false,
            }),
        };
        match (lo, hi) {
            (None, None) => Self::full(),
            (None, Some(h)) => Self {
                start: true,
                cuts: vec![h],
            },
            (Some(l), None) => Self {
                start: false,
                cuts: vec![l],
            },
            (Some(l), Some(h)) if l < h => Self {
                start: false,
                cuts: vec![l, h],
            },
            _ => Self::empty(),
        }
    }

    /// `{p}`.
    pub fn point(p: P) -> Self {
        Self::interval(Bound::Included(p.clone()), Bound::Included(p))
    }

    /// Whether it has no interval at all (on a discrete line a nonempty interval may
    /// still hold no point: count those with [`Line::segments`]).
    pub fn is_void(&self) -> bool {
        !self.start && self.cuts.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.start && self.cuts.is_empty()
    }

    pub fn contains(&self, p: &P) -> bool {
        let before = self.cuts.partition_point(|c| {
            c.at < *p || (c.at == *p && !c.after) // c ≤ just before p
        });
        self.start ^ (before % 2 == 1)
    }

    pub fn complement(&self) -> Self {
        Self {
            start: !self.start,
            cuts: self.cuts.clone(),
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        self.combine(other, |a, b| a || b)
    }

    pub fn intersection(&self, other: &Self) -> Self {
        self.combine(other, |a, b| a && b)
    }

    fn combine(&self, other: &Self, op: impl Fn(bool, bool) -> bool) -> Self {
        let (mut a, mut b) = (self.start, other.start);
        let start = op(a, b);
        let mut state = start;
        let mut cuts = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < self.cuts.len() || j < other.cuts.len() {
            let next = match (self.cuts.get(i), other.cuts.get(j)) {
                (Some(x), Some(y)) if x == y => {
                    a = !a;
                    b = !b;
                    i += 1;
                    j += 1;
                    x
                }
                (Some(x), Some(y)) if x < y => {
                    a = !a;
                    i += 1;
                    x
                }
                (Some(_), Some(y)) => {
                    b = !b;
                    j += 1;
                    y
                }
                (Some(x), None) => {
                    a = !a;
                    i += 1;
                    x
                }
                (None, Some(y)) => {
                    b = !b;
                    j += 1;
                    y
                }
                (None, None) => unreachable!(),
            };
            let now = op(a, b);
            if now != state {
                cuts.push(next.clone());
                state = now;
            }
        }
        Self { start, cuts }
    }

    /// The maximal intervals, in order.
    pub fn segments(&self) -> Vec<Segment<'_, P>> {
        let mut out = Vec::new();
        let mut open: Option<Option<&Cut<P>>> = self.start.then_some(None);
        for c in &self.cuts {
            match open.take() {
                Some(from) => out.push(Segment { from, to: Some(c) }),
                None => open = Some(Some(c)),
            }
        }
        if let Some(from) = open {
            out.push(Segment { from, to: None });
        }
        out
    }
}

/// The closed integer range `[lo, hi]` of an interval of a line over integers (`None`
/// bounds: unbounded), empty when `lo > hi`.
pub fn integer_range(segment: &Segment<'_, i128>) -> (Option<i128>, Option<i128>) {
    let lo = segment.from.map(|c| {
        if c.after {
            c.at.saturating_add(1)
        } else {
            c.at
        }
    });
    let hi = segment.to.map(|c| {
        if c.after {
            c.at
        } else {
            c.at.saturating_sub(1)
        }
    });
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Bound::{Excluded as E, Included as I, Unbounded as U};

    fn members(l: &Line<i32>) -> Vec<i32> {
        (-10..=10).filter(|p| l.contains(p)).collect()
    }

    #[test]
    fn intervals_and_operations_agree_with_membership() {
        let a = Line::interval(I(-2), E(3));
        assert_eq!(members(&a), vec![-2, -1, 0, 1, 2]);
        let b = Line::interval(E(0), U);
        assert_eq!(members(&b), (1..=10).collect::<Vec<_>>());
        assert!(Line::interval(E(1), E(1)).is_void());
        assert!(Line::interval(I(2), I(1)).is_void());
        assert_eq!(members(&Line::point(4)), vec![4]);
        // Brute force over every pair of small intervals.
        let bounds = |v: i32| [U, I(v), E(v)];
        let mut lines = Vec::new();
        for lo in -3..=3 {
            for hi in -3..=3 {
                for l in bounds(lo) {
                    for h in bounds(hi) {
                        lines.push(Line::interval(l, h));
                    }
                }
            }
        }
        for x in lines.iter().step_by(5) {
            for y in lines.iter().step_by(7) {
                let u = x.union(y);
                let i = x.intersection(y);
                let c = x.complement();
                for p in -10..=10 {
                    assert_eq!(u.contains(&p), x.contains(&p) || y.contains(&p));
                    assert_eq!(i.contains(&p), x.contains(&p) && y.contains(&p));
                    assert_eq!(c.contains(&p), !x.contains(&p));
                }
                // Canonical: equal sets, equal forms.
                assert_eq!(x.union(y), y.union(x));
                assert_eq!(x.intersection(&x.complement()), Line::empty());
                assert_eq!(x.union(&x.complement()), Line::full());
            }
        }
    }

    #[test]
    fn segments_and_points() {
        let l = Line::point(2).union(&Line::interval(I(5), U));
        let s = l.segments();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].point(), Some(&2));
        assert_eq!(s[1].point(), None);
        assert!(s[1].to.is_none());
        let ints: Line<i128> = Line::interval(E(1), E(4));
        let seg = ints.segments();
        assert_eq!(integer_range(&seg[0]), (Some(2), Some(3)));
    }
}
