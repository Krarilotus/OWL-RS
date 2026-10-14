//! NRESE's answers to the small-scope cases (docs/design/owl2-dl.md §11; the cases are
//! `nrese_owl::scope`'s), with the certificate of each answer that has one, for the gate's
//! test and the recorded campaign (`examples/scope_check.rs`) alike.
//!
//! Each case is answered by the three entry points the store uses: the hypertableau's
//! consistency (as configured by default, and keeping its model), classification and
//! realisation ([`crate::classify`]); they must agree. The code is the reference runner's
//! (`nrese.dl.Scope`): `i` for an inconsistent ontology, else `c` and a bit per question
//! of [`nrese_owl::scope::QUESTIONS`]; `u` (unsupported), `t` (gave up) or `x` (the entry
//! points disagree, a defect) with why.
//!
//! **Certificates** ([`crate::certificate`]): "consistent", each satisfiable class and
//! each non-entailment is the satisfiability of something (the ontology; it with a fresh
//! individual in `A`, in `A ⊓ ¬B`, in `¬A`; it with `a : ¬A`), shown by a model the
//! checker verifies. The rest (inconsistency, entailments) is decided by the references.

use std::collections::HashMap;
use std::time::Duration;

use nrese_owl::{Axiom, ClassExpr, ExprId, Intern, Ontology, Term, TermKind, Terms};

use crate::certificate::{self, Certificate};
use crate::classify;
use crate::tableau::{self, Answer, Config};

/// The terms a case's questions are about.
#[derive(Debug, Clone, Copy)]
pub struct Names {
    /// A and B.
    pub classes: [Term; 2],
    /// a and b.
    pub individuals: [Term; 2],
    /// An individual the ontology doesn't name, for satisfiability witnesses: anonymous,
    /// since a class is satisfiable by any element, and keys apply only to named ones.
    pub fresh: Term,
}

/// A case's answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    /// `i`, `c` and ten bits, or `u`, `t`, `x` with why.
    pub code: String,
    /// Per answer (consistency, then each question): `m` where a checked model certifies
    /// it, `-` where none does.
    pub certified: String,
}

impl Answers {
    fn undecided(kind: char, why: &str) -> Self {
        Self {
            code: format!("{kind} {why}"),
            certified: "-".repeat(11),
        }
    }
}

/// The engines' configuration for a case: the caller's time limit, no other budget.
fn config(timeout: Option<Duration>) -> Config {
    Config {
        timeout,
        ..Config::default()
    }
}

/// `o` with `x : C` (`x` anonymous where it is the fresh individual), `C` as `class`
/// builds it in the copy's table, and whether that has a certified model: `Some(true)` it
/// has, `Some(false)` it is inconsistent, `None` neither is shown.
fn witnessed(
    o: &Ontology,
    class: impl FnOnce(&mut Ontology) -> ExprId,
    x: Term,
    anonymous: bool,
    timeout: Option<Duration>,
) -> Option<bool> {
    let mut o = o.clone();
    let c = class(&mut o);
    o.axioms.push(Axiom::ClassAssertion(c, x));
    o.sources.push(Vec::new());
    if anonymous {
        o.anonymous.insert(x);
    }
    let out = tableau::consistency(
        &o,
        &Config {
            keep_model: true,
            ..config(timeout)
        },
    );
    match out.answer {
        Answer::Inconsistent => Some(false),
        Answer::Consistent => out
            .model
            .as_ref()
            .is_some_and(|m| certificate::certify(&o, m).is_model())
            .then_some(true),
        _ => None,
    }
}

fn kind(a: &Answer) -> Option<(char, String)> {
    match a {
        Answer::Unsupported(why) => Some(('u', why.clone())),
        Answer::GaveUp(why) => Some(('t', why.clone())),
        _ => None,
    }
}

