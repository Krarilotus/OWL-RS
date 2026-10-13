//! Classification of inputs outside OWL 2 DL, by what [`super::consistency`] decided:
//!
//! - **inconsistent:** every class is unsatisfiable (exact);
//! - **consistent, the DL part's TBox without nominals and the universal property (but as
//!   a superproperty: a tautology):** the classification is the DL part's (exact). Proof:
//!   for named classes `A`, `B` with
//!   `A ⋢ B` in the TBox, a model `I₁` of the TBox has an element in `A ∖ B`; the disjoint
//!   union of `I₁` and a model `I₂` of the whole input, the individuals in `I₂`, is a model
//!   of the whole input. TBox axioms hold in a disjoint union where no expression names an
//!   individual or relates everything; assertions, keys and DL-safe rules concern named
//!   individuals only, which `I₂` interprets as before. So the input entails `A ⊑ B` iff
//!   the TBox does, and so does the consistent DL part;
//! - **otherwise:** the subsumptions of the DL part with the rule instances grounded (each
//!   entailed by the input), a lower bound; what isn't in it is open, and said so.

use std::collections::HashSet;

use nrese_owl::{Axiom, ClassExpr, ExprId, Ontology};

use super::{Decision, Options, Verdict, consistency};
use crate::classify::{self, Classification};

/// A classification with how far it goes.
#[derive(Debug, Clone)]
pub struct Classified {
    pub decision: Decision,
    /// What is known: every subsumption listed is entailed.
    pub classification: Classification,
    /// Whether it is the whole answer (every subsumption not listed isn't entailed).
    pub exact: bool,
    /// How it was found, or why it isn't exact.
    pub why: Vec<String>,
}

/// Whether a class expression names an individual or uses `owl:topObjectProperty`.
fn nominal(o: &Ontology, e: ExprId) -> bool {
    let top = o.builtin.top_object;
    let mut stack = vec![e];
    let mut seen = std::collections::HashSet::new();
    while let Some(e) = stack.pop() {
        if !seen.insert(e) {
            continue;
        }
        let p = match o.classes.get(e.0) {
            ClassExpr::OneOf(_) | ClassExpr::HasValue(..) => return true,
            ClassExpr::And(xs) | ClassExpr::Or(xs) => {
                stack.extend(xs);
                continue;
            }
            ClassExpr::Not(x) => {
                stack.push(*x);
                continue;
            }
            ClassExpr::Some(p, x)
            | ClassExpr::All(p, x)
            | ClassExpr::Min(_, p, x)
            | ClassExpr::Max(_, p, x)
            | ClassExpr::Exact(_, p, x) => {
                stack.push(*x);
                *p
            }
            ClassExpr::HasSelf(p) => *p,
            _ => continue,
        };
        if Some(p.named()) == top {
            return true;
        }
    }
    false
}

/// Whether the TBox and RBox (every axiom but the assertions) use no nominal and not the
/// universal property.
fn tbox_without_nominals(o: &Ontology) -> bool {
    let top = o.builtin.top_object;
    o.axioms.iter().filter(|a| !a.is_assertion()).all(|a| {
        let exprs: Vec<ExprId> = match a {
            Axiom::SubClassOf(x, y) => vec![*x, *y],
            Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => xs.clone(),
            Axiom::DisjointUnion(_, xs) => xs.clone(),
            Axiom::ObjectPropertyDomain(_, x)
            | Axiom::ObjectPropertyRange(_, x)
            | Axiom::DataPropertyDomain(_, x)
            | Axiom::HasKey(x, _, _) => vec![*x],
            _ => Vec::new(),
        };
        let properties: Vec<nrese_owl::ObjProp> = match a {
            // Into the universal property: a tautology.
            Axiom::SubObjectPropertyOf(_, sup) if Some(sup.named()) == top => Vec::new(),
            Axiom::SubObjectPropertyOf(chain, sup) => chain.iter().copied().chain([*sup]).collect(),
            Axiom::EquivalentObjectProperties(ps) | Axiom::DisjointObjectProperties(ps) => {
                ps.clone()
            }
            Axiom::InverseObjectProperties(p, q) => vec![*p, *q],
            Axiom::ObjectPropertyDomain(p, _)
            | Axiom::ObjectPropertyRange(p, _)
            | Axiom::ObjectCharacteristic(_, p) => vec![*p],
            Axiom::HasKey(_, ps, _) => ps.clone(),
            _ => Vec::new(),
        };
        exprs.iter().all(|&e| !nominal(o, e)) && properties.iter().all(|p| Some(p.named()) != top)
    })
}

