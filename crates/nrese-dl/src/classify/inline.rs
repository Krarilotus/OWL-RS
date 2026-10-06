//! Fresh names eliminated by resolution before the engines see the clauses.
//!
//! The normalisation names subexpressions with fresh concepts. For `∃r.Q ⊑ A` it makes
//! `⊤ → A(x) ∨ F(x)` and `F(x) ∧ r(x, y) ∧ Q(y) → ⊥`: a disjunction at every element, where
//! HermiT's clausification has the one Horn clause `r(x, y) ∧ Q(y) → A(x)`. Both engines
//! pay for it: the hypertableau branches on it at every node, the context core's Horn
//! stage needs a renaming for it.
//!
//! **The step** (predicate elimination, as Davis–Putnam's or Ackermann's): a fresh `F`
//! with exactly one clause `P = B(x) → H ∨ F(x)` where it occurs positively, and clauses
//! `N = F(v) ∧ R → H'` where it occurs negatively (once each), is replaced by the
//! resolvents `B(v) ∧ R → H' ∨ H(v)`. Since `F` occurs negatively elsewhere, its least
//! extension `{x | B(x) ∧ ¬H(x)}` satisfies everything any extension does, and with it
//! the `N` hold exactly when the resolvents do: the clauses keep their models up to `F`.
//! A fresh name with no positive occurrence can be empty (its clauses go); one with no
//! negative occurrence can hold everywhere (its clauses go).
//!
//! **Where it applies:** `F` is a concept of the clauses only (not in a number
//! restriction's filler, an assertion or a rule); `P`'s body is concepts on `x`; if
//! some `N` has `F` on a neighbour, `P`'s other head atoms are concepts. The resolvents
//! never outnumber the clauses they replace.
//!
//! **Then the polarities** ([`rename`]): a fresh name may stand for its complement (it
//! occurs in no query), and the context core's renaming (`context::horn`, Lewis's
//! renamable Horn) finds which to flip so that no clause keeps two positive literals.
//! Here it is applied for the hypertableau too, on the clauses that can become Horn:
//! `∃R.Q ⊑ A` over a transitive `R` puts an automaton state beside `A` in an
//! everywhere-clause, which flipped is `R(x, y) ∧ P(y) → P₀(x)`, `P₀(x) → A(x)`: no choice
//! at any node. Genuinely disjunctive clauses stay disjunctive.

use std::collections::HashSet;

use nrese_owl::{BodyAtom, Clause, Concept, Filler, HeadAtom, Normalised, Var};

/// What the elimination did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inlined {
    pub eliminated: usize,
    /// Fresh names flipped to their complements.
    pub flipped: usize,
    pub clauses_before: usize,
    pub clauses_after: usize,
    /// Clauses with more than one head atom, before and after.
    pub wide_before: usize,
    pub wide_after: usize,
}

const PASSES: usize = 4;

/// `n` with the fresh names that can be eliminated eliminated.
pub fn eliminate(n: &Normalised) -> (Normalised, Inlined) {
    let wide = |cs: &[Clause]| cs.iter().filter(|c| c.head.len() > 1).count();
    let mut stats = Inlined {
        clauses_before: n.clauses.len(),
        wide_before: wide(&n.clauses),
        ..Inlined::default()
    };
    let blocked = blocked(n);
    let mut clauses = n.clauses.clone();
    for _ in 0..PASSES {
        let (next, eliminated) = pass(&clauses, &blocked, n.fresh.len());
        if eliminated == 0 {
            break;
        }
        stats.eliminated += eliminated;
        clauses = next;
    }
    let (clauses, flipped) = rename(clauses, &blocked, n.fresh.len());
    stats.flipped = flipped;
    stats.clauses_after = clauses.len();
    stats.wide_after = wide(&clauses);
    let out = Normalised {
        clauses,
        ..n.clone()
    };
    (out, stats)
}

