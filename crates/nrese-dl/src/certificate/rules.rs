//! DL-safe rules in a finite interpretation (`nrese_owl::rules`' semantics): every binding
//! of the individual variables to the elements named individuals denote and of the data
//! variables to data values under which the body holds must make the head hold. The body
//! binds a data variable through a data property atom (the values the element has) or a
//! built-in that computes it (its first argument); a variable nothing binds finitely is no
//! answer (`Err`), never guessed.

use std::collections::HashMap;

use nrese_owl::rules::{Atom, DArg, IArg, Rule};
use nrese_owl::{ExprId, Ontology};
use nrese_xsd::owl::Value;

use super::data::built_in;
use super::{Set, View};

/// What a variable is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Element(usize),
    Value(Value),
}

/// A rule instance an interpretation breaks: the rule (its index in [`Ontology::rules`])
/// and the binding of its variables under which the body holds and the head doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: usize,
    pub binding: Vec<Binding>,
}

/// The search's state for one rule.
struct Join<'v, 'a> {
    view: &'v View<'a>,
    rule: &'v Rule,
    /// The elements named individuals denote, sorted.
    named: &'v [usize],
    classes: HashMap<ExprId, Set>,
    out: Vec<Vec<Binding>>,
    max: usize,
}

impl View<'_> {
    /// The elements named (not anonymous) individuals denote.
    pub(super) fn named_elements(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .individuals
            .iter()
            .filter(|(t, _)| !self.o.anonymous.contains(t))
            .map(|(_, &e)| e)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The instances of `rule` this interpretation breaks, at most `max`.
    pub(super) fn violations(&self, rule: &Rule, max: usize) -> Result<Vec<Vec<Binding>>, String> {
        if let Some(why) = rule.malformed() {
            return Err(why.into());
        }
        let named = self.named_elements();
        let mut join = Join {
            view: self,
            rule,
            named: &named,
            classes: HashMap::new(),
            out: Vec::new(),
            max,
        };
        let mut binding: Vec<Option<Binding>> = vec![None; rule.variables.len()];
        let mut done = vec![false; rule.body.len()];
        join.search(&mut binding, &mut done)?;
        Ok(join.out)
    }
}

/// An atom's readiness: every variable bound (a test), or how it binds the rest.
enum Ready {
    Test,
    Generate,
    No,
}

impl Join<'_, '_> {
    fn class(&mut self, c: ExprId) -> Result<&Set, String> {
        if !self.classes.contains_key(&c) {
            let s = self.view.eval(c)?;
            self.classes.insert(c, s);
        }
        Ok(&self.classes[&c])
    }

    fn element(&self, a: &IArg, binding: &[Option<Binding>]) -> Result<Option<usize>, String> {
        Ok(match *a {
            IArg::Individual(t) => Some(
                self.view
                    .ind(t)
                    .ok_or("an individual the model doesn't name")?,
            ),
            IArg::Var(v) => match &binding[v as usize] {
                Some(Binding::Element(e)) => Some(*e),
                Some(Binding::Value(_)) => return Err("a data value bound as an individual".into()),
                None => None,
            },
        })
    }

    fn value(&self, a: &DArg, binding: &[Option<Binding>]) -> Result<Option<Value>, String> {
        Ok(match *a {
            DArg::Literal(t) => Some(self.view.values.literal(t)?),
            DArg::Var(v) => match &binding[v as usize] {
                Some(Binding::Value(x)) => Some(x.clone()),
                Some(Binding::Element(_)) => {
                    return Err("an individual bound as a data value".into());
                }
                None => None,
            },
        })
    }