/// `o` without its rules (and their diagnostics): the DL part.
fn dl_part(o: &Ontology) -> Ontology {
    let mut dl = o.clone();
    dl.rules.clear();
    dl.diagnostics
        .retain(|d| !matches!(d, nrese_owl::Diagnostic::Rule { .. }));
    dl
}

/// The classification of the input, as far as it can be told soundly (the module docs).
pub fn classify(o: &Ontology, options: &Options, with: &classify::Options) -> Classified {
    let decision = consistency(o, options);
    let classes = crate::context::signature(o);
    match &decision.verdict {
        Verdict::Inconsistent { .. } => Classified {
            classification: Classification {
                classes: classes.clone(),
                unsatisfiable: classes,
                consistent: false,
                ..Classification::default()
            },
            exact: true,
            why: vec!["inconsistent: every class is unsatisfiable".into()],
            decision,
        },
        Verdict::Consistent { .. } if tbox_without_nominals(o) => {
            let t = classify::classify(&dl_part(o), with);
            let exact = t.complete();
            let mut why = vec![
                "consistent, and the TBox names no individual: the DL part's classification"
                    .to_owned(),
            ];
            why.extend(t.incomplete.iter().cloned());
            Classified {
                classification: t.classification,
                exact,
                why,
                decision,
            }
        }
        Verdict::Consistent { .. } => {
            let mut work = decision.work.clone();
            work.rules.clear();
            let t = classify::classify(&work, with);
            let mut why = vec![
                "consistent, but the TBox names individuals: the DL part with the rule \
                 instances gives the entailed subsumptions; models of the whole input refute \
                 the rest"
                    .to_owned(),
            ];
            why.extend(t.incomplete.iter().cloned());
            let (classification, open) = refine(o, &decision, t.classification, options);
            if open > 0 {
                why.push(format!("{open} pairs of classes open"));
            }
            Classified {
                classification,
                exact: open == 0,
                why,
                decision,
            }
        }
        _ => {
            let mut work = decision.work.clone();
            work.rules.clear();
            let t = classify::classify(&work, with);
            let mut why = vec![match &decision.verdict {
                Verdict::Consistent { .. } => {
                    "consistent, but the TBox names individuals: the DL part with the rule \
                     instances gives the entailed subsumptions; the rest is open"
                        .to_owned()
                }
                Verdict::Undecided(w) => format!(
                    "consistency undecided ({w}): the subsumptions of the DL part with the rule \
                     instances found hold; the rest is open"
                ),
                Verdict::Inconsistent { .. } => unreachable!("handled above"),
            }];
            why.extend(t.incomplete.iter().cloned());
            Classified {
                classification: t.classification,
                exact: false,
                why,
                decision,
            }
        }
    }
}

/// A term for a fresh anonymous individual (a witness): far from any store's ids and from
/// the grounder's.
const WITNESS: nrese_owl::Term = u64::MAX / 2 - 1;

/// The classes `w` is in, in a certified model's interpretation.
fn classes_of(i: &crate::certificate::Interpretation, e: usize) -> HashSet<nrese_owl::Term> {
    i.classes
        .iter()
        .filter(|(_, es)| es.contains(&e))
        .map(|(&c, _)| c)
        .collect()
}

/// Whether `o` with `w : C` (`C` built from `sub` and `not`) is consistent, by
/// [`consistency`]: the decision.
fn with_witness(
    o: &Ontology,
    sub: nrese_owl::Term,
    not: Option<nrese_owl::Term>,
    options: &Options,
) -> Decision {
    let mut test = o.clone();
    let a = ExprId(test.classes.intern(ClassExpr::Class(sub)));
    let c = match not {
        None => a,
        Some(b) => {
            let b = ExprId(test.classes.intern(ClassExpr::Class(b)));
            let not_b = ExprId(test.classes.intern(ClassExpr::Not(b)));
            let mut parts = vec![a, not_b];
            parts.sort_unstable();
            ExprId(test.classes.intern(ClassExpr::And(parts)))
        }
    };
    test.axioms.push(Axiom::ClassAssertion(c, WITNESS));
    test.sources.push(Vec::new());
    test.anonymous.insert(WITNESS);
    consistency(&test, options)
}

