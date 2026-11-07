//! Source subsets of the normal form (review A-1204): the clauses and facts with a source
//! set inside a set of axioms derive what those axioms entail, whichever axiom first made
//! a shared name, and nothing the set doesn't entail.

use std::collections::HashSet;

use nrese_owl::{
    Axiom, BodyAtom, Characteristic, ClassExpr, Clause, Concept, ExprId, HeadAtom, Normalised,
    ObjProp, Ontology, Options, Term, Var, normalise_with,
};

/// An atom over named individuals: a concept's or a role's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Fact {
    Concept(Concept, Term),
    Role(Term, Term, Term),
}

/// What the facts and clauses that `axioms` give derive over the individuals `names`, by
/// forward chaining over every binding of the clauses' variables to names. It reads the
/// Horn shapes these cases normalise into (concept, role and nominal atoms), and panics on
/// any other, so that a clause it can't read never goes unnoticed.
fn derived(n: &Normalised, axioms: &[usize], names: &[Term]) -> HashSet<Fact> {
    let within = |set: &Vec<usize>| set.iter().all(|a| axioms.contains(a));
    let mut facts: HashSet<Fact> = n
        .facts
        .concepts
        .iter()
        .filter(|f| axioms.contains(&f.2))
        .map(|&(c, a, _)| Fact::Concept(c, a))
        .chain(
            n.facts
                .roles
                .iter()
                .filter(|f| axioms.contains(&f.3))
                .map(|&(r, a, b, _)| Fact::Role(r, a, b)),
        )
        .collect();
    let clauses: Vec<&Clause> = n
        .clauses
        .iter()
        .filter(|c| c.sources.iter().any(within))
        .collect();
    loop {
        let before = facts.len();
        for clause in &clauses {
            assert!(
                clause.head.len() == 1,
                "not a Horn clause with a head: {clause:?}"
            );
            let mut vars: Vec<Var> = Vec::new();
            let mut note = |v: Var| {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            };
            for atom in &clause.body {
                match *atom {
                    BodyAtom::Concept(_, v) | BodyAtom::Nominal(_, v) => note(v),
                    BodyAtom::Role(_, a, b) => {
                        note(a);
                        note(b);
                    }
                    BodyAtom::Data(..) => panic!("a data atom: {clause:?}"),
                }
            }
            match clause.head[0] {
                HeadAtom::Concept(_, v) => note(v),
                HeadAtom::Role(_, a, b) => {
                    note(a);
                    note(b);
                }
                _ => panic!("a head this check doesn't read: {clause:?}"),
            }
            let mut binding = vec![0usize; vars.len()];
            loop {
                let at = |v: Var| names[binding[vars.iter().position(|&w| w == v).unwrap()]];
                let holds = clause.body.iter().all(|atom| match *atom {
                    BodyAtom::Concept(c, v) => facts.contains(&Fact::Concept(c, at(v))),
                    BodyAtom::Role(r, a, b) => facts.contains(&Fact::Role(r, at(a), at(b))),
                    BodyAtom::Nominal(t, v) => at(v) == t,
                    BodyAtom::Data(..) => unreachable!(),
                });
                if holds {
                    facts.insert(match clause.head[0] {
                        HeadAtom::Concept(c, v) => Fact::Concept(c, at(v)),
                        HeadAtom::Role(r, a, b) => Fact::Role(r, at(a), at(b)),
                        _ => unreachable!(),
                    });
                }
                // The next binding, as a number in base `names.len()`.
                let Some(i) = binding.iter().position(|&k| k + 1 < names.len()) else {
                    break;
                };
                binding[i] += 1;
                binding[..i].fill(0);
            }
        }
        if facts.len() == before {
            return facts;
        }
    }
}

fn class(o: &mut Ontology, c: Term) -> ExprId {
    ExprId(o.classes.intern(ClassExpr::Class(c)))
}

fn expr(o: &mut Ontology, e: ClassExpr) -> ExprId {
    ExprId(o.classes.intern(e))
}

/// `axioms` with no sources of their own, normalised with exact provenance.
fn normalised(mut o: Ontology, axioms: Vec<Axiom>) -> Normalised {
    o.axioms = axioms;
    o.sources = vec![Vec::new(); o.axioms.len()];
    let n = normalise_with(&o, Options::default());
    assert!(n.unsupported.is_empty(), "{:?}", n.unsupported);
    n
}

/// One row: `fact` follows from the axioms `with`, and not from `without` (the same, less
/// the axiom that uses the shared name), each checked by hand. Each axiom that uses the name
/// is a row of its own, so whichever of them makes it first (the queue is read last in,
/// first out), another one's row is checked.
type Row = (&'static [usize], &'static [usize], Fact);

