//! Inputs outside OWL 2 DL (docs/design/owl2-dl.md §16): decided where that is sound,
//! with why each answer holds; where it isn't, what is outside OWL 2 DL, the restriction
//! of the specification it breaks, and what was tried.
//!
//! - **Repairs:** what the reader settles from the input alone (a property's type by its
//!   use, a blank node two expressions share) is read so and reported
//!   ([`Decision::repairs`]).
//! - **Monotonicity:** the axioms NRESE reads are part of what the input says; whatever
//!   follows from part of it follows from the whole. "Inconsistent", and every entailment
//!   of the part, hold for the input.
//! - **Rules** (DL-safe, `nrese_owl::rules`): each instance is an OWL 2 DL axiom
//!   ([`ground`]), so the loop runs the hypertableau on the DL part with the instances
//!   found so far, checks its model against every axiom and rule ([`crate::certificate`]),
//!   and adds the instances the model breaks: until a model breaks none ("consistent",
//!   certified: a model of the whole input), the instances make the part inconsistent
//!   ("inconsistent", by monotonicity: each instance follows from its rule), or a budget
//!   ends. Before it, instances whose body the assertions state are added outright
//!   (their heads are entailed).
//! - **Certification beyond the part:** an axiom the engines leave out but the input
//!   states in OWL 2's structure (a non-simple property where a simple one belongs, an
//!   irregular property hierarchy) has a direct semantics: the certificate checks it too.
//! - What the reader couldn't map has no direct semantics: then "consistent" isn't
//!   claimed, and the answer says why.

pub mod classify;
pub mod ground;

use std::time::{Duration, Instant};

use nrese_owl::{Diagnostic, Ontology, Term};

use crate::certificate::{self, Interpretation, Violation};
use crate::tableau::{self, Answer, Config};
use ground::Grounder;

/// The budgets: every one a setting, `None` unlimited.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The hypertableau's configuration (its own budgets apply per run).
    pub tableau: Config,
    /// The most rounds of model, check and grounding.
    pub max_rounds: Option<usize>,
    /// The most rule instances one round grounds.
    pub max_instances: Option<usize>,
    /// The whole decision's time.
    pub timeout: Option<Duration>,
    /// The most consistency tests a classification runs for the subsumptions its lower
    /// bound and models leave open ([`classify`]).
    pub max_tests: Option<usize>,
}

/// What is outside OWL 2 DL in the input, and the restriction it breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub what: String,
    /// The specification's section.
    pub restriction: &'static str,
    /// Whether the certificate checks it (it has a direct semantics) or the reader left it
    /// out (it hasn't).
    pub checked: bool,
}

/// How an answer was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A model of the whole input, checked (`elements`, `lifted`: as
    /// [`certificate::Certificate::Model`]).
    Consistent { elements: usize, lifted: bool },
    /// The DL part with `instances` rule instances (0: the DL part alone) has no model.
    Inconsistent { instances: usize },
    /// Neither: why.
    Undecided(String),
}

/// The answer for an input with what it rests on.
#[derive(Debug, Clone)]
pub struct Decision {
    pub verdict: Verdict,
    pub repairs: Vec<String>,
    pub left_out: Vec<LeftOut>,
    /// What was tried, in order.
    pub tried: Vec<String>,
    /// Rounds of the loop, rule instances grounded (by the chase over the assertions and by
    /// the loop).
    pub rounds: usize,
    pub chased: usize,
    pub grounded: usize,
    /// The DL part with the instances grounded (entailed by the input).
    pub work: Ontology,
    /// The certified model, where the input is consistent.
    pub model: Option<Interpretation>,
}

