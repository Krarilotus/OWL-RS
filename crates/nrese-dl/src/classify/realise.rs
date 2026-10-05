//! Realisation (docs/design/owl2-dl.md §7): each named individual's types, and its most
//! specific ones over the taxonomy, as classification does it for classes:
//!
//! - **Known types:** the deterministic label of the individual in the model of the
//!   consistency test, closed upwards by the taxonomy (the store's RL closure is a lower
//!   bound for this too once the driver runs in the store, §8).
//! - **Possible types:** its label in that model, cut by its label in every later model.
//! - **Tests:** a candidate `D` of `a` is a type iff `¬D(a)` makes the ontology
//!   inconsistent. Most general first; a refutation's model rules out `D`'s subclasses for
//!   `a` and, through every individual's label in it, candidates of all the others. Tests
//!   run in waves, one per individual at a time, over every core.
//!
//! The output is the reference runner's canonical realisation: `a individual rep` per
//! direct type, the type by its representative as in the canonical taxonomy (`owl:Thing`
//! for an individual with no other type), sorted.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use nrese_owl::{Axiom, ClassExpr, EntityKind, Ontology, Term};
use rayon::prelude::*;

use super::driver::{insert_sorted, intersect_opt, reason, union_into};
use super::{Classification, Deadline, Options, Profile, Taxonomy, classify, with_pool};
use crate::tableau::{self, Answer, At, Labels, Prepared, Probe, ProbeOutcome, Want};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The individuals' types.
#[derive(Debug, Clone, Default)]
pub struct Realisation {
    /// The taxonomy the types are over.
    pub taxonomy: Taxonomy,
    /// The named individuals, sorted.
    pub individuals: Vec<Term>,
    /// Per individual: every named class it is an instance of (sorted).
    pub types: Vec<Vec<Term>>,
    /// Why it isn't complete (empty: it is).
    pub incomplete: Vec<String>,
    pub profile: Profile,
}

/// The named individuals of an ontology: declared or used (blank nodes left out).
pub fn individuals(ontology: &Ontology) -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    for a in &ontology.axioms {
        match a {
            Axiom::Declaration(EntityKind::NamedIndividual, t) | Axiom::ClassAssertion(_, t) => {
                out.push(*t);
            }
            Axiom::ObjectPropertyAssertion(_, a, b)
            | Axiom::NegativeObjectPropertyAssertion(_, a, b) => out.extend([*a, *b]),
            Axiom::DataPropertyAssertion(_, a, _)
            | Axiom::NegativeDataPropertyAssertion(_, a, _) => {
                out.push(*a);
            }
            Axiom::SameIndividual(v) | Axiom::DifferentIndividuals(v) => out.extend(v),
            _ => {}
        }
    }
    for id in 0..ontology.classes.len() {
        match ontology.classes.get(id as u32) {
            ClassExpr::OneOf(v) => out.extend(v),
            ClassExpr::HasValue(_, t) => out.push(*t),
            _ => {}
        }
    }
    out.retain(|t| !ontology.anonymous.contains(t));
    out.sort_unstable();
    out.dedup();
    out
}

/// Classifies `ontology` and realises its individuals.
pub fn realise(ontology: &Ontology, options: &Options) -> Realisation {
    let started = Instant::now();
    let deadline = Deadline::new(options.timeout);
    let taxonomy = classify(ontology, options);
    let individuals = individuals(ontology);
    let mut r = Realisation {
        individuals: individuals.clone(),
        incomplete: taxonomy.incomplete.clone(),
        profile: taxonomy.profile.clone(),
        ..Realisation::default()
    };
    if !taxonomy.classification.consistent {
        r.taxonomy = taxonomy;
        return r;
    }
    let t = Instant::now();
    let prepared = tableau::prepared(ontology);
    let (normalised, _) = super::clauses(&prepared, options);
    let classes = &taxonomy.classification.classes;
    let program = Prepared::new(&prepared, &normalised, classes, &individuals);
    let mut work = Work::new(&taxonomy.classification, &individuals, options, deadline);
    work.run(&program);
    r.types = work
        .types
        .iter()
        .map(|t| t.iter().map(|&c| classes[c as usize]).collect())
        .collect();
    r.incomplete.extend(work.incomplete);
    r.profile.type_candidates = work.stats.candidates;
    r.profile.type_tests = work.stats.tests;
    r.profile.type_positive = work.stats.positive;
    r.profile.compile += program.compile_time();
    r.profile.realisation = t.elapsed();
    r.profile.total = started.elapsed();
    r.taxonomy = taxonomy;
    r
}

