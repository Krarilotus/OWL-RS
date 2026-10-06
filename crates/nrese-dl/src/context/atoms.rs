//! Context terms, atoms and the term order (Bate et al., JAIR 2018, Definitions 1 and 3).
//!
//! A context speaks about one element `x` of a canonical model, its predecessor `y` and its
//! successors `f(x)`, one per successor function `f`. Every atom of a context clause is
//! about those terms only, so an atom packs into one `u64`: hashing, ordering and
//! comparing atoms is a word operation, and no string or pointer is on any hot path.

use std::cmp::Ordering;
use std::fmt;

/// A concept of the compiled program: named classes first, then fresh names.
pub type ConceptId = u32;
/// A named object property.
pub type RoleId = u32;
/// A successor function, one per existential restriction `∃R.B` of the clauses.
pub type FuncId = u32;

/// The largest concept, role or function id an atom can hold (30 bits).
pub const MAX_ID: u32 = (1 << 30) - 1;

/// A context a-term: `x`, `y` (the predecessor) or `f(x)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CTerm(u32);

impl CTerm {
    pub const X: CTerm = CTerm(0);
    pub const Y: CTerm = CTerm(1);

    pub fn func(f: FuncId) -> CTerm {
        CTerm(f + 2)
    }

    /// The function of `f(x)`.
    pub fn as_func(self) -> Option<FuncId> {
        self.0.checked_sub(2)
    }
}

impl fmt::Debug for CTerm {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.0, self.as_func()) {
            (0, _) => write!(out, "x"),
            (1, _) => write!(out, "y"),
            (_, Some(f)) => write!(out, "f{f}(x)"),
            _ => unreachable!(),
        }
    }
}

/// What an atom is: `B(t)`, `S(x, t)` or `S(t, x)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Concept,
    /// `S(x, t)`; `S(x, x)` is always written so.
    Out,
    /// `S(t, x)` for `t ≠ x`.
    In,
}

/// A context atom: `kind` (2 bits) | predicate (30 bits) | term (32 bits). The empty head
/// `⊥` is [`Atom::BOTTOM`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Atom(u64);

impl Atom {
    /// The empty head: the clause's body is contradictory.
    pub const BOTTOM: Atom = Atom(u64::MAX);

    fn pack(kind: u64, pred: u32, term: CTerm) -> Atom {
        debug_assert!(pred <= MAX_ID);
        Atom((kind << 62) | (u64::from(pred) << 32) | u64::from(term.0))
    }

    /// `B(t)`.
    pub fn concept(c: ConceptId, t: CTerm) -> Atom {
        Atom::pack(0, c, t)
    }

    /// `S(x, t)`.
    pub fn out(r: RoleId, t: CTerm) -> Atom {
        Atom::pack(1, r, t)
    }

    /// `S(t, x)` (`S(x, x)` when `t` is `x`).
    pub fn into(r: RoleId, t: CTerm) -> Atom {
        if t == CTerm::X {
            Atom::out(r, t)
        } else {
            Atom::pack(2, r, t)
        }
    }

    pub fn of(kind: Kind, pred: u32, t: CTerm) -> Atom {
        match kind {
            Kind::Concept => Atom::concept(pred, t),
            Kind::Out => Atom::out(pred, t),
            Kind::In => Atom::into(pred, t),
        }
    }

    pub fn is_bottom(self) -> bool {
        self == Atom::BOTTOM
    }

    pub fn kind(self) -> Kind {
        match self.0 >> 62 {
            0 => Kind::Concept,
            1 => Kind::Out,
            _ => Kind::In,
        }
    }

    pub fn pred(self) -> u32 {
        ((self.0 >> 32) as u32) & MAX_ID
    }

    pub fn term(self) -> CTerm {
        CTerm(self.0 as u32)
    }

    /// The function `f` if the atom is about `f(x)`.
    pub fn func(self) -> Option<FuncId> {
        if self.is_bottom() {
            None
        } else {
            self.term().as_func()
        }
    }

    /// The atom of a successor context reached by `f`, seen from its predecessor:
    /// `σ = {x ↦ f(x), y ↦ x}` (the Pred rule). `None` where the image is no context term
    /// (`S(f(x), f(x))`, or an atom that is already about a successor).
    pub fn up(self, f: FuncId) -> Option<Atom> {
        if self.is_bottom() {
            return Some(self);
        }
        let (kind, pred, t) = (self.kind(), self.pred(), self.term());
        match (kind, t) {
            (Kind::Concept, CTerm::X) => Some(Atom::concept(pred, CTerm::func(f))),
            (Kind::Concept, CTerm::Y) => Some(Atom::concept(pred, CTerm::X)),
            // S(x, y) ↦ S(f(x), x); S(y, x) ↦ S(x, f(x)).
            (Kind::Out, CTerm::Y) => Some(Atom::into(pred, CTerm::func(f))),
            (Kind::In, CTerm::Y) => Some(Atom::out(pred, CTerm::func(f))),
            _ => None,
        }
    }

    /// The inverse of [`Atom::up`] for an atom about `f(x)`: how the successor context sees
    /// it (the Succ rule). `None` for atoms not about a successor.
    pub fn down(self) -> Option<Atom> {
        self.func()?;
        let pred = self.pred();
        Some(match self.kind() {
            Kind::Concept => Atom::concept(pred, CTerm::X),
            // S(x, f(x)) ↦ S(y, x); S(f(x), x) ↦ S(x, y).
            Kind::Out => Atom::into(pred, CTerm::Y),
            Kind::In => Atom::out(pred, CTerm::Y),
        })
    }