/// The spec's restriction for a diagnostic, and whether the axiom stays in the model
/// (the checker checks it) or the reader left it out.
fn restriction(d: &Diagnostic) -> Option<(&'static str, bool)> {
    Some(match d {
        Diagnostic::Rule { .. } => (
            "OWL 2 Structural Specification §9: no rule axioms (SWRL, a W3C Member Submission; DL-safe semantics)",
            true,
        ),
        Diagnostic::NonSimpleProperty { .. } => (
            "OWL 2 Structural Specification §11.1-11.2: a simple property required",
            true,
        ),
        Diagnostic::AnonymousIndividual { .. } => (
            "OWL 2 Structural Specification §11.2: a named individual in a nominal or hasValue",
            true,
        ),
        Diagnostic::AmbiguousProperty { .. } => (
            "OWL 2 Structural Specification §5.8.1: typing constraints (one IRI as two kinds of property)",
            false,
        ),
        Diagnostic::Syntax { .. } => (
            "OWL 2 Structural Specification §3: the functional-style grammar",
            false,
        ),
        Diagnostic::Malformed { .. }
        | Diagnostic::BrokenList { .. }
        | Diagnostic::NotOwl { .. } => (
            "OWL 2 Mapping to RDF Graphs §3: no structural axiom for the triples",
            false,
        ),
        Diagnostic::Unsupported { .. } => ("OWL 2 syntax NRESE doesn't read yet", false),
        Diagnostic::Reserved { .. } => (
            "OWL 2 Structural Specification §2.4: the reserved vocabulary used as an entity (OWL 2 Full)",
            false,
        ),
        Diagnostic::UndeclaredProperty { .. }
        | Diagnostic::SharedBlankNode { .. }
        | Diagnostic::Unused { .. }
        | Diagnostic::RepeatedOperand => return None,
    })
}

/// The normalisation's reason as the restriction it breaks, where it is outside OWL 2 DL.
fn engine_restriction(why: &str) -> &'static str {
    if why.contains("irregular") {
        "OWL 2 Structural Specification §11.2: a regular property hierarchy"
    } else if why.contains("datatype") || why.contains("facet") {
        "OWL 2 Structural Specification §4, §11.2: the datatype map"
    } else {
        "left out by NRESE's engines"
    }
}

impl Decision {
    fn new(o: &Ontology) -> Self {
        let mut repairs = Vec::new();
        let mut left_out = Vec::new();
        for d in &o.diagnostics {
            match restriction(d) {
                None => repairs.push(match d {
                    Diagnostic::UndeclaredProperty { property } => {
                        format!("property {property} typed by its use (no declaration)")
                    }
                    Diagnostic::SharedBlankNode { node } => {
                        format!("blank node {node} read as one expression where two use it")
                    }
                    Diagnostic::RepeatedOperand => "a disjointness repeating an operand read as \
                        that operand being empty (an individual: a contradiction), its meaning \
                        in OWL 2's RDF-Based Semantics"
                        .to_owned(),
                    other => format!("{other:?} read past (it says nothing)"),
                }),
                Some((restriction, checked)) => left_out.push(LeftOut {
                    what: match d {
                        Diagnostic::Rule { rule } => format!(
                            "DL-safe rule {rule} (line {})",
                            o.rules.get(*rule).map_or(0, |r| r.line)
                        ),
                        other => format!("{other:?}"),
                    },
                    restriction,
                    checked,
                }),
            }
        }
        // What the engines leave out of what the reader read: the certificate checks it.
        let normalised = nrese_owl::normalise(&tableau::prepared(o));
        let mut reasons: Vec<&str> = normalised
            .unsupported
            .iter()
            .map(|(_, why)| *why)
            .filter(|why| {
                *why != nrese_owl::UNSUPPORTED_DATATYPE_DEFINITIONS
                    && *why != nrese_owl::UNSUPPORTED_KEYS
            })
            .collect();
        reasons.sort_unstable();
        reasons.dedup();
        for why in reasons {
            let n = normalised
                .unsupported
                .iter()
                .filter(|(_, w)| *w == why)
                .count();
            left_out.push(LeftOut {
                what: format!("{n} axioms: {why}"),
                restriction: engine_restriction(why),
                checked: true,
            });
        }
        Self {
            verdict: Verdict::Undecided("not run".into()),
            repairs,
            left_out,
            tried: Vec::new(),
            rounds: 0,
            chased: 0,
            grounded: 0,
            work: Ontology::default(),
            model: None,
        }
    }