/// `clauses` with the fresh names flipped that make the most of them Horn (none if that
/// would flip a name used outside the clauses, or if no flip exists).
fn rename(mut clauses: Vec<Clause>, blocked: &HashSet<u32>, fresh: usize) -> (Vec<Clause>, usize) {
    let fixed = |c: &Clause| {
        c.head
            .iter()
            .filter(|h| !matches!(h, HeadAtom::Concept(Concept::Fresh(_), _)))
            .count()
    };
    let can: Vec<&Clause> = clauses.iter().filter(|c| fixed(c) < 2).collect();
    let Ok(flips) = crate::context::horn::renaming(&can, fresh) else {
        return (clauses, 0);
    };
    if flips
        .iter()
        .enumerate()
        .any(|(k, &f)| f && blocked.contains(&(k as u32)))
    {
        return (clauses, 0);
    }
    let flipped = flips.iter().filter(|&&f| f).count();
    if flipped == 0 {
        return (clauses, 0);
    }
    let is = |c: &Concept| matches!(c, Concept::Fresh(k) if flips[*k as usize]);
    let swap = |f: &Filler| match f {
        Filler::Is(c) if is(c) => Filler::Not(*c),
        Filler::Not(c) if is(c) => Filler::Is(*c),
        other => *other,
    };
    for c in &mut clauses {
        let touches = c
            .body
            .iter()
            .any(|b| matches!(b, BodyAtom::Concept(k, _) if is(k)))
            || c.head.iter().any(|h| match h {
                HeadAtom::Concept(k, _) => is(k),
                HeadAtom::AtLeast { filler, .. } | HeadAtom::AtMost { filler, .. } => {
                    swap(filler) != *filler
                }
                _ => false,
            });
        if !touches {
            continue;
        }
        let mut body = Vec::new();
        let mut head = Vec::new();
        for b in &c.body {
            match b {
                BodyAtom::Concept(k, v) if is(k) => head.push(HeadAtom::Concept(*k, *v)),
                other => body.push(other.clone()),
            }
        }
        for h in &c.head {
            match h {
                HeadAtom::Concept(k, v) if is(k) => body.push(BodyAtom::Concept(*k, *v)),
                HeadAtom::AtLeast {
                    n,
                    role,
                    filler,
                    var,
                } => head.push(HeadAtom::AtLeast {
                    n: *n,
                    role: *role,
                    filler: swap(filler),
                    var: *var,
                }),
                HeadAtom::AtMost {
                    n,
                    role,
                    filler,
                    var,
                } => head.push(HeadAtom::AtMost {
                    n: *n,
                    role: *role,
                    filler: swap(filler),
                    var: *var,
                }),
                other => head.push(other.clone()),
            }
        }
        let sources = std::mem::take(&mut c.sources);
        *c = Clause::new(body, head, Vec::new());
        c.sources = sources;
    }
    (clauses, flipped)
}

/// Fresh names used outside plain concept atoms of the clauses.
fn blocked(n: &Normalised) -> HashSet<u32> {
    let mut out = HashSet::new();
    let filler = |f: &Filler, out: &mut HashSet<u32>| {
        if let Filler::Is(Concept::Fresh(k)) | Filler::Not(Concept::Fresh(k)) = f {
            out.insert(*k);
        }
    };
    for c in &n.clauses {
        for h in &c.head {
            match h {
                HeadAtom::AtLeast { filler: f, .. } | HeadAtom::AtMost { filler: f, .. } => {
                    filler(f, &mut out);
                }
                _ => {}
            }
        }
    }
    for (c, _, _) in &n.facts.concepts {
        if let Concept::Fresh(k) = c {
            out.insert(*k);
        }
    }
    for r in &n.rules {
        for b in &r.body {
            if let BodyAtom::Concept(Concept::Fresh(k), _) = b {
                out.insert(*k);
            }
        }
        for h in &r.head {
            if let HeadAtom::Concept(Concept::Fresh(k), _) = h {
                out.insert(*k);
            }
        }
    }
    out
}

/// One pass: each eligible fresh name whose clauses no other elimination of the pass
/// touched.
fn pass(clauses: &[Clause], blocked: &HashSet<u32>, fresh: usize) -> (Vec<Clause>, usize) {
    let mut pos: Vec<Vec<usize>> = vec![Vec::new(); fresh];
    let mut neg: Vec<Vec<usize>> = vec![Vec::new(); fresh];
    let mut bad: Vec<bool> = vec![false; fresh];
    for (i, c) in clauses.iter().enumerate() {
        let mut seen: Vec<u32> = Vec::new();
        for b in &c.body {
            if let BodyAtom::Concept(Concept::Fresh(k), _) = b {
                neg[*k as usize].push(i);
                seen.push(*k);
            }
        }
        for h in &c.head {
            if let HeadAtom::Concept(Concept::Fresh(k), _) = h {
                pos[*k as usize].push(i);
                seen.push(*k);
            }
        }
        seen.sort_unstable();
        for w in seen.windows(2) {
            if w[0] == w[1] {
                bad[w[0] as usize] = true;
            }
        }
    }
    let mut gone = vec![false; clauses.len()];
    let mut touched = vec![false; clauses.len()];
    let mut added: Vec<Clause> = Vec::new();
    let mut eliminated = 0;
    for k in 0..fresh {
        if bad[k] || blocked.contains(&(k as u32)) || (pos[k].is_empty() && neg[k].is_empty()) {
            continue;
        }
        let all: Vec<usize> = pos[k].iter().chain(&neg[k]).copied().collect();
        if all.iter().any(|&i| touched[i]) {
            continue;
        }
        let resolvents = if pos[k].is_empty() || neg[k].is_empty() {
            Some(Vec::new())
        } else if pos[k].len() == 1 {
            resolve(
                &clauses[pos[k][0]],
                neg[k].iter().map(|&i| &clauses[i]),
                k as u32,
            )
        } else {
            None
        };
        let Some(resolvents) = resolvents else {
            continue;
        };
        for &i in &all {
            touched[i] = true;
            gone[i] = true;
        }
        added.extend(resolvents);
        eliminated += 1;
    }
    let mut out: Vec<Clause> = clauses
        .iter()
        .zip(&gone)
        .filter(|(_, g)| !**g)
        .map(|(c, _)| c.clone())
        .collect();
    out.extend(added);
    (out, eliminated)
}

