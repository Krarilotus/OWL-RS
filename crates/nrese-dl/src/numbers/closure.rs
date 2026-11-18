//! Layer 2 of the number module (docs/design/owl2-dl.md#number-reasoning-layers): the
//! sizes a [`NumberProblem`] fixes, closed under its equations, or a refutation. Sound
//! for any problem, complete or not: its axioms are a subset of the ontology's.
//!
//! The unknowns are class sizes and edge counts per property; each equation says
//! `a · U = b · V` (or that a size is a constant):
//! - **a singleton class** `A ≡ {a}` has one element;
//! - **the neighbours of a nominal:** `X ≡ ∃R.O` with `O ≡ {a}` and `O ⊑ (= n R⁻)` make
//!   `X` the `R`-predecessors of `a`, so `|X| = n`;
//! - **a product:** `Y ≡ ∃q.X` with `q` functional and `X ⊑ (= m q⁻)` make `Y` the
//!   disjoint union of the `q`-predecessors of each `X`-element: `|Y| = m · |X|`;
//! - **the degree sum of a property** with domain `Y` and range `X`: every edge starts in
//!   `Y` and ends in `X`, so where `Y`'s out-degree is exactly `a` and `X`'s in-degree
//!   exactly `b`, the edges number `a · |Y|` and `b · |X|`. Out-degree 1 also follows from
//!   a functional property and an existential over it.
//!
//! In W3C DL-906, 907 and 910 these give `|N| = n`, `|NM| = m · |N|` and `|NM| = k`: so
//! `k = m · n`, a degree sum over the nominal no per-node count sees. A size that two
//! equations set apart, or that an equation sets to a fraction, refutes the problem.

use hashbrown::{HashMap, HashSet};
use nrese_owl::Term;

use super::problem::{Dir, Expr, NumberProblem};

/// Why the problem has no model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refutation {
    pub why: String,
}

/// An unknown of the equations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unknown {
    Size(Term),
    /// The edges of a property (its inverse's are the same, reversed).
    Edges(Term),
}

/// `a · U = b · V` (`None` stands for the constant 1).
#[derive(Debug, Clone, Copy)]
struct Equation {
    left: (u64, Option<Unknown>),
    right: (u64, Option<Unknown>),
    why: &'static str,
}

/// The sizes and edge counts the problem fixes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub values: HashMap<Unknown, u64>,
}

impl Counts {
    pub fn size(&self, class: Term) -> Option<u64> {
        self.values.get(&Unknown::Size(class)).copied()
    }

    pub fn edges(&self, property: Term) -> Option<u64> {
        self.values.get(&Unknown::Edges(property)).copied()
    }
}

/// What the inclusions say, read once.
struct Facts {
    /// `A ≡ {a}`.
    singleton: HashMap<Term, Term>,
    /// `X ≡ ∃R.B`.
    defined: Vec<(Term, Dir, Term)>,
    /// `C ⊑ (= n R)`, and `C ⊑ ∃R.B` with `R` functional (`= 1`).
    exact: HashMap<(Term, Dir), u64>,
    domain: HashMap<Dir, Term>,
}

fn facts(p: &NumberProblem) -> Facts {
    let has = |sub: Expr, sup: Expr| p.inclusions.iter().any(|i| i.sub == sub && i.sup == sup);
    let mut f = Facts {
        singleton: HashMap::new(),
        defined: Vec::new(),
        exact: HashMap::new(),
        domain: HashMap::new(),
    };
    let mut at_least: HashMap<(Term, Dir), u64> = HashMap::new();
    let mut at_most: HashMap<(Term, Dir), u64> = HashMap::new();
    let mut seen: HashSet<(Term, Dir, Term)> = HashSet::new();
    for i in &p.inclusions {
        let Expr::Class(c) = i.sub else {
            continue;
        };
        match i.sup {
            Expr::Nominal(a) if has(Expr::Nominal(a), Expr::Class(c)) => {
                f.singleton.insert(c, a);
            }
            Expr::Some(d, Some(b)) if has(i.sup, i.sub) && seen.insert((c, d, b)) => {
                f.defined.push((c, d, b));
            }
            _ => {}
        }
        let (least, most) = match i.sup {
            Expr::Exact(n, d) => (Some((d, n)), Some((d, n))),
            Expr::AtLeast(n, d) => (Some((d, n)), None),
            Expr::AtMost(n, d) => (None, Some((d, n))),
            Expr::Some(d, _) => (Some((d, 1)), None),
            _ => (None, None),
        };
        if let Some((d, n)) = least {
            let e = at_least.entry((c, d)).or_default();
            *e = (*e).max(u64::from(n));
        }
        if let Some((d, n)) = most {
            let e = at_most.entry((c, d)).or_insert(u64::MAX);
            *e = (*e).min(u64::from(n));
        }
    }
    for &d in &p.functional {
        for (&(c, d2), _) in at_least.iter().filter(|&(&(_, d2), &n)| d2 == d && n >= 1) {
            let e = at_most.entry((c, d2)).or_insert(u64::MAX);
            *e = (*e).min(1);
        }
    }
    for (&key, &n) in &at_least {
        if at_most.get(&key) == Some(&n) {
            f.exact.insert(key, n);
        }
    }
    for &(d, c, _) in &p.domains {
        if let Some(c) = c {
            f.domain.insert(d, c);
        }
    }
    f
}