    fn undecided(mut self, why: String) -> Self {
        self.tried.push(why.clone());
        self.verdict = Verdict::Undecided(why);
        self
    }
}

/// The element → named individual map of an interpretation (the smallest name).
fn names(o: &Ontology, i: &Interpretation) -> impl Fn(usize) -> Option<Term> + use<> {
    let mut by: Vec<Option<Term>> = vec![None; i.size];
    for (&t, &e) in &i.individuals {
        if !o.anonymous.contains(&t) && by[e].is_none_or(|x| t < x) {
            by[e] = Some(t);
        }
    }
    move |e| by.get(e).copied().flatten()
}

/// Grounds `violations`; how many were new, or why one can't be written.
fn ground_all(
    work: &mut Ontology,
    grounder: &mut Grounder,
    violations: &[Violation],
    i: &Interpretation,
) -> Result<usize, String> {
    let name = names(work, i);
    let rules = work.rules.clone();
    let mut new = 0;
    for v in violations {
        if grounder.ground(work, v.rule, &rules[v.rule], &v.binding, &name)? {
            new += 1;
        }
    }
    Ok(new)
}

/// The interpretation the assertions alone state: an element per named individual (one
/// for those `SameIndividual` makes one), named classes, property and data values.
fn asserted(o: &Ontology) -> Interpretation {
    use nrese_owl::{Axiom, ClassExpr};
    let mut individuals: Vec<Term> = o.named_individuals();
    for r in &o.rules {
        for a in r.body.iter().chain(&r.head) {
            if let nrese_owl::rules::Atom::Class(_, nrese_owl::rules::IArg::Individual(t))
            | nrese_owl::rules::Atom::Data(_, nrese_owl::rules::IArg::Individual(t), _) = a
            {
                individuals.push(*t);
            }
            if let nrese_owl::rules::Atom::Object(_, x, y)
            | nrese_owl::rules::Atom::Same(x, y)
            | nrese_owl::rules::Atom::Different(x, y) = a
            {
                for z in [x, y] {
                    if let nrese_owl::rules::IArg::Individual(t) = z {
                        individuals.push(*t);
                    }
                }
            }
        }
    }
    individuals.sort_unstable();
    individuals.dedup();
    let index = |t: Term| individuals.binary_search(&t).ok();
    // Union-find over `SameIndividual`.
    let mut parent: Vec<usize> = (0..individuals.len()).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for a in &o.axioms {
        if let Axiom::SameIndividual(xs) = a {
            let ids: Vec<usize> = xs.iter().filter_map(|&t| index(t)).collect();
            for w in ids.windows(2) {
                let (a, b) = (root(&mut parent, w[0]), root(&mut parent, w[1]));
                parent[a] = b;
            }
        }
    }
    let mut element = vec![usize::MAX; individuals.len()];
    let mut size = 0;
    for k in 0..individuals.len() {
        let r = root(&mut parent, k);
        if element[r] == usize::MAX {
            element[r] = size;
            size += 1;
        }
        element[k] = element[r];
    }
    let mut i = Interpretation {
        size,
        ..Interpretation::default()
    };
    for (k, &t) in individuals.iter().enumerate() {
        i.individuals.insert(t, element[k]);
    }
    let values = certificate::data::Values::new(o);
    for a in &o.axioms {
        match a {
            Axiom::ClassAssertion(c, x) => {
                if let (ClassExpr::Class(t), Some(k)) = (o.classes.get(c.0), index(*x)) {
                    i.classes.entry(*t).or_default().push(element[k]);
                }
            }
            Axiom::ObjectPropertyAssertion(p, x, y) => {
                if let (Some(a), Some(b)) = (index(*x), index(*y)) {
                    i.roles
                        .entry(*p)
                        .or_default()
                        .push((element[a], element[b]));
                }
            }
            Axiom::DataPropertyAssertion(p, x, l) => {
                if let (Some(a), Ok(v)) = (index(*x), values.literal(*l)) {
                    i.data.entry(*p).or_default().push((element[a], v));
                }
            }
            _ => {}
        }
    }
    i
}

