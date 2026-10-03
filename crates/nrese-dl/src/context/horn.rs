//! Renaming fresh names to make clauses Horn (renamable Horn, Lewis, JACM 1978).
//!
//! The structural transformation of `nrese-owl` names a subexpression by its polarity, and
//! a few EL and Horn axioms come out with two head atoms that way: `∃R.(A ⊓ B) ⊑ C`
//! becomes `R(x, y) → Q(y) ∨ C(x)` with `Q(x) ∧ A(x) ∧ B(x) → ⊥`, and `∃R.B ⊑ C` over a
//! transitive `R` puts an automaton state `Q₀(x)` beside `C(x)`. A fresh name occurs in no
//! query, so it may stand for its complement instead: with `Q := ¬P` the clauses above
//! become `R(x, y) ∧ P(y) → C(x)` and `A(x) ∧ B(x) → P(x)`, the same entailments over the
//! named classes, and Horn.
//!
//! Which fresh names to flip is a 2-SAT problem: per clause, no two of its literals may be
//! positive after the flips (named classes, roles and existentials never flip). Flips are
//! kept to those some clause forces; only if that doesn't satisfy every clause does an
//! arbitrary solution of the 2-SAT instance decide.

use nrese_owl::{BodyAtom, Clause, Concept, HeadAtom};

/// Why the clauses can't be made Horn: a clause with two fixed head atoms, or (`None`)
/// fresh names whose polarities contradict one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotHorn {
    pub clause: Option<usize>,
}

/// A literal of the 2-SAT instance: `2v` is "flip `v`", `2v + 1` "keep `v`".
type Lit = u32;

fn flip(v: u32) -> Lit {
    2 * v
}

fn keep(v: u32) -> Lit {
    2 * v + 1
}

fn neg(l: Lit) -> Lit {
    l ^ 1
}

/// The fresh names of `fresh` count to flip so that every clause has one head atom at
/// most; `clauses` are the ones the engine reads.
pub fn renaming(clauses: &[&Clause], fresh: usize) -> Result<Vec<bool>, NotHorn> {
    let n = fresh;
    let mut graph: Vec<Vec<Lit>> = vec![Vec::new(); 2 * n];
    let mut units: Vec<Lit> = Vec::new();
    let mut constrained = false;
    for (i, clause) in clauses.iter().enumerate() {
        // The literal "this fresh atom is positive" for each fresh atom of the clause.
        let mut positive: Vec<Lit> = Vec::new();
        let mut fixed = 0usize;
        for b in &clause.body {
            if let BodyAtom::Concept(Concept::Fresh(q), _) = b {
                positive.push(flip(*q));
            }
        }
        for h in &clause.head {
            match h {
                HeadAtom::Concept(Concept::Fresh(q), _) => positive.push(keep(*q)),
                _ => fixed += 1,
            }
        }
        if fixed >= 2 {
            return Err(NotHorn { clause: Some(i) });
        }
        if positive.is_empty() || (fixed == 0 && positive.len() == 1) {
            continue;
        }
        constrained = true;
        for (j, &a) in positive.iter().enumerate() {
            if fixed == 1 {
                // ¬a: a → ¬a.
                graph[a as usize].push(neg(a));
                units.push(neg(a));
            }
            for &b in &positive[j + 1..] {
                // ¬a ∨ ¬b.
                graph[a as usize].push(neg(b));
                graph[b as usize].push(neg(a));
            }
        }
    }
    if !constrained {
        return Ok(vec![false; n]);
    }
    if let Some(assignment) = forced(&graph, &units, n) {
        return Ok(assignment);
    }
    // No renaming exists: the clauses' names contradict one another, so no single clause
    // is to blame.
    solve(&graph, n).ok_or(NotHorn { clause: None })
}

/// Keeps every name except those the unit clauses force, propagated; `None` if that
/// leaves a clause unsatisfied (then [`solve`] decides).
fn forced(graph: &[Vec<Lit>], units: &[Lit], n: usize) -> Option<Vec<bool>> {
    // value[l]: the literal is true.
    let mut value = vec![false; 2 * n];
    let mut stack: Vec<Lit> = Vec::new();
    for &u in units {
        if !value[u as usize] {
            value[u as usize] = true;
            stack.push(u);
        }
    }
    while let Some(l) = stack.pop() {
        for &m in &graph[l as usize] {
            if !value[m as usize] {
                value[m as usize] = true;
                stack.push(m);
            }
        }
    }
    // Unforced names keep their polarity.
    for v in 0..n as u32 {
        if value[flip(v) as usize] && value[keep(v) as usize] {
            return None;
        }
        if !value[flip(v) as usize] {
            value[keep(v) as usize] = true;
        }
    }
    // Every implication from a true literal must land on a true one.
    for (l, edges) in graph.iter().enumerate() {
        if value[l] && edges.iter().any(|&m| !value[m as usize]) {
            return None;
        }
    }
    Some((0..n as u32).map(|v| value[flip(v) as usize]).collect())
}

/// A solution of the 2-SAT instance by strongly connected components (Aspvall, Plass and
/// Tarjan 1979), or `None` if there is none.
fn solve(graph: &[Vec<Lit>], n: usize) -> Option<Vec<bool>> {
    let comp = components(graph);
    let mut out = Vec::with_capacity(n);
    for v in 0..n as u32 {
        let (f, k) = (comp[flip(v) as usize], comp[keep(v) as usize]);
        if f == k {
            return None;
        }
        // Tarjan numbers components in reverse topological order.
        out.push(f < k);
    }
    Some(out)
}