fn equations(p: &NumberProblem, f: &Facts) -> Vec<Equation> {
    let mut eqs = Vec::new();
    let flip = |(t, inverted): Dir| (t, !inverted);
    for &c in f.singleton.keys() {
        eqs.push(Equation {
            left: (1, Some(Unknown::Size(c))),
            right: (1, None),
            why: "a singleton class",
        });
    }
    for &(x, d, o) in &f.defined {
        if f.singleton.contains_key(&o)
            && let Some(&n) = f.exact.get(&(o, flip(d)))
        {
            eqs.push(Equation {
                left: (1, Some(Unknown::Size(x))),
                right: (n, None),
                why: "the neighbours of a nominal",
            });
        }
        if p.functional.contains(&d)
            && let Some(&m) = f.exact.get(&(o, flip(d)))
        {
            eqs.push(Equation {
                left: (1, Some(Unknown::Size(x))),
                right: (m, Some(Unknown::Size(o))),
                why: "a product through a functional property",
            });
        }
    }
    // Degree sums: a direction's domain and range, each with an exact degree.
    for (&d, &from) in &f.domain {
        let edges = Some(Unknown::Edges(d.0));
        if let Some(&a) = f.exact.get(&(from, d)) {
            eqs.push(Equation {
                left: (1, edges),
                right: (a, Some(Unknown::Size(from))),
                why: "a degree sum",
            });
        }
    }
    eqs
}

/// The counts `p` fixes, or why it has no model.
pub fn close(p: &NumberProblem) -> Result<Counts, Refutation> {
    let f = facts(p);
    let eqs = equations(p, &f);
    let mut counts = Counts::default();
    let value = |counts: &Counts, (a, u): (u64, Option<Unknown>)| match u {
        None => Some(a),
        Some(u) => counts.values.get(&u).and_then(|&v| a.checked_mul(v)),
    };
    let name = |u: Option<Unknown>| u.map_or_else(|| "1".to_owned(), |u| format!("{u:?}"));
    loop {
        let mut changed = false;
        for e in &eqs {
            match (value(&counts, e.left), value(&counts, e.right)) {
                (Some(l), Some(r)) if l != r => {
                    return Err(Refutation {
                        why: format!(
                            "{} · {} = {l} and {} · {} = {r} ({})",
                            e.left.0,
                            name(e.left.1),
                            e.right.0,
                            name(e.right.1),
                            e.why
                        ),
                    });
                }
                (Some(known), None) | (None, Some(known)) => {
                    let (a, u) = if value(&counts, e.left).is_none() {
                        e.left
                    } else {
                        e.right
                    };
                    let Some(u) = u else {
                        continue;
                    };
                    if a == 0 {
                        continue;
                    }
                    if known % a != 0 {
                        return Err(Refutation {
                            why: format!("{a} · {u:?} = {known} ({}): not a whole number", e.why),
                        });
                    }
                    counts.values.insert(u, known / a);
                    changed = true;
                }
                _ => {}
            }
        }
        if !changed {
            return Ok(counts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::problem::{extract, tests::product};
    use super::*;

    /// DL-906's shape closes: |N| = 20, |NM| = 600, and the edges of each property.
    #[test]
    fn the_product_of_906_closes() {
        let p = extract(&product(20, 30, 600));
        let c = close(&p).expect("consistent counts");
        assert_eq!(c.size(2), Some(20), "{c:?}");
        assert_eq!(c.size(3), Some(600), "{c:?}");
        assert_eq!(c.size(1), Some(1), "{c:?}");
    }

    /// Guard (layer 2, DL-910): `k ≠ m · n` is refuted, and so is a `k` that `m` doesn't
    /// divide where `n` isn't given (the degree sum alone).
    #[test]
    fn a_wrong_product_is_refuted() {
        assert!(close(&extract(&product(20, 30, 601))).is_err());
        assert!(close(&extract(&product(2, 3, 7))).is_err());
        assert!(close(&extract(&product(2, 3, 6))).is_ok());
        // Without `O ≡ (= n p⁻)`: |NM| = 601 and |NM| = 30 · |N| leave |N| a fraction.
        let mut o = product(20, 30, 601);
        o.axioms.remove(1);
        o.sources.remove(1);
        let err = close(&extract(&o)).expect_err("601 isn't a multiple of 30");
        assert!(err.why.contains("whole number"), "{}", err.why);
        let mut o = product(20, 30, 600);
        o.axioms.remove(1);
        o.sources.remove(1);
        assert_eq!(close(&extract(&o)).expect("600 is").size(2), Some(20));
    }
}