/// Whether the chase over the assertions may apply a rule: its body's atoms hold in the
/// asserted interpretation only where they are entailed (named classes, properties, data
/// values, equality, built-ins and data ranges on values; not a complex class, not `≠`).
fn chaseable(o: &Ontology, rule: &nrese_owl::Rule) -> bool {
    use nrese_owl::ClassExpr;
    use nrese_owl::rules::Atom;
    rule.body.iter().all(|a| match a {
        Atom::Class(c, _) => matches!(o.classes.get(c.0), ClassExpr::Class(_) | ClassExpr::Thing),
        Atom::Different(..) => false,
        Atom::Object(..)
        | Atom::Data(..)
        | Atom::Same(..)
        | Atom::BuiltIn(..)
        | Atom::Range(..) => true,
    })
}

/// Adds the heads of the rule instances the assertions entail, to a fixpoint: how many.
fn chase(
    work: &mut Ontology,
    grounder: &mut Grounder,
    options: &Options,
    deadline: Option<Instant>,
) -> Result<usize, String> {
    let rules: Vec<usize> = (0..work.rules.len())
        .filter(|&k| chaseable(work, &work.rules[k]))
        .collect();
    if rules.is_empty() {
        return Ok(0);
    }
    let mut total = 0;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(total);
        }
        let i = asserted(work);
        let mut only = work.clone();
        only.rules = rules.iter().map(|&k| work.rules[k].clone()).collect();
        let found =
            certificate::violations(&only, &i, options.max_instances.unwrap_or(usize::MAX))?;
        let found: Vec<Violation> = found
            .into_iter()
            .map(|v| Violation {
                rule: rules[v.rule],
                binding: v.binding,
            })
            .collect();
        let new = ground_all(work, grounder, &found, &i)?;
        if new == 0 {
            return Ok(total);
        }
        total += new;
    }
}

/// The namespace of SQWRL's built-ins (O'Connor and Das, *SQWRL: a Query Language for
/// OWL*, OWLED 2009).
const SQWRL: &str = "http://sqwrl.stanford.edu/";

/// Whether a rule is a SQWRL query: it uses a SQWRL built-in (`sqwrl:select`, …). SQWRL
/// reads such a rule as a query over the ontology, which adds nothing to it.
fn is_query(o: &Ontology, rule: &nrese_owl::Rule) -> bool {
    rule.body.iter().chain(&rule.head).any(|a| {
        matches!(a, nrese_owl::rules::Atom::BuiltIn(b, _)
            if o.data.iris.get(b).is_some_and(|iri| iri.starts_with(SQWRL)))
    })
}

/// The first built-in of a rule that isn't SWRL's (`swrlb:`), by its IRI.
fn foreign_built_in(o: &Ontology, rule: &nrese_owl::Rule) -> Option<String> {
    rule.body.iter().chain(&rule.head).find_map(|a| match a {
        nrese_owl::rules::Atom::BuiltIn(b, _) => {
            let iri = o.data.iris.get(b).cloned().unwrap_or_default();
            (!iri.starts_with(certificate::data::SWRLB)).then_some(iri)
        }
        _ => None,
    })
}

