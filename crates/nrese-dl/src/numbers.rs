//! Arithmetic over counted classes (docs/design/owl2-dl.md#number-reasoning-layers, layer 2):
//! sizes of classes that the clauses fix exactly, compared before any search. The
//! pigeonhole cases (W3C DL-906, 907, 910: integer multiplication through nominals and
//! functional properties) are hard for merges, which are resolution, and trivial for
//! arithmetic.
//!
//! A size is fixed by two patterns, over the normalised clauses:
//! - **Neighbours of a nominal:** `X ⊑ ∃S.O` and `S(x, y) ∧ O(y) → X(x)` with `O ≡ {o}`
//!   make `X` the S-predecessors of `o`; `O ⊑ (= n S⁻)` then gives `|X| = n`.
//! - **A product:** `Y ⊑ ∃q.X` and `q(y, x) ∧ X(x) → Y(y)` with `q` functional make `Y` the
//!   disjoint union of each `X`-element's q-predecessors; `X ⊑ (= m q⁻)` then gives
//!   `|Y| = m·|X|`.
//!
//! A class with two different sizes has no model, so neither has the ontology (the
//! clauses used are a subset of its axioms). Nothing here claims a model.

pub mod problem;

use hashbrown::{HashMap, HashSet};
use nrese_owl::{BodyAtom, Concept, Filler, HeadAtom, Normalised, ObjProp, Term, Var};

/// A role in a direction: the property and whether it is read inverted.
type Dir = (Term, bool);

/// Why the clauses have no model: a class with two sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refutation {
    pub why: String,
}

/// What the clauses say about sizes, read once.
#[derive(Default)]
struct Facts {
    /// Concepts equal to one nominal.
    singleton: HashMap<Concept, Term>,
    /// Named inverse pairs, both ways.
    inverse: HashMap<Term, Term>,
    /// Directions with at most one neighbour per element (functional, or over the
    /// inverse inverse-functional).
    functional: HashSet<Dir>,
    at_least: HashMap<(Concept, Dir), u64>,
    at_most: HashMap<(Concept, Dir), u64>,
    /// `X ⊑ ∃R.C` (unqualified count 1), and `R(x, y) ∧ C(y) → X(x)`.
    some: HashSet<(Concept, Dir, Concept)>,
    closed: HashSet<(Concept, Dir, Concept)>,
}

impl Facts {
    /// A direction with named inverses folded onto the smaller term.
    fn dir(&self, (t, inverted): Dir) -> Dir {
        match self.inverse.get(&t) {
            Some(&u) if u < t => (u, !inverted),
            _ => (t, inverted),
        }
    }

    fn exact(&self, c: Concept, d: Dir) -> Option<u64> {
        let d = self.dir(d);
        let n = *self.at_least.get(&(c, d))?;
        (self.at_most.get(&(c, d)) == Some(&n)).then_some(n)
    }

    /// `X ≡ ∃R.C` read both ways.
    fn defined(&self) -> impl Iterator<Item = (Concept, Dir, Concept)> + '_ {
        self.some
            .iter()
            .copied()
            .filter(|f| self.closed.contains(f))
    }
}

fn prop(r: ObjProp) -> Dir {
    match r {
        ObjProp::Named(t) => (t, false),
        ObjProp::Inverse(t) => (t, true),
    }
}