/// The resolvents of `p` (with `F(x)` in its head) with each `n` (with `F(v)` in its
/// body), or `None` where the step doesn't apply.
fn resolve<'a>(p: &Clause, ns: impl Iterator<Item = &'a Clause>, k: u32) -> Option<Vec<Clause>> {
    let f = Concept::Fresh(k);
    if !p.head.contains(&HeadAtom::Concept(f, Var::X)) {
        return None;
    }
    if !p
        .body
        .iter()
        .all(|b| matches!(b, BodyAtom::Concept(_, Var::X)))
    {
        return None;
    }
    let rest: Vec<&HeadAtom> = p
        .head
        .iter()
        .filter(|h| **h != HeadAtom::Concept(f, Var::X))
        .collect();
    let only_concepts = rest
        .iter()
        .all(|h| matches!(h, HeadAtom::Concept(_, Var::X)));
    let at_x = rest.iter().all(|h| {
        matches!(
            h,
            HeadAtom::Concept(_, Var::X)
                | HeadAtom::AtLeast { var: Var::X, .. }
                | HeadAtom::AtMost { var: Var::X, .. }
        )
    });
    let mut out = Vec::new();
    for n in ns {
        let v = n.body.iter().find_map(|b| match b {
            BodyAtom::Concept(c, v) if *c == f => Some(*v),
            _ => None,
        })?;
        if (v == Var::X && !at_x) || (v != Var::X && !only_concepts) {
            return None;
        }
        let at = |var: Var| if var == Var::X { v } else { var };
        let mut body: Vec<BodyAtom> = n
            .body
            .iter()
            .filter(|b| **b != BodyAtom::Concept(f, v))
            .cloned()
            .collect();
        body.extend(p.body.iter().map(|b| match b {
            BodyAtom::Concept(c, x) => BodyAtom::Concept(*c, at(*x)),
            other => other.clone(),
        }));
        let mut head = n.head.clone();
        for h in &rest {
            head.push(match (*h).clone() {
                HeadAtom::Concept(c, x) => HeadAtom::Concept(c, at(x)),
                HeadAtom::AtLeast {
                    n,
                    role,
                    filler,
                    var,
                } => HeadAtom::AtLeast {
                    n,
                    role,
                    filler,
                    var: at(var),
                },
                HeadAtom::AtMost {
                    n,
                    role,
                    filler,
                    var,
                } => HeadAtom::AtMost {
                    n,
                    role,
                    filler,
                    var: at(var),
                },
                other => other,
            });
        }
        // A tautology (a head atom in the body) holds anyway.
        let tautology = head.iter().any(|h| match h {
            HeadAtom::Concept(c, x) => body.contains(&BodyAtom::Concept(*c, *x)),
            _ => false,
        });
        if tautology {
            continue;
        }
        let mut c = Clause::new(body, head, Vec::new());
        c.sources = sources(&p.sources, &n.sources);
        out.push(c);
    }
    Some(out)
}

/// Each way to have both clauses: one set of each, together (at most a few).
fn sources(a: &[Vec<usize>], b: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    for x in a.iter().take(4) {
        for y in b.iter().take(4) {
            let mut s: Vec<usize> = x.iter().chain(y).copied().collect();
            s.sort_unstable();
            s.dedup();
            out.push(s);
        }
    }
    out.sort();
    out.dedup();
    out
}