/// Whether the input is consistent, as far as can be decided soundly (the module docs).
pub fn consistency(o: &Ontology, options: &Options) -> Decision {
    let started = Instant::now();
    let deadline = options.timeout.map(|t| started + t);
    let mut d = Decision::new(o);
    let dropped: Vec<&LeftOut> = d.left_out.iter().filter(|l| !l.checked).collect();
    let mut dropped = (!dropped.is_empty()).then(|| {
        format!(
            "{} parts the reader couldn't read as OWL 2 have no direct semantics ({})",
            dropped.len(),
            dropped[0].restriction
        )
    });
    let mut work = o.clone();
    work.diagnostics
        .retain(|x| !matches!(x, Diagnostic::Rule { .. }));
    // SQWRL queries: their meaning is a query's, they assert nothing.
    let mut rules = Vec::new();
    for (k, r) in std::mem::take(&mut work.rules).into_iter().enumerate() {
        if is_query(o, &r) {
            d.repairs.push(format!(
                "rule {k} (line {}) is a SQWRL query (sqwrl: built-ins): a query, it asserts nothing",
                r.line
            ));
        } else if let Some(iri) = foreign_built_in(o, &r) {
            // A built-in outside SWRL's (`swrlb:`, over data values): no meaning NRESE
            // knows. Left out: "inconsistent" from the rest still holds.
            let why = format!(
                "rule {k} (line {}) uses the built-in {iri}, not one of SWRL's (swrlb:, over \
                 data values): it isn't evaluated",
                r.line
            );
            d.tried.push(why.clone());
            dropped.get_or_insert(why);
        } else {
            rules.push(r);
        }
    }
    work.rules = rules;
    let mut grounder = Grounder::new(&work);
    if !work.rules.is_empty() {
        match chase(&mut work, &mut grounder, options, deadline) {
            Ok(n) => {
                d.chased = n;
                d.tried
                    .push(format!("chase over the assertions: {n} rule instances"));
            }
            Err(why) => d
                .tried
                .push(format!("chase over the assertions stopped: {why}")),
        }
    }
    loop {
        d.rounds += 1;
        let mut config = Config {
            keep_model: true,
            ..options.tableau.clone()
        };
        if let Some(end) = deadline {
            config.deadline = Some(config.deadline.map_or(end, |x| x.min(end)));
        }
        let out = tableau::consistency(&work, &config);
        let instances = d.chased + d.grounded;
        let model = match out.answer {
            Answer::Inconsistent => {
                d.tried.push(format!(
                    "round {}: the DL part with {instances} rule instances is inconsistent",
                    d.rounds
                ));
                d.verdict = Verdict::Inconsistent { instances };
                d.work = work;
                return d;
            }
            Answer::GaveUp(why) => {
                d.work = work;
                let round = d.rounds;
                return d.undecided(format!("round {round}: the hypertableau gave up: {why}"));
            }
            Answer::Consistent | Answer::Unsupported(_) => match out.model {
                Some(m) => m,
                None => {
                    d.work = work;
                    return d.undecided("the hypertableau kept no model".into());
                }
            },
        };
        // A model that isn't certified still shows rule instances to ground (each sound
        // whatever the model); it only can't answer "consistent".
        let (i, lifted, uncertified) = match certificate::axiom_model(&work, &model) {
            Ok((i, lifted)) => (i, lifted, None),
            Err(why) => (certificate::candidate(&work, &model), false, Some(why)),
        };
        // The budget bounds what a round grounds, not what it checks: at least one instance
        // is asked for, so none found means every rule holds.
        let max = options.max_instances.unwrap_or(usize::MAX);
        let violations = match certificate::violations(&work, &i, max.max(1)) {
            Ok(v) => v,
            Err(why) => {
                d.work = work;
                return d.undecided(format!("a rule isn't evaluated: {why}"));
            }
        };
        if violations.is_empty() {
            d.work = work;
            if let Some(why) = uncertified {
                let round = d.rounds;
                return d.undecided(format!(
                    "round {round}: the DL part's model isn't certified: {why}"
                ));
            }
            if let Some(why) = dropped {
                d.model = Some(i);
                return d.undecided(format!("what NRESE read has a certified model, but {why}"));
            }
            d.tried.push(format!(
                "round {}: the model satisfies every axiom and rule",
                d.rounds
            ));
            d.verdict = Verdict::Consistent {
                elements: i.size,
                lifted,
            };
            d.model = Some(i);
            return d;
        }
        if violations.len() > max {
            d.work = work;
            let round = d.rounds;
            return d.undecided(format!(
                "round {round}: the model breaks a rule instance, and the instance budget \
                 ({max} a round) grounds none"
            ));
        }
        let new = match ground_all(&mut work, &mut grounder, &violations, &i) {
            Ok(n) => n,
            Err(why) => {
                d.work = work;
                return d.undecided(format!("a rule instance can't be written: {why}"));
            }
        };
        d.grounded += new;
        d.tried.push(format!(
            "round {}: the model breaks {} rule instances, {new} new",
            d.rounds,
            violations.len()
        ));
        if new == 0 {
            d.work = work;
            return d.undecided(match uncertified {
                Some(why) => format!(
                    "the DL part's model isn't certified ({why}), and the rule instances it \
                     breaks are all added"
                ),
                None => "the model breaks rule instances already added (their axioms hold in \
                         no model the check accepts)"
                    .into(),
            });
        }
        if options.max_rounds.is_some_and(|m| d.rounds >= m) {
            d.work = work;
            let round = d.rounds;
            return d.undecided(format!("the round budget ({round}) ran out"));
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            d.work = work;
            return d.undecided("the time budget ran out".into());
        }
    }
}