/// The answers to case `o` (over `names`), each engine run within `timeout` (`None`: no
/// limit).
pub fn answer(o: &Ontology, names: &Names, timeout: Option<Duration>) -> Answers {
    let plain = tableau::consistency(o, &config(timeout));
    if let Some((k, why)) = kind(&plain.answer) {
        return Answers::undecided(k, &why);
    }
    let kept = tableau::consistency(
        o,
        &Config {
            keep_model: true,
            ..config(timeout)
        },
    );
    if let Some((k, why)) = kind(&kept.answer) {
        return Answers::undecided(k, &format!("keeping the model: {why}"));
    }
    if kept.answer != plain.answer {
        return Answers::undecided(
            'x',
            &format!(
                "consistency: {:?}, keeping the model {:?}",
                plain.answer, kept.answer
            ),
        );
    }
    let options = classify::Options {
        timeout,
        ..classify::Options::default()
    };
    let taxonomy = classify::classify(o, &options);
    if !taxonomy.complete() {
        return Answers::undecided(
            'u',
            &format!("classification: {}", taxonomy.incomplete.join("; ")),
        );
    }
    let realisation = classify::realise(o, &options);
    if !realisation.incomplete.is_empty() || !realisation.taxonomy.complete() {
        let mut why = realisation.incomplete.clone();
        why.extend(realisation.taxonomy.incomplete.iter().cloned());
        return Answers::undecided('u', &format!("realisation: {}", why.join("; ")));
    }
    let consistent = plain.answer == Answer::Consistent;
    let seen = [
        taxonomy.classification.consistent,
        realisation.taxonomy.classification.consistent,
    ];
    if seen.iter().any(|&c| c != consistent) {
        return Answers::undecided(
            'x',
            &format!("consistency {consistent}, classification and realisation {seen:?}"),
        );
    }
    if !consistent {
        return Answers {
            code: "i".into(),
            certified: "-".repeat(11),
        };
    }
    let bits_of = |c: &crate::context::Classification| -> [bool; 6] {
        let [a, b] = names.classes;
        let unsat = |x: Term| c.unsatisfiable.contains(&x);
        let sub =
            |x: Term, y: Term| unsat(x) || c.subsumptions.contains(&(x, y)) || c.top.contains(&y);
        [
            !unsat(a),
            !unsat(b),
            sub(a, b),
            sub(b, a),
            c.top.contains(&a),
            c.top.contains(&b),
        ]
    };
    let classified = bits_of(&taxonomy.classification);
    if bits_of(&realisation.taxonomy.classification) != classified {
        return Answers::undecided('x', "classification and realisation's taxonomy differ");
    }
    let types = |i: usize, c: usize| {
        realisation
            .individuals
            .iter()
            .position(|&t| t == names.individuals[i])
            .is_some_and(|k| realisation.types[k].contains(&names.classes[c]))
    };
    let mut bits: Vec<bool> = classified.to_vec();
    bits.extend([types(0, 0), types(0, 1), types(1, 0), types(1, 1)]);
    let code: String = std::iter::once('c')
        .chain(bits.iter().map(|&b| if b { '1' } else { '0' }))
        .collect();
    // Certificates.
    let mut certified = String::with_capacity(11);
    certified.push(
        match kept.model.as_ref().map(|m| certificate::certify(o, m)) {
            Some(Certificate::Model { .. }) => 'm',
            _ => '-',
        },
    );
    let [a, b] = names.classes;
    let x = names.fresh;
    for (k, &bit) in bits.iter().enumerate() {
        // The witness of the claim, where it is a satisfiability: a class (`sub`, if
        // any, and not `sup`, if any) and where it must hold.
        let witness: Option<(Option<Term>, Option<Term>, Term)> = match (k, bit) {
            (0, true) => Some((Some(a), None, x)),
            (1, true) => Some((Some(b), None, x)),
            (2, false) => Some((Some(a), Some(b), x)),
            (3, false) => Some((Some(b), Some(a), x)),
            (4, false) => Some((None, Some(a), x)),
            (5, false) => Some((None, Some(b), x)),
            (6, false) => Some((None, Some(a), names.individuals[0])),
            (7, false) => Some((None, Some(b), names.individuals[0])),
            (8, false) => Some((None, Some(a), names.individuals[1])),
            (9, false) => Some((None, Some(b), names.individuals[1])),
            _ => None,
        };
        let mark = match witness {
            None => '-',
            Some((sub, not, at)) => {
                let build = |o: &mut Ontology| {
                    let mut parts: Vec<ExprId> = Vec::new();
                    if let Some(s) = sub {
                        parts.push(ExprId(o.classes.intern(ClassExpr::Class(s))));
                    }
                    if let Some(n) = not {
                        let c = ExprId(o.classes.intern(ClassExpr::Class(n)));
                        parts.push(ExprId(o.classes.intern(ClassExpr::Not(c))));
                    }
                    parts.sort();
                    match parts[..] {
                        [one] => one,
                        _ => ExprId(o.classes.intern(ClassExpr::And(parts))),
                    }
                };
                match witnessed(o, build, at, at == names.fresh, timeout) {
                    Some(true) => 'm',
                    Some(false) => {
                        return Answers::undecided(
                            'x',
                            &format!(
                                "{} is answered {bit}, but its witness is inconsistent",
                                nrese_owl::scope::QUESTIONS[k]
                            ),
                        );
                    }
                    None => '-',
                }
            }
        };
        certified.push(mark);
    }
    Answers { code, certified }
}