    /// Whether the atom mentions the predecessor `y`.
    pub fn about_y(self) -> bool {
        !self.is_bottom() && self.term() == CTerm::Y
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_bottom() {
            return write!(out, "⊥");
        }
        let (p, t) = (self.pred(), self.term());
        match self.kind() {
            Kind::Concept => write!(out, "B{p}({t:?})"),
            Kind::Out => write!(out, "S{p}(x,{t:?})"),
            Kind::In => write!(out, "S{p}({t:?},x)"),
        }
    }
}

/// The context term order (Definition 3, with the choices of the paper's §5.4): atoms
/// about a successor `f(x)` are greatest (function symbols above predicates, so that they
/// never take part in Hyper), then atoms about `x` with a named concept or a role, then
/// those with a fresh concept (fresh names smallest, §5.4), and atoms about the
/// predecessor `y` smallest of all (condition 5: the predecessor triggers `Pr(O)` are
/// below every other atom). Within a rank, the packed atom decides, so the order is total.
///
/// In the Horn stage every clause head has one literal at most, so the order selects
/// nothing yet; it is here so the head-selection points of the rules are the ones the
/// disjunctive stage uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct TermOrder {
    /// Concepts below this id are named.
    pub named: u32,
}

impl TermOrder {
    fn rank(self, a: Atom) -> u8 {
        if a.is_bottom() {
            return 0;
        }
        match a.term() {
            CTerm::Y => 0,
            t if t.as_func().is_some() => 3,
            _ if a.kind() == Kind::Concept && a.pred() >= self.named => 1,
            _ => 2,
        }
    }

    pub fn compare(self, a: Atom, b: Atom) -> Ordering {
        (self.rank(a), a).cmp(&(self.rank(b), b))
    }

    /// The maximal literal of a head (`None` for `⊥`).
    pub fn max(self, head: &[Atom]) -> Option<Atom> {
        head.iter().copied().max_by(|&a, &b| self.compare(a, b))
    }
}

/// Whether sorted `small` is a subset of sorted `big`.
/// A body's signature: one of 64 bits per atom (SatELite's): `small ⊆ big` needs
/// `sig(small) & !sig(big) == 0`, so most failing subset tests end at one AND.
pub fn signature(body: &[Atom]) -> u64 {
    body.iter().fold(0u64, |s, a| {
        s | 1u64 << (a.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58)
    })
}

pub fn is_subset(small: &[Atom], big: &[Atom]) -> bool {
    if small.len() > big.len() {
        return false;
    }
    let mut j = 0;
    for &a in small {
        while j < big.len() && big[j] < a {
            j += 1;
        }
        if j == big.len() || big[j] != a {
            return false;
        }
        j += 1;
    }
    true
}

/// The sorted union of sorted `a` and `b` into `out`.
pub fn union_into(a: &[Atom], b: &[Atom], out: &mut Vec<Atom>) {
    out.clear();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoms_pack_and_map_between_contexts() {
        let f = 7;
        let b = Atom::concept(3, CTerm::X);
        assert_eq!(b.up(f), Some(Atom::concept(3, CTerm::func(f))));
        assert_eq!(b.up(f).and_then(Atom::down), Some(b));
        let s_yx = Atom::into(5, CTerm::Y);
        assert_eq!(s_yx.up(f), Some(Atom::out(5, CTerm::func(f))));
        assert_eq!(s_yx.up(f).and_then(Atom::down), Some(s_yx));
        let s_xy = Atom::out(5, CTerm::Y);
        assert_eq!(s_xy.up(f), Some(Atom::into(5, CTerm::func(f))));
        assert_eq!(s_xy.up(f).and_then(Atom::down), Some(s_xy));
        assert_eq!(Atom::concept(3, CTerm::Y).up(f), Some(b));
        assert_eq!(Atom::out(5, CTerm::X).up(f), None);
        assert_eq!(Atom::into(5, CTerm::X), Atom::out(5, CTerm::X));
        assert_eq!(Atom::BOTTOM.up(f), Some(Atom::BOTTOM));
        assert_eq!(b.pred(), 3);
        assert_eq!(Atom::out(MAX_ID, CTerm::func(9)).pred(), MAX_ID);
    }

    #[test]
    fn the_order_puts_successors_on_top_and_the_predecessor_at_the_bottom() {
        let order = TermOrder { named: 10 };
        let succ = Atom::concept(1, CTerm::func(0));
        let named = Atom::concept(1, CTerm::X);
        let fresh = Atom::concept(11, CTerm::X);
        let pred = Atom::concept(1, CTerm::Y);
        let role = Atom::into(2, CTerm::Y);
        assert_eq!(order.max(&[pred, named, succ, fresh]), Some(succ));
        assert_eq!(order.max(&[pred, fresh]), Some(fresh));
        assert_eq!(order.max(&[pred, fresh, named]), Some(named));
        assert_eq!(order.max(&[role, pred]).map(Atom::about_y), Some(true));
        assert_eq!(order.max(&[]), None);
    }

    #[test]
    fn subsets_and_unions_of_sorted_atoms() {
        let a = Atom::concept(1, CTerm::X);
        let b = Atom::concept(2, CTerm::X);
        let c = Atom::into(1, CTerm::Y);
        assert!(is_subset(&[], &[a]));
        assert!(is_subset(&[a, c], &[a, b, c]));
        assert!(!is_subset(&[a, c], &[a, b]));
        let mut out = Vec::new();
        union_into(&[a, c], &[a, b], &mut out);
        assert_eq!(out, vec![a, b, c]);
    }
}