/// The classification of a consistent input completed past `lower` (the DL part's with
/// the rule instances, entailed): a pair is refuted by the certified model of the input
/// (an element in `A ∖ B`), then by one with a witness in `A`, then by one in `A ⊓ ¬B`; a
/// witness that has no model makes the subsumption entailed. How many pairs stay open (the
/// budget `options.max_tests`, undecided tests).
fn refine(
    o: &Ontology,
    decision: &Decision,
    mut lower: Classification,
    options: &Options,
) -> (Classification, usize) {
    let classes = lower.classes.clone();
    let mut known: HashSet<(nrese_owl::Term, nrese_owl::Term)> =
        lower.subsumptions.iter().copied().collect();
    let unsat: HashSet<nrese_owl::Term> = lower.unsatisfiable.iter().copied().collect();
    let top: HashSet<nrese_owl::Term> = lower.top.iter().copied().collect();
    let entailed = |known: &HashSet<_>, a, b| {
        a == b || unsat.contains(&a) || top.contains(&b) || known.contains(&(a, b))
    };
    let mut refuted: HashSet<(nrese_owl::Term, nrese_owl::Term)> = HashSet::new();
    // The certified model of the input.
    if let Some(m) = &decision.model {
        for e in 0..m.size {
            let here = classes_of(m, e);
            for &a in &here {
                for &b in &classes {
                    if !here.contains(&b) {
                        refuted.insert((a, b));
                    }
                }
            }
        }
    }
    let open = |known: &HashSet<_>, refuted: &HashSet<_>, a| {
        classes
            .iter()
            .filter(|&&b| !entailed(known, a, b) && !refuted.contains(&(a, b)))
            .copied()
            .collect::<Vec<_>>()
    };
    let mut tests = 0usize;
    let budget = options.max_tests.unwrap_or(usize::MAX);
    let mut newly_unsat: Vec<nrese_owl::Term> = Vec::new();
    for &a in &classes {
        if unsat.contains(&a) || open(&known, &refuted, a).is_empty() || tests >= budget {
            continue;
        }
        tests += 1;
        let d = with_witness(o, a, None, options);
        match (&d.verdict, &d.model) {
            (Verdict::Consistent { .. }, Some(m)) => {
                if let Some(&w) = m.individuals.get(&WITNESS) {
                    let here = classes_of(m, w);
                    for &b in &classes {
                        if !here.contains(&b) {
                            refuted.insert((a, b));
                        }
                    }
                }
            }
            (Verdict::Inconsistent { .. }, _) => newly_unsat.push(a),
            _ => {}
        }
    }
    for &a in &classes {
        if newly_unsat.contains(&a) {
            continue;
        }
        for b in open(&known, &refuted, a) {
            if tests >= budget {
                break;
            }
            tests += 1;
            match with_witness(o, a, Some(b), options).verdict {
                Verdict::Consistent { .. } => {
                    refuted.insert((a, b));
                }
                Verdict::Inconsistent { .. } => {
                    known.insert((a, b));
                }
                Verdict::Undecided(_) => {}
            }
        }
    }
    let mut remaining = 0;
    for &a in &classes {
        if !newly_unsat.contains(&a) {
            remaining += open(&known, &refuted, a).len();
        }
    }
    lower.unsatisfiable.extend(newly_unsat);
    lower.unsatisfiable.sort_unstable();
    lower.unsatisfiable.dedup();
    let unsat_all: HashSet<nrese_owl::Term> = lower.unsatisfiable.iter().copied().collect();
    let mut subs: Vec<(nrese_owl::Term, nrese_owl::Term)> = known
        .into_iter()
        .filter(|(a, b)| a != b && !unsat_all.contains(a) && !top.contains(b))
        .collect();
    subs.sort_unstable();
    lower.subsumptions = subs;
    (lower, remaining)
}