/// A term of a case's document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Text {
    Iri(String),
    Literal(String, String, Option<String>),
    Blank(String),
}

/// The terms of a case's document, as the functional-syntax reader interns them.
#[derive(Debug, Default)]
struct Table {
    terms: Vec<Text>,
    ids: HashMap<Text, Term>,
}

impl Table {
    fn id(&mut self, t: Text) -> Term {
        if let Some(&id) = self.ids.get(&t) {
            return id;
        }
        let id = self.terms.len() as Term;
        self.terms.push(t.clone());
        self.ids.insert(t, id);
        id
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match &self.terms[term as usize] {
            Text::Iri(_) => TermKind::Iri,
            Text::Blank(_) => TermKind::Blank,
            Text::Literal(..) => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            Text::Literal(l, ..) => Some(l.clone()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids.get(&Text::Iri(iri.to_owned())).copied()
    }

    fn datatype(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            Text::Literal(_, d, _) => Some(d.clone()),
            _ => None,
        }
    }

    fn language(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            Text::Literal(_, _, l) => l.clone(),
            _ => None,
        }
    }

    fn iri_text(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            Text::Iri(i) => Some(i.clone()),
            _ => None,
        }
    }
}

impl Intern for Table {
    fn iri_id(&mut self, iri: &str) -> Term {
        self.id(Text::Iri(iri.to_owned()))
    }

    fn literal_id(&mut self, lexical: &str, datatype: &str, language: Option<&str>) -> Term {
        self.id(Text::Literal(
            lexical.to_owned(),
            datatype.to_owned(),
            language.map(str::to_owned),
        ))
    }

    fn blank_id(&mut self, label: &str, document: u32) -> Term {
        self.id(Text::Blank(format!("d{document}x{label}")))
    }
}

/// A case's document (`nrese_owl::scope::document`) read by the functional-syntax reader,
/// as the references read it, with the terms its questions are about; an error for a
/// document with a diagnostic (the cases are OWL 2 DL: none should have one).
pub fn read(document: &str) -> Result<(Ontology, Names), String> {
    let mut table = Table::default();
    let scope = nrese_owl::scope::SCOPE;
    let mut name = |local: &str| table.iri_id(&format!("{scope}{local}"));
    let names = Names {
        classes: [name("A"), name("B")],
        individuals: [name("a"), name("b")],
        fresh: name("x"),
    };
    let (o, _) = nrese_owl::read_functional(document, &mut table);
    if let Some(d) = o.diagnostics.first() {
        return Err(format!(
            "{} diagnostics, the first {d:?}",
            o.diagnostics.len()
        ));
    }
    Ok((o, names))
}

