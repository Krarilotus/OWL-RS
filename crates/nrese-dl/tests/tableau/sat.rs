//! Propositional satisfiability through nominals (the W3C test WebOnt-description-logic-501):
//! `TorF ≡ {T, F} ≡ {plusᵢ, minusᵢ}`, `T ≠ F`, and each clause of a 3-SAT instance as
//! `T : {l₁, l₂, l₃}`. Variable `i` is true iff `plusᵢ = T`.

use super::build::Build;
use nrese_dl::tableau::{Answer, Config, consistency};
use nrese_owl::{ClassExpr, Ontology, Term};

/// The clauses of WebOnt-description-logic-501 (satisfiable: x2, x6, x7, x8 true, x1, x3,
/// x5, x9 false, x4 either way).
pub const DL501: [[i8; 3]; 45] = [
    [7, -9, -8],
    [1, 2, -8],
    [4, 7, -5],
    [2, 3, -1],
    [-1, 5, 8],
    [-8, -6, -3],
    [-3, -8, 7],
    [-3, 6, 8],
    [-4, -6, 8],
    [6, 7, 3],
    [3, 6, -9],
    [-5, -2, 3],
    [5, 8, 2],
    [-2, -7, -3],
    [-6, -8, -5],
    [2, 7, -3],
    [9, -1, -2],
    [1, 7, -6],
    [1, 9, -3],
    [-8, -9, -2],
    [-9, -8, 2],
    [5, 8, 4],
    [-7, 2, 5],
    [-1, 7, -4],
    [7, -8, 4],
    [-3, 2, -6],
    [1, -2, -9],
    [7, 3, -2],
    [-7, 8, 4],
    [1, -7, -5],
    [-5, 4, -3],
    [6, 7, -1],
    [-1, 7, -9],
    [3, 2, 6],
    [8, 3, -7],
    [-1, 9, -8],
    [5, -9, -7],
    [-7, 3, -9],
    [3, -1, -2],
    [6, 1, 4],
    [6, -7, 5],
    [8, -6, 3],
    [5, -2, 6],
    [8, 3, -5],
    [-2, -4, -9],
];

const T: Term = 1000;
const F: Term = 1001;

fn literal(l: i8) -> Term {
    let i = Term::from(l.unsigned_abs());
    if l > 0 { 1000 + 2 * i } else { 1001 + 2 * i }
}

/// The ontology of `clauses` over the variables `1..=vars`.
pub fn ontology(vars: u8, clauses: &[Vec<i8>]) -> Ontology {
    let mut b = Build::default();
    let torf = b.class(1);
    let tf = b.e(ClassExpr::OneOf(vec![T, F]));
    b.equivalent(torf, tf);
    for i in 1..=vars as i8 {
        let mut pm = vec![literal(i), literal(-i)];
        pm.sort_unstable();
        let pm = b.e(ClassExpr::OneOf(pm));
        b.equivalent(torf, pm);
    }
    b.different(T, F);
    for c in clauses {
        let mut ls: Vec<Term> = c.iter().map(|&l| literal(l)).collect();
        ls.sort_unstable();
        ls.dedup();
        let one = b.e(ClassExpr::OneOf(ls));
        b.assert(one, T);
    }
    b.o
}

/// Whether `clauses` is satisfiable, by brute force.
pub fn satisfiable(vars: u8, clauses: &[Vec<i8>]) -> bool {
    (0..1u32 << vars).any(|a| {
        clauses.iter().all(|c| {
            c.iter().any(|&l| {
                let v = a >> (l.unsigned_abs() - 1) & 1 == 1;
                v == (l > 0)
            })
        })
    })
}

/// Every combination of the search's switches, with small budgets.
fn configs() -> impl Iterator<Item = Config> {
    (0..16u32).map(|bits| Config {
        semantic_branching: bits & 1 != 0,
        backjumping: bits & 2 != 0,
        anywhere_blocking: bits & 4 != 0,
        disjunctions_first: bits & 8 != 0,
        timeout: Some(std::time::Duration::from_secs(2)),
        max_nodes: 10_000,
        max_memory: 256 << 20,
        ..Config::default()
    })
}

/// The right answer under every switch. With neither semantic branching nor backjumping
/// the search thrashes on these (a failed `T ≈ l` is tried again by other routes): it may
/// run out of time, never answer wrongly, and is left out where `thrashing` is false.
fn check(vars: u8, clauses: &[Vec<i8>], thrashing: bool) {
    let expected = if satisfiable(vars, clauses) {
        Answer::Consistent
    } else {
        Answer::Inconsistent
    };
    let o = ontology(vars, clauses);
    for config in configs() {
        let plain = !config.backjumping && !config.semantic_branching;
        if plain && !thrashing {
            continue;
        }
        let got = consistency(&o, &config).answer;
        let slow = plain && matches!(&got, Answer::GaveUp(why) if why.contains("time"));
        assert!(
            got == expected || slow,
            "{clauses:?} under {config:?}: {got:?}, not {expected:?}"
        );
    }
}

/// A merge that happens while an equality waits in the queue, or before a nominal is
/// tested, adds its dependencies: `T ≈ plus₁` by a choice, then `T`'s individual seen
/// through `plus₁`. Backjumping without them skipped the choice and refuted a consistent
/// ontology (DL-501 was answered inconsistent).
#[test]
fn merges_carry_their_dependencies() {
    check(8, &[vec![6, 1, 4], vec![5, -2, 6], vec![8, 3, -5]], true);
    let dl501: Vec<Vec<i8>> = DL501.iter().map(|c| c.to_vec()).collect();
    check(9, &dl501, true);
}

/// Random 3-SAT through nominals, around the hard ratio of clauses to variables, against
/// brute force: every merge choice and its backtracking.
#[test]
fn random_sat_through_nominals() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for round in 0..60 {
        let vars = 4 + (round % 4) as u8;
        let count = (f64::from(vars) * 4.3) as usize + next(5) as usize - 2;
        let clauses: Vec<Vec<i8>> = (0..count)
            .map(|_| {
                (0..3)
                    .map(|_| {
                        let v = 1 + next(u64::from(vars)) as i8;
                        if next(2) == 0 { v } else { -v }
                    })
                    .collect()
            })
            .collect();
        check(vars, &clauses, false);
    }
}