/// Tarjan's strongly connected components, iteratively: the component of each node, in
/// the order the components complete.
fn components(graph: &[Vec<Lit>]) -> Vec<u32> {
    const NONE: u32 = u32::MAX;
    let n = graph.len();
    let (mut index, mut low, mut comp) = (vec![NONE; n], vec![0u32; n], vec![NONE; n]);
    let (mut on_stack, mut stack) = (vec![false; n], Vec::new());
    let (mut next, mut count) = (0u32, 0u32);
    for start in 0..n {
        if index[start] != NONE {
            continue;
        }
        // (node, next edge to look at)
        let mut calls: Vec<(usize, usize)> = vec![(start, 0)];
        index[start] = next;
        low[start] = next;
        next += 1;
        stack.push(start);
        on_stack[start] = true;
        while let Some(&mut (v, ref mut e)) = calls.last_mut() {
            if let Some(&w) = graph[v].get(*e) {
                *e += 1;
                let w = w as usize;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    calls.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(parent, _)) = calls.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = count;
                    if w == v {
                        break;
                    }
                }
                count += 1;
            }
        }
    }
    comp
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_owl::{Filler, ObjProp, Var};

    fn clause(body: Vec<BodyAtom>, head: Vec<HeadAtom>) -> Clause {
        Clause {
            body,
            head,
            sources: vec![0],
            flags: Default::default(),
        }
    }

    #[test]
    fn a_name_beside_a_named_head_is_flipped_and_its_definition_follows() {
        // R(x, y) → Q0(y) ∨ C(x); Q0(x) ∧ A(x) ∧ B(x) → ⊥.
        let main = clause(
            vec![BodyAtom::Role(1, Var::X, Var::Y(0))],
            vec![
                HeadAtom::Concept(Concept::Fresh(0), Var::Y(0)),
                HeadAtom::Concept(Concept::Named(7), Var::X),
            ],
        );
        let def = clause(
            vec![
                BodyAtom::Concept(Concept::Fresh(0), Var::X),
                BodyAtom::Concept(Concept::Named(5), Var::X),
            ],
            vec![],
        );
        // An unrelated name stays.
        let other = clause(
            vec![BodyAtom::Concept(Concept::Named(5), Var::X)],
            vec![HeadAtom::Concept(Concept::Fresh(1), Var::X)],
        );
        assert_eq!(renaming(&[&main, &def, &other], 2), Ok(vec![true, false]));
    }

    #[test]
    fn automaton_states_flip_together() {
        // ⊤ → Q0 ∨ C; Q0(x) ∧ R(x, y) → Q1(y); Q1 → Q0; Q1 ∧ B → ⊥.
        let main = clause(
            vec![],
            vec![
                HeadAtom::Concept(Concept::Fresh(0), Var::X),
                HeadAtom::Concept(Concept::Named(9), Var::X),
            ],
        );
        let step = clause(
            vec![
                BodyAtom::Concept(Concept::Fresh(0), Var::X),
                BodyAtom::Role(1, Var::X, Var::Y(0)),
            ],
            vec![HeadAtom::Concept(Concept::Fresh(1), Var::Y(0))],
        );
        let loop_back = clause(
            vec![BodyAtom::Concept(Concept::Fresh(1), Var::X)],
            vec![HeadAtom::Concept(Concept::Fresh(0), Var::X)],
        );
        let end = clause(
            vec![
                BodyAtom::Concept(Concept::Fresh(1), Var::X),
                BodyAtom::Concept(Concept::Named(3), Var::X),
            ],
            vec![],
        );
        assert_eq!(
            renaming(&[&main, &step, &loop_back, &end], 2),
            Ok(vec![true, true])
        );
    }

    #[test]
    fn genuine_disjunctions_stay_non_horn() {
        let or = clause(
            vec![BodyAtom::Concept(Concept::Named(1), Var::X)],
            vec![
                HeadAtom::Concept(Concept::Named(2), Var::X),
                HeadAtom::AtLeast {
                    n: 1,
                    role: ObjProp::Named(4),
                    filler: Filler::Top,
                    var: Var::X,
                },
            ],
        );
        assert_eq!(renaming(&[&or], 0), Err(NotHorn { clause: Some(0) }));
        // A ⊑ Q ⊔ C with Q used positively elsewhere: Q ⊑ ∃R.⊤ can't flip either.
        let a = clause(
            vec![],
            vec![
                HeadAtom::Concept(Concept::Fresh(0), Var::X),
                HeadAtom::Concept(Concept::Named(2), Var::X),
            ],
        );
        let b = clause(
            vec![BodyAtom::Concept(Concept::Fresh(0), Var::X)],
            vec![HeadAtom::AtLeast {
                n: 1,
                role: ObjProp::Named(4),
                filler: Filler::Top,
                var: Var::X,
            }],
        );
        let c = clause(
            vec![],
            vec![
                HeadAtom::Concept(Concept::Fresh(0), Var::X),
                HeadAtom::Concept(Concept::Fresh(1), Var::X),
            ],
        );
        assert!(renaming(&[&a, &b], 1).is_err());
        // Two fresh heads and nothing else: one of them flips (2-SAT decides).
        let flips = renaming(&[&c], 2).expect("solvable");
        assert!(flips[0] || flips[1]);
    }
}