#[derive(Debug, Default)]
struct Stats {
    candidates: u64,
    tests: u64,
    positive: u64,
}

struct Work<'a> {
    options: &'a Options,
    deadline: Deadline,
    /// Per class (index into the classification's classes): its subsumers, sorted.
    supers: Vec<Vec<u32>>,
    top: Vec<u32>,
    /// Per individual: proven types (closed upwards), possible types, refuted ones.
    types: Vec<Vec<u32>>,
    possible: Vec<Option<Vec<u32>>>,
    refuted: Vec<Vec<u32>>,
    /// Per individual: undecided (why).
    undecided: Vec<Option<String>>,
    incomplete: Vec<String>,
    stats: Stats,
}

impl<'a> Work<'a> {
    fn new(
        c: &Classification,
        individuals: &[Term],
        options: &'a Options,
        deadline: Deadline,
    ) -> Self {
        let index: HashMap<Term, u32> = c
            .classes
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, i as u32))
            .collect();
        let mut supers = vec![Vec::new(); c.classes.len()];
        for (a, b) in &c.subsumptions {
            if let (Some(&a), Some(&b)) = (index.get(a), index.get(b)) {
                supers[a as usize].push(b);
            }
        }
        for s in &mut supers {
            s.sort_unstable();
            s.dedup();
        }
        let mut top: Vec<u32> = c.top.iter().filter_map(|t| index.get(t).copied()).collect();
        top.sort_unstable();
        let n = individuals.len();
        Self {
            options,
            deadline,
            supers,
            top,
            types: vec![Vec::new(); n],
            possible: vec![None; n],
            refuted: vec![Vec::new(); n],
            undecided: vec![None; n],
            incomplete: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// `d` and its subsumers into `a`'s types.
    fn prove(&mut self, a: usize, d: u32) {
        insert_sorted(&mut self.types[a], d);
        let s = self.supers[d as usize].clone();
        union_into(&mut self.types[a], &s);
    }

    /// The individuals' labels of a model of the ontology.
    fn observe(&mut self, labels: &Labels) {
        for (a, label) in labels.individuals.iter().enumerate() {
            if let Some(label) = label {
                intersect_opt(&mut self.possible[a], &label.classes);
            }
        }
    }

    fn run(&mut self, program: &Prepared) {
        let n = self.types.len();
        let config = self.deadline.config(&self.options.tableau);
        let want = Want {
            elements: false,
            individuals: true,
        };
        let out = program.probe(
            &Probe {
                at: At::Nothing,
                positive: &[],
                negative: &[],
            },
            &config,
            want,
        );
        self.stats.tests += 1;
        match (&out.answer, &out.labels) {
            (Answer::Consistent, Some(labels)) => {
                for (a, label) in labels.individuals.iter().enumerate() {
                    if let Some(label) = label {
                        for &k in &label.known {
                            self.prove(a, k);
                        }
                    }
                }
                self.observe(labels);
            }
            (other, _) => {
                self.incomplete.push(format!(
                    "the individuals' model not found ({}): {}",
                    other.class(),
                    reason(other)
                ));
                return;
            }
        }
        for a in 0..n {
            let top = self.top.clone();
            union_into(&mut self.types[a], &top);
            for t in top {
                let s = self.supers[t as usize].clone();
                union_into(&mut self.types[a], &s);
            }
            if !program.has_individual(a as u32) {
                // No assertion or clause mentions it: a fresh element, of the types every
                // element has.
                self.possible[a] = Some(self.types[a].clone());
            }
        }
        for a in 0..n {
            let types = &self.types[a];
            self.stats.candidates += self.possible[a].as_ref().map_or(0, |p| {
                p.iter().filter(|d| types.binary_search(d).is_err()).count()
            }) as u64;
        }
        // Waves: each individual's most general open candidate, over the workers.
        let wave = (self.options.threads * 4).max(1);
        let mut cursor = 0usize;
        loop {
            if self.deadline.passed() {
                self.incomplete.push("realisation: the deadline".into());
                break;
            }
            let mut batch: Vec<(u32, u32)> = Vec::new();
            for step in 0..n {
                if batch.len() >= wave {
                    break;
                }
                let a = (cursor + step) % n;
                if let Some(d) = self.next_candidate(a) {
                    batch.push((a as u32, d));
                }
            }
            if batch.is_empty() {
                break;
            }
            cursor = (batch.last().map_or(0, |b| b.0 as usize) + 1) % n.max(1);
            let outcomes: Vec<(u32, u32, ProbeOutcome)> = with_pool(self.options.threads, || {
                batch
                    .par_iter()
                    .map(|&(a, d)| {
                        let probe = Probe {
                            at: At::Individual(a),
                            positive: &[],
                            negative: std::slice::from_ref(&d),
                        };
                        (a, d, program.probe(&probe, &config, want))
                    })
                    .collect()
            });
            for (a, d, out) in outcomes {
                let a = a as usize;
                self.stats.tests += 1;
                match out.answer {
                    Answer::Inconsistent => {
                        self.stats.positive += 1;
                        self.prove(a, d);
                    }
                    Answer::Consistent => {
                        insert_sorted(&mut self.refuted[a], d);
                        if let Some(labels) = &out.labels {
                            self.observe(labels);
                        }
                    }
                    other => {
                        insert_sorted(&mut self.refuted[a], d);
                        self.undecided[a] = Some(format!("{}: {}", other.class(), reason(&other)));
                    }
                }
            }
        }
        let undecided = self.undecided.iter().flatten().count();
        if undecided > 0 {
            let why = self
                .undecided
                .iter()
                .flatten()
                .next()
                .cloned()
                .unwrap_or_default();
            self.incomplete.push(format!(
                "{undecided} individuals' types not decided ({why})"
            ));
        }
    }

    /// `a`'s most general candidate not yet decided: possible, not proven, not refuted,
    /// not below a refuted one.
    fn next_candidate(&mut self, a: usize) -> Option<u32> {
        let possible = self.possible[a].as_ref()?;
        let mut best: Option<(usize, u32)> = None;
        for &d in possible {
            if self.types[a].binary_search(&d).is_ok() || self.refuted[a].binary_search(&d).is_ok()
            {
                continue;
            }
            if self.supers[d as usize]
                .iter()
                .any(|s| self.refuted[a].binary_search(s).is_ok())
            {
                continue;
            }
            let key = (self.supers[d as usize].len(), d);
            if best.is_none_or(|b| key < b) {
                best = Some(key);
            }
        }
        best.map(|(_, d)| d)
    }
}