/// Every row's failures, asserted once (a red run then shows each row).
fn assert_rows(n: &Normalised, names: &[Term], rows: &[Row]) {
    let mut wrong = Vec::new();
    for &(with, without, fact) in rows {
        if !derived(n, with, names).contains(&fact) {
            wrong.push(format!("{fact:?} not derived from axioms {with:?}"));
        }
        if derived(n, without, names).contains(&fact) {
            wrong.push(format!(
                "{fact:?} derived from axioms {without:?}, which don't entail it"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{}\n{:#?}\n{:#?}",
        wrong.join("\n"),
        n.clauses,
        n.facts
    );
}

/// A positive shared name (`fresh_positive`): `(A ⊓ B)(a)` (0) and `(A ⊓ B)(b)` (1) share
/// `Q ⊑ A ⊓ B`; axiom 0 alone entails `A(a)`, axiom 1 alone `A(b)`.
#[test]
fn a_positive_shared_name_has_each_axiom_that_uses_it_as_a_source() {
    let (a, b, ia, ib) = (1, 2, 100, 101);
    let mut o = Ontology::default();
    let (ea, eb) = (class(&mut o, a), class(&mut o, b));
    let both = expr(&mut o, ClassExpr::And(vec![ea, eb]));
    let n = normalised(
        o,
        vec![
            Axiom::ClassAssertion(both, ia),
            Axiom::ClassAssertion(both, ib),
        ],
    );
    let rows: [Row; 2] = [
        (&[0], &[1], Fact::Concept(Concept::Named(a), ia)),
        (&[1], &[0], Fact::Concept(Concept::Named(a), ib)),
    ];
    assert_rows(&n, &[ia, ib], &rows);
}

/// A negative shared name through an automaton (`fresh_negative` over a transitive role):
/// `A ⊓ ∃R.D ⊑ B` (0) and `C ⊓ ∃R.D ⊑ E` (1) share `∃R.D ⊑ P`, read as `D ⊑ ∀R⁻.P` along
/// R⁻'s automaton. With `Transitive(R)` (2), `A(a)` (3), `C(a)` (4), `R(a, b)` (5),
/// `R(b, c)` (6) and `D(c)` (7): axioms 0, 2, 3, 5–7 entail `B(a)`; 1, 2, 4–7 entail `E(a)`.
#[test]
fn a_negative_shared_name_and_its_automaton_have_each_axiom_that_uses_them() {
    let (ca, cb, cc, cd, ce, r) = (1, 2, 3, 4, 5, 10);
    let (ia, ib, ic) = (100, 101, 102);
    let mut o = Ontology::default();
    let (ea, eb, ec, ed, ee) = (
        class(&mut o, ca),
        class(&mut o, cb),
        class(&mut o, cc),
        class(&mut o, cd),
        class(&mut o, ce),
    );
    let some = expr(&mut o, ClassExpr::Some(ObjProp::Named(r), ed));
    let left0 = expr(&mut o, ClassExpr::And(vec![ea, some]));
    let left1 = expr(&mut o, ClassExpr::And(vec![ec, some]));
    let n = normalised(
        o,
        vec![
            Axiom::SubClassOf(left0, eb),
            Axiom::SubClassOf(left1, ee),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(r)),
            Axiom::ClassAssertion(ea, ia),
            Axiom::ClassAssertion(ec, ia),
            Axiom::ObjectPropertyAssertion(r, ia, ib),
            Axiom::ObjectPropertyAssertion(r, ib, ic),
            Axiom::ClassAssertion(ed, ic),
        ],
    );
    let rows: [Row; 2] = [
        (
            &[0, 2, 3, 5, 6, 7],
            &[2, 3, 5, 6, 7],
            Fact::Concept(Concept::Named(cb), ia),
        ),
        (
            &[1, 2, 4, 5, 6, 7],
            &[2, 4, 5, 6, 7],
            Fact::Concept(Concept::Named(ce), ia),
        ),
    ];
    assert_rows(&n, &[ia, ib, ic], &rows);
}

/// A shared automaton taken by two axioms directly: `A ⊑ ∀R.C` (0) and `D ⊑ ∀R.C` (1)
/// share R's automaton for `C`. With `Transitive(R)` (2), `A(a)` (3), `D(a)` (4),
/// `R(a, b)` (5) and `R(b, c)` (6): axioms 0, 2, 3, 5, 6 entail `C(c)`, and so do 1, 2,
/// 4–6.
#[test]
fn a_shared_automaton_has_each_axiom_that_uses_it_as_a_source() {
    let (ca, cc, cd, r) = (1, 3, 4, 10);
    let (ia, ib, ic) = (100, 101, 102);
    let mut o = Ontology::default();
    let (ea, ec, ed) = (class(&mut o, ca), class(&mut o, cc), class(&mut o, cd));
    let all = expr(&mut o, ClassExpr::All(ObjProp::Named(r), ec));
    let n = normalised(
        o,
        vec![
            Axiom::SubClassOf(ea, all),
            Axiom::SubClassOf(ed, all),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(r)),
            Axiom::ClassAssertion(ea, ia),
            Axiom::ClassAssertion(ed, ia),
            Axiom::ObjectPropertyAssertion(r, ia, ib),
            Axiom::ObjectPropertyAssertion(r, ib, ic),
        ],
    );
    let fact = Fact::Concept(Concept::Named(cc), ic);
    let rows: [Row; 2] = [
        (&[0, 2, 3, 5, 6], &[2, 3, 5, 6], fact),
        (&[1, 2, 4, 5, 6], &[2, 4, 5, 6], fact),
    ];
    assert_rows(&n, &[ia, ib, ic], &rows);
}