    fn ready(&self, atom: &Atom, binding: &[Option<Binding>]) -> Ready {
        let unbound = atom
            .variables()
            .iter()
            .filter(|(v, _)| binding[*v as usize].is_none())
            .count();
        if unbound == 0 {
            return Ready::Test;
        }
        match atom {
            // Individual variables range over the named elements; a data atom binds its
            // value from its individual's values.
            Atom::Class(..)
            | Atom::Object(..)
            | Atom::Same(..)
            | Atom::Different(..)
            | Atom::Data(..) => Ready::Generate,
            // A built-in computes its first argument from the rest.
            // A computing built-in binds its first argument from the rest; a comparison
            // waits until every argument is bound.
            Atom::BuiltIn(b, args) => {
                let first_only = matches!(args.first(), Some(DArg::Var(v)) if binding[*v as usize].is_none())
                    && args[1..]
                        .iter()
                        .all(|a| !matches!(a, DArg::Var(v) if binding[*v as usize].is_none()));
                let computes = self
                    .view
                    .iri(*b)
                    .is_ok_and(|iri| super::data::computes(&iri));
                if first_only && computes {
                    Ready::Generate
                } else {
                    Ready::No
                }
            }
            Atom::Range(..) => Ready::No,
        }
    }

    /// Whether `atom` holds with every variable bound.
    fn holds(&mut self, atom: &Atom, binding: &[Option<Binding>]) -> Result<bool, String> {
        let el = |s: &Self, a: &IArg| -> Result<usize, String> {
            s.element(a, binding)?
                .ok_or_else(|| "an unbound variable".to_owned())
        };
        let val = |s: &Self, a: &DArg| -> Result<Value, String> {
            s.value(a, binding)?
                .ok_or_else(|| "an unbound variable".to_owned())
        };
        Ok(match atom {
            Atom::Class(c, x) => {
                let x = el(self, x)?;
                self.class(*c)?.contains(x)
            }
            Atom::Object(p, x, y) => {
                let (x, y) = (el(self, x)?, el(self, y)?);
                self.view.related(*p, x, y)
            }
            Atom::Data(p, x, v) => {
                let (x, v) = (el(self, x)?, val(self, v)?);
                self.view.data_values(*p, x)?.contains(&v)
            }
            Atom::Range(r, v) => {
                let v = val(self, v)?;
                self.view.values.member(*r, &v)?
            }
            Atom::Same(x, y) => el(self, x)? == el(self, y)?,
            Atom::Different(x, y) => el(self, x)? != el(self, y)?,
            Atom::BuiltIn(b, args) => {
                let iri = self.view.iri(*b)?;
                let values: Vec<Value> = args
                    .iter()
                    .map(|a| val(self, a))
                    .collect::<Result<_, _>>()?;
                let (first, rest) = values.split_first().ok_or("a built-in without arguments")?;
                built_in(&iri, Some(first), rest)?.is_some()
            }
        })
    }

    fn search(
        &mut self,
        binding: &mut Vec<Option<Binding>>,
        done: &mut Vec<bool>,
    ) -> Result<(), String> {
        if self.out.len() >= self.max {
            return Ok(());
        }
        // The next atom: a test where one is ready, else the one that binds with the fewest
        // unbound individual variables (each ranges over every named element).
        let mut next: Option<(usize, bool)> = None;
        let mut cost = usize::MAX;
        for (k, atom) in self.rule.body.iter().enumerate() {
            if done[k] {
                continue;
            }
            match self.ready(atom, binding) {
                Ready::Test => {
                    next = Some((k, true));
                    break;
                }
                Ready::Generate => {
                    let mut fresh: Vec<u32> = atom
                        .variables()
                        .into_iter()
                        .filter(|&(v, data)| !data && binding[v as usize].is_none())
                        .map(|(v, _)| v)
                        .collect();
                    fresh.dedup();
                    if fresh.len() < cost {
                        cost = fresh.len();
                        next = Some((k, false));
                    }
                }
                Ready::No => {}
            }
        }
        let Some((k, test)) = next else {
            if done.iter().any(|d| !d) {
                // The built-ins left: one that computes nothing here may be why.
                let pending: Vec<String> = self
                    .rule
                    .body
                    .iter()
                    .zip(done.iter())
                    .filter_map(|(a, &d)| match a {
                        Atom::BuiltIn(b, _) if !d => Some(self.view.iri(*b).unwrap_or_default()),
                        _ => None,
                    })
                    .collect();
                return Err(format!(
                    "a rule variable no body atom binds to finitely many values{}",
                    if pending.is_empty() {
                        String::new()
                    } else {
                        format!(" (built-ins waiting for it: {})", pending.join(", "))
                    }
                ));
            }
            // The body holds: the head must (an empty head is false, SWRL §3).
            let mut holds = !self.rule.head.is_empty();
            for atom in &self.rule.head {
                if !holds {
                    break;
                }
                holds = self.holds(atom, binding)?;
            }
            if !holds {
                self.out.push(
                    binding
                        .iter()
                        .map(|b| b.clone().unwrap_or(Binding::Element(usize::MAX)))
                        .collect(),
                );
            }
            return Ok(());
        };
        let atom = self.rule.body[k].clone();
        done[k] = true;
        if test {
            if self.holds(&atom, binding)? {
                self.search(binding, done)?;
            }
            done[k] = false;
            return Ok(());
        }
        for candidate in self.candidates(&atom, binding)? {
            let saved = binding.clone();
            for (v, b) in candidate {
                binding[v as usize] = Some(b);
            }
            if self.holds(&atom, binding)? {
                self.search(binding, done)?;
            }
            *binding = saved;
            if self.out.len() >= self.max {
                break;
            }
        }
        done[k] = false;
        Ok(())
    }