impl Realisation {
    /// The canonical realisation (the reference runner's): `a individual rep` per direct
    /// type; `name` gives an IRI.
    pub fn canonical(&self, name: &dyn Fn(Term) -> String) -> String {
        let c = &self.taxonomy.classification;
        let index: HashMap<Term, usize> =
            c.classes.iter().enumerate().map(|(i, &t)| (t, i)).collect();
        let n = c.classes.len();
        let mut supers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        for (a, b) in &c.subsumptions {
            if let (Some(&a), Some(&b)) = (index.get(a), index.get(b)) {
                supers[a].insert(b);
            }
        }
        let top: BTreeSet<usize> = c.top.iter().filter_map(|t| index.get(t).copied()).collect();
        let unsat: BTreeSet<usize> = c
            .unsatisfiable
            .iter()
            .filter_map(|t| index.get(t).copied())
            .collect();
        let names: Vec<String> = c.classes.iter().map(|&t| name(t)).collect();
        let rep = |x: usize| -> String {
            if unsat.contains(&x) {
                return NOTHING.into();
            }
            if top.contains(&x) {
                return THING.into();
            }
            supers[x]
                .iter()
                .filter(|&&d| supers[d].contains(&x))
                .map(|&d| names[d].clone())
                .chain([names[x].clone()])
                .min()
                .unwrap_or_default()
        };
        let mut lines: BTreeSet<String> = BTreeSet::new();
        for (i, &a) in self.individuals.iter().enumerate() {
            let types: Vec<usize> = self.types[i]
                .iter()
                .filter_map(|t| index.get(t).copied())
                .filter(|t| !top.contains(t))
                .collect();
            // Direct: no other type strictly below it.
            let mut direct: BTreeSet<String> = BTreeSet::new();
            for &t in &types {
                let below = types
                    .iter()
                    .any(|&u| u != t && supers[u].contains(&t) && !supers[t].contains(&u));
                if !below {
                    direct.insert(rep(t));
                }
            }
            if direct.is_empty() {
                direct.insert(THING.into());
            }
            for d in direct {
                lines.insert(format!("a {} {}", name(a), d));
            }
        }
        if lines.is_empty() {
            return String::new();
        }
        let mut out = lines.into_iter().collect::<Vec<_>>().join("\n");
        out.push('\n');
        out
    }
}