/// A case with DL-safe rules (`nrese_owl::rules`) read as [`read`] does: its only
/// diagnostics may be the rules'.
pub fn read_with_rules(document: &str) -> Result<(Ontology, Names), String> {
    let mut table = Table::default();
    let scope = nrese_owl::scope::SCOPE;
    let mut name = |local: &str| table.iri_id(&format!("{scope}{local}"));
    let names = Names {
        classes: [name("A"), name("B")],
        individuals: [name("a"), name("b")],
        fresh: name("x"),
    };
    let (o, _) = nrese_owl::read_functional(document, &mut table);
    if let Some(d) = o
        .diagnostics
        .iter()
        .find(|d| !matches!(d, nrese_owl::Diagnostic::Rule { .. }))
    {
        return Err(format!("a diagnostic besides the rules': {d:?}"));
    }
    Ok((o, names))
}

/// The answers to a case with rules, each by [`crate::beyond::consistency`]: of the case,
/// then of the case with the test of each question (`x : A`, `x : A ⊓ ¬B`, `x : ¬A`,
/// `a : ¬A`, …, `x` a fresh anonymous individual). Certified marks: `m` a model of the
/// whole case (with the test) checked, `g` inconsistent by monotonicity (the DL part with
/// rule instances); `u` with why where one isn't decided.
pub fn answer_beyond(o: &Ontology, names: &Names, options: &crate::beyond::Options) -> Answers {
    use crate::beyond::{Verdict, consistency};
    let decide = |o: &Ontology| -> Result<bool, String> {
        match consistency(o, options).verdict {
            Verdict::Consistent { .. } => Ok(true),
            Verdict::Inconsistent { .. } => Ok(false),
            Verdict::Undecided(why) => Err(why),
        }
    };
    match decide(o) {
        Err(why) => return Answers::undecided('u', &why),
        Ok(false) => {
            return Answers {
                code: "i".into(),
                certified: format!("g{}", "-".repeat(10)),
            };
        }
        Ok(true) => {}
    }
    let [a, b] = names.classes;
    let x = names.fresh;
    let mut code = String::from("c");
    let mut certified = String::from("m");
    for k in 0..10 {
        // The test: who, in which class; and whether a model of it answers "yes".
        let (sub, not, at, yes_if_consistent) = match k {
            0 => (Some(a), None, x, true),
            1 => (Some(b), None, x, true),
            2 => (Some(a), Some(b), x, false),
            3 => (Some(b), Some(a), x, false),
            4 => (None, Some(a), x, false),
            5 => (None, Some(b), x, false),
            6 => (None, Some(a), names.individuals[0], false),
            7 => (None, Some(b), names.individuals[0], false),
            8 => (None, Some(a), names.individuals[1], false),
            _ => (None, Some(b), names.individuals[1], false),
        };
        let mut test = o.clone();
        let mut parts: Vec<ExprId> = Vec::new();
        if let Some(s) = sub {
            parts.push(ExprId(test.classes.intern(ClassExpr::Class(s))));
        }
        if let Some(n) = not {
            let c = ExprId(test.classes.intern(ClassExpr::Class(n)));
            parts.push(ExprId(test.classes.intern(ClassExpr::Not(c))));
        }
        parts.sort();
        let c = match parts[..] {
            [one] => one,
            _ => ExprId(test.classes.intern(ClassExpr::And(parts))),
        };
        test.axioms.push(Axiom::ClassAssertion(c, at));
        test.sources.push(Vec::new());
        if at == x {
            test.anonymous.insert(x);
        }
        match decide(&test) {
            Err(why) => {
                return Answers::undecided(
                    'u',
                    &format!("{}: {why}", nrese_owl::scope::QUESTIONS[k]),
                );
            }
            Ok(consistent) => {
                code.push(if consistent == yes_if_consistent {
                    '1'
                } else {
                    '0'
                });
                certified.push(if consistent { 'm' } else { 'g' });
            }
        }
    }
    Answers { code, certified }
}
