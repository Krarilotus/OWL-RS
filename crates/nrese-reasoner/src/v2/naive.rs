//! The naive reference evaluator (reasoner-v2 design §7.1): the oracle the batch and delta
//! executors are tested against.
//!
//! Every round re-evaluates every rule over all facts, until a round derives nothing new.
//! List axioms are re-instantiated each round, because derived facts can create them. The
//! evaluator is simple rather than fast: joins backtrack atom by atom, over hash indexes
//! rebuilt each round. It returns the closure's derived facts and every consistency
//! violation, with the rule and bindings that produced it.

use std::collections::{HashMap, HashSet};

use super::ir::{Atom, Guard, Head, Rule, Term};
use super::lists::{Facts, ListVocabulary, instantiate};

pub type Triple = [u64; 3];

/// A consistency rule that fired: its name and the bindings of its variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Violation {
    pub rule: String,
    pub bindings: Vec<u64>,
}

#[derive(Debug, Default)]
pub struct Closure {
    /// Facts derived beyond the input (never an input fact).
    pub derived: HashSet<Triple>,
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<String>,
    pub rounds: usize,
}

/// Indexes over a fact set for backtracking joins.
#[derive(Default)]
struct Index {
    all: Vec<Triple>,
    by_s: HashMap<u64, Vec<Triple>>,
    by_p: HashMap<u64, Vec<Triple>>,
    by_o: HashMap<u64, Vec<Triple>>,
    by_sp: HashMap<(u64, u64), Vec<Triple>>,
    by_po: HashMap<(u64, u64), Vec<Triple>>,
    set: HashSet<Triple>,
}

impl Index {
    fn new(facts: impl IntoIterator<Item = Triple>) -> Self {
        let mut index = Self::default();
        for t in facts {
            if index.set.insert(t) {
                index.all.push(t);
                index.by_s.entry(t[0]).or_default().push(t);
                index.by_p.entry(t[1]).or_default().push(t);
                index.by_o.entry(t[2]).or_default().push(t);
                index.by_sp.entry((t[0], t[1])).or_default().push(t);
                index.by_po.entry((t[1], t[2])).or_default().push(t);
            }
        }
        index
    }

    fn candidates(&self, s: Option<u64>, p: Option<u64>, o: Option<u64>) -> &[Triple] {
        fn slice(v: Option<&Vec<Triple>>) -> &[Triple] {
            v.map_or(&[][..], Vec::as_slice)
        }
        match (s, p, o) {
            (Some(s), Some(p), _) => slice(self.by_sp.get(&(s, p))),
            (_, Some(p), Some(o)) => slice(self.by_po.get(&(p, o))),
            (Some(s), None, _) => slice(self.by_s.get(&s)),
            (None, Some(p), None) => slice(self.by_p.get(&p)),
            (None, None, Some(o)) => slice(self.by_o.get(&o)),
            (None, None, None) => &self.all,
        }
    }
}

impl Facts for Index {
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64> {
        self.candidates(Some(subject), Some(predicate), None)
            .iter()
            .map(|t| t[2])
            .collect()
    }

    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)> {
        self.candidates(None, Some(predicate), None)
            .iter()
            .map(|t| (t[0], t[2]))
            .collect()
    }
}

/// Calls `emit` with every binding of `rule`'s variables that satisfies its body and guards.
fn matches(rule: &Rule, index: &Index, emit: &mut dyn FnMut(&[Option<u64>])) {
    let mut bindings = vec![None; rule.variables()];
    fn value(term: Term, bindings: &[Option<u64>]) -> Option<u64> {
        match term {
            Term::Const(c) => Some(c),
            Term::Var(v) => bindings[usize::from(v)],
        }
    }
    fn walk(
        rule: &Rule,
        atom: usize,
        index: &Index,
        bindings: &mut Vec<Option<u64>>,
        emit: &mut dyn FnMut(&[Option<u64>]),
    ) {
        if atom == rule.body.len() {
            let guards_hold = rule.guards.iter().all(|g| match g {
                Guard::NotEqual(a, b) => value(*a, bindings) != value(*b, bindings),
            });
            if guards_hold {
                emit(bindings);
            }
            return;
        }
        let Atom(terms) = rule.body[atom];
        let bound = terms.map(|t| value(t, bindings));
        for fact in index.candidates(bound[0], bound[1], bound[2]) {
            // Bind this atom's variables, checking repeated ones and constants.
            let mut newly = Vec::new();
            let mut consistent = true;
            for (position, term) in terms.iter().enumerate() {
                match *term {
                    Term::Const(c) if c != fact[position] => consistent = false,
                    Term::Var(v) => match bindings[usize::from(v)] {
                        Some(existing) if existing != fact[position] => consistent = false,
                        Some(_) => {}
                        None => {
                            bindings[usize::from(v)] = Some(fact[position]);
                            newly.push(v);
                        }
                    },
                    Term::Const(_) => {}
                }
                if !consistent {
                    break;
                }
            }
            if consistent {
                walk(rule, atom + 1, index, bindings, emit);
            }
            for v in newly {
                bindings[usize::from(v)] = None;
            }
        }
    }
    walk(rule, 0, index, &mut bindings, emit);
}

fn instantiate_head(atom: &Atom, bindings: &[Option<u64>]) -> Triple {
    atom.0.map(|term| match term {
        Term::Const(c) => c,
        Term::Var(v) => bindings[usize::from(v)].expect("safe rules bind head variables"),
    })
}

/// The closure of `input` under `rules` (plus the list rules when `lists` is given).
pub fn materialise(input: &[Triple], rules: &[Rule], lists: Option<&ListVocabulary>) -> Closure {
    let input_set: HashSet<Triple> = input.iter().copied().collect();
    let mut facts: HashSet<Triple> = input_set.clone();
    let mut closure = Closure::default();
    loop {
        closure.rounds += 1;
        let index = Index::new(facts.iter().copied());
        let mut program: Vec<Rule> = rules.to_vec();
        if let Some(vocabulary) = lists {
            let (list_rules, diagnostics) = instantiate(vocabulary, &index);
            program.extend(list_rules);
            closure.diagnostics = diagnostics;
        }
        let mut new = Vec::new();
        for rule in &program {
            let Head::Facts(heads) = &rule.head else {
                continue;
            };
            matches(rule, &index, &mut |bindings| {
                for head in heads {
                    let triple = instantiate_head(head, bindings);
                    if !index.set.contains(&triple) {
                        new.push(triple);
                    }
                }
            });
        }
        if new.is_empty() {
            // Consistency rules on the final closure.
            let mut seen = HashSet::new();
            for rule in program.iter().filter(|r| r.head == Head::Inconsistent) {
                matches(rule, &index, &mut |bindings| {
                    let violation = Violation {
                        rule: rule.name.clone(),
                        bindings: bindings.iter().map(|b| b.unwrap_or(0)).collect(),
                    };
                    if seen.insert(violation.clone()) {
                        closure.violations.push(violation);
                    }
                });
            }
            break;
        }
        facts.extend(new);
    }
    closure.derived = facts.difference(&input_set).copied().collect();
    closure
}