    /// Bindings of an atom's unbound variables to try (each then tested): its individual
    /// variables over the named elements, a data property atom's value over its
    /// individual's values, a built-in's first argument as it computes it.
    fn candidates(
        &mut self,
        atom: &Atom,
        binding: &[Option<Binding>],
    ) -> Result<Vec<Vec<(u32, Binding)>>, String> {
        let mut out: Vec<Vec<(u32, Binding)>> = vec![Vec::new()];
        let mut fresh: Vec<u32> = Vec::new();
        for (v, data) in atom.variables() {
            if !data && binding[v as usize].is_none() && !fresh.contains(&v) {
                fresh.push(v);
            }
        }
        for v in fresh {
            out = out
                .into_iter()
                .flat_map(|c| {
                    self.named.iter().map(move |&e| {
                        let mut c = c.clone();
                        c.push((v, Binding::Element(e)));
                        c
                    })
                })
                .collect();
        }
        match atom {
            Atom::Data(p, x, DArg::Var(d)) if binding[*d as usize].is_none() => {
                let mut with = Vec::new();
                for c in out {
                    let e = match *x {
                        IArg::Var(v) => match c.iter().find(|(w, _)| *w == v) {
                            Some((_, Binding::Element(e))) => *e,
                            _ => self.element(x, binding)?.ok_or("an unbound variable")?,
                        },
                        IArg::Individual(_) => {
                            self.element(x, binding)?.ok_or("an unbound variable")?
                        }
                    };
                    for value in self.view.data_values(*p, e)? {
                        let mut c = c.clone();
                        c.push((*d, Binding::Value(value)));
                        with.push(c);
                    }
                }
                Ok(with)
            }
            Atom::BuiltIn(b, args) => {
                let iri = self.view.iri(*b)?;
                let Some(DArg::Var(first)) = args.first() else {
                    return Ok(Vec::new());
                };
                let rest: Vec<Value> = args[1..]
                    .iter()
                    .map(|a| {
                        self.value(a, binding)?
                            .ok_or_else(|| "an unbound variable".to_owned())
                    })
                    .collect::<Result<_, _>>()?;
                Ok(match built_in(&iri, None, &rest)? {
                    Some(v) => vec![vec![(*first, Binding::Value(v))]],
                    None => Vec::new(),
                })
            }
            _ => Ok(out),
        }
    }
}

/// The rule instances `i` breaks, at most `max` (`usize::MAX`: every one), by rule; or why
/// they can't be told.
pub fn violations(
    o: &Ontology,
    i: &super::Interpretation,
    max: usize,
) -> Result<Vec<Violation>, String> {
    let view = View::new(o, i);
    let mut out = Vec::new();
    for (index, rule) in o.rules.iter().enumerate() {
        let left = max.saturating_sub(out.len());
        if left == 0 {
            break;
        }
        for binding in view.violations(rule, left)? {
            out.push(Violation {
                rule: index,
                binding,
            });
        }
    }
    Ok(out)
}