fn read(n: &Normalised) -> Facts {
    let mut f = Facts::default();
    let mut to_nominal = HashSet::new();
    let mut from_nominal = HashSet::new();
    for c in &n.clauses {
        match (&c.body[..], &c.head[..]) {
            ([BodyAtom::Nominal(o, Var::X)], [HeadAtom::Concept(k, Var::X)]) => {
                from_nominal.insert((*k, *o));
            }
            ([BodyAtom::Concept(k, Var::X)], [HeadAtom::Nominal(o, Var::X)]) => {
                to_nominal.insert((*k, *o));
            }
            ([BodyAtom::Role(a, Var::X, y)], [HeadAtom::Role(b, y2, Var::X)]) if y == y2 => {
                f.inverse.insert(*a, *b);
                f.inverse.insert(*b, *a);
            }
            ([BodyAtom::Role(a, s0, t0), BodyAtom::Role(b, s1, t1)], [HeadAtom::Equal(e0, e1)])
                if a == b =>
            {
                let ends = |y0: &Var, y1: &Var| {
                    y0 != y1 && [*e0, *e1].contains(y0) && [*e0, *e1].contains(y1)
                };
                if *s0 == Var::X && *s1 == Var::X && ends(t0, t1) {
                    f.functional.insert((*a, false));
                } else if *t0 == Var::X && *t1 == Var::X && ends(s0, s1) {
                    f.functional.insert((*a, true));
                }
            }
            (
                [BodyAtom::Concept(k, Var::X)],
                [
                    HeadAtom::AtLeast {
                        n,
                        role,
                        filler,
                        var: Var::X,
                    },
                ],
            ) => match filler {
                Filler::Top => {
                    let e = f.at_least.entry((*k, prop(*role))).or_default();
                    *e = (*e).max(u64::from(*n));
                }
                Filler::Is(d) if *n >= 1 => {
                    f.some.insert((*k, prop(*role), *d));
                }
                _ => {}
            },
            (
                [BodyAtom::Concept(k, Var::X)],
                [
                    HeadAtom::AtMost {
                        n,
                        role,
                        filler: Filler::Top,
                        var: Var::X,
                    },
                ],
            ) => {
                let e = f.at_most.entry((*k, prop(*role))).or_insert(u64::MAX);
                *e = (*e).min(u64::from(*n));
            }
            _ => {}
        }
        // `C(x) ∧ R(x, y₀) ∧ … ∧ R(x, yₙ) → yᵢ ≈ yⱼ` for every pair: `C ⊑ ≤ n R`.
        if let Some((k, d, n)) = spelled_at_most(c) {
            let e = f.at_most.entry((k, d)).or_insert(u64::MAX);
            *e = (*e).min(n);
        }
        // `R(x, y) ∧ C(y) → X(x)`, either order of the body.
        if let [HeadAtom::Concept(x, Var::X)] = c.head[..] {
            let mut role = None;
            let mut filler = None;
            for b in &c.body {
                match *b {
                    BodyAtom::Role(r, Var::X, y) if y != Var::X => role = Some(((r, false), y)),
                    BodyAtom::Role(r, y, Var::X) if y != Var::X => role = Some(((r, true), y)),
                    BodyAtom::Concept(k, y) if y != Var::X => filler = Some((k, y)),
                    _ => {
                        role = None;
                        break;
                    }
                }
            }
            if c.body.len() == 2
                && let (Some((d, y)), Some((k, y2))) = (role, filler)
                && y == y2
            {
                f.closed.insert((x, d, k));
            }
        }
    }
    for (k, o) in to_nominal {
        if from_nominal.contains(&(k, o)) {
            f.singleton.insert(k, o);
        }
    }
    // Directions folded once the inverses are known.
    let fold = |f: &Facts, m: &HashMap<(Concept, Dir), u64>| {
        m.iter()
            .map(|(&(k, d), &n)| ((k, f.dir(d)), n))
            .collect::<HashMap<_, _>>()
    };
    f.at_least = fold(&f, &f.at_least);
    f.at_most = fold(&f, &f.at_most);
    let fold3 = |f: &Facts, s: &HashSet<(Concept, Dir, Concept)>| {
        s.iter()
            .map(|&(x, d, k)| (x, f.dir(d), k))
            .collect::<HashSet<_>>()
    };
    f.functional = f.functional.iter().map(|&d| f.dir(d)).collect();
    f.some = fold3(&f, &f.some);
    f.closed = fold3(&f, &f.closed);
    f
}

/// A spelled-out at-most: `C(x)` and `n + 1` atoms `R(x, yᵢ)` (or `R(yᵢ, x)`) in the body,
/// every `yᵢ ≈ yⱼ` in the head.
fn spelled_at_most(c: &nrese_owl::Clause) -> Option<(Concept, Dir, u64)> {
    let mut guard = None;
    let mut dir = None;
    let mut ys = Vec::new();
    for b in &c.body {
        match *b {
            BodyAtom::Concept(k, Var::X) if guard.is_none() => guard = Some(k),
            BodyAtom::Role(r, Var::X, y) if y != Var::X => {
                if dir.is_some_and(|d| d != (r, false)) {
                    return None;
                }
                dir = Some((r, false));
                ys.push(y);
            }
            BodyAtom::Role(r, y, Var::X) if y != Var::X => {
                if dir.is_some_and(|d| d != (r, true)) {
                    return None;
                }
                dir = Some((r, true));
                ys.push(y);
            }
            _ => return None,
        }
    }
    let (k, d) = (guard?, dir?);
    if ys.len() < 2 || c.head.len() != ys.len() * (ys.len() - 1) / 2 {
        return None;
    }
    for h in &c.head {
        let HeadAtom::Equal(a, b) = h else {
            return None;
        };
        if a == b || !ys.contains(a) || !ys.contains(b) {
            return None;
        }
    }
    Some((k, d, ys.len() as u64 - 1))
}

/// The clauses' sizes compared: a class with two sizes refutes them.
pub fn refute(n: &Normalised) -> Option<Refutation> {
    let f = read(n);
    if f.singleton.is_empty() {
        return None;
    }
    let mut size: HashMap<Concept, u64> = HashMap::new();
    let set =
        |size: &mut HashMap<Concept, u64>, x: Concept, v: u64, how: &str| match size.insert(x, v) {
            Some(old) if old != v => Some(Refutation {
                why: format!("{x:?} has {old} elements and, {how}, {v}"),
            }),
            _ => None,
        };
    // Neighbours of a nominal.
    let defined: Vec<_> = f.defined().collect();
    for &(x, (s, inverted), o) in &defined {
        if f.singleton.contains_key(&o)
            && let Some(n) = f.exact(o, (s, !inverted))
            && let Some(r) = set(&mut size, x, n, "as the neighbours of a nominal")
        {
            return Some(r);
        }
    }
    // Products, to a fixpoint (each pass fixes a class at most once more).
    for _ in 0..defined.len() {
        let mut changed = false;
        for &(y, (q, inverted), x) in &defined {
            if !f.functional.contains(&(q, inverted)) {
                continue;
            }
            let (Some(&sx), Some(m)) = (size.get(&x), f.exact(x, (q, !inverted))) else {
                continue;
            };
            let Some(v) = sx.checked_mul(m) else {
                continue;
            };
            match size.get(&y) {
                Some(&old) if old == v => {}
                Some(&old) => {
                    return Some(Refutation {
                        why: format!(
                            "{y:?} has {old} elements and, {m} for each of {x:?}'s {sx}, {v}"
                        ),
                    });
                }
                None => {
                    size.insert(y, v);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    None
}