/// A minimal inconsistent part of an inconsistent input: the indexes of its axioms
/// (declarations always kept, not listed) and of its rules, such that the part is
/// inconsistent ([`consistency`]) and none of them can go; `None` where the input isn't
/// found inconsistent. Deletion by halving chunks, then one at a time; each step one
/// decision, so as dear as `n log n` of them at worst.
pub fn justify(o: &Ontology, options: &Options) -> Option<(Vec<usize>, Vec<usize>)> {
    use nrese_owl::Axiom;
    let inconsistent = |axioms: &[usize], rules: &[usize]| -> bool {
        let mut part = o.clone();
        let keep: std::collections::HashSet<usize> = axioms.iter().copied().collect();
        let mut kept_axioms = Vec::new();
        let mut kept_sources = Vec::new();
        for (k, a) in o.axioms.iter().enumerate() {
            if keep.contains(&k) || matches!(a, Axiom::Declaration(..)) {
                kept_axioms.push(a.clone());
                kept_sources.push(o.sources.get(k).cloned().unwrap_or_default());
            }
        }
        part.axioms = kept_axioms;
        part.sources = kept_sources;
        part.rules = rules.iter().map(|&k| o.rules[k].clone()).collect();
        part.diagnostics
            .retain(|d| !matches!(d, Diagnostic::Rule { .. }));
        for (k, _) in part.rules.iter().enumerate() {
            part.diagnostics.push(Diagnostic::Rule { rule: k });
        }
        matches!(
            consistency(&part, options).verdict,
            Verdict::Inconsistent { .. }
        )
    };
    let mut axioms: Vec<usize> = (0..o.axioms.len())
        .filter(|&k| !matches!(o.axioms[k], Axiom::Declaration(..)))
        .collect();
    let mut rules: Vec<usize> = (0..o.rules.len()).collect();
    if !inconsistent(&axioms, &rules) {
        return None;
    }
    // Items: axioms, then rules (as `axioms.len() + k`).
    let mut items: Vec<usize> = axioms
        .iter()
        .copied()
        .chain(rules.iter().map(|&k| o.axioms.len() + k))
        .collect();
    let split = |items: &[usize]| -> (Vec<usize>, Vec<usize>) {
        items.iter().partition(|&&x| x < o.axioms.len())
    };
    let mut chunk = items.len().div_ceil(2).max(1);
    loop {
        let mut at = 0;
        while at < items.len() {
            let end = (at + chunk).min(items.len());
            let rest: Vec<usize> = items[..at].iter().chain(&items[end..]).copied().collect();
            let (a, r) = split(&rest);
            let r: Vec<usize> = r.into_iter().map(|x| x - o.axioms.len()).collect();
            if inconsistent(&a, &r) {
                items = rest;
            } else {
                at = end;
            }
        }
        if chunk == 1 {
            break;
        }
        chunk = chunk.div_ceil(2);
    }
    let (a, r) = split(&items);
    axioms = a;
    rules = r.into_iter().map(|x| x - o.axioms.len()).collect();
    Some((axioms, rules))
}
