//! The hypertableau driver of classification (docs/design/owl2-dl.md §7), in phases:
//!
//! 1. **Consistency** of the whole ontology, once. Without nominals the individuals can't
//!    change a subsumption of a consistent ontology (a model of the terminology and one of
//!    the ontology combine as a disjoint union), so the tests then run on the terminology
//!    alone ([`super::Options::tbox_only`]).
//! 2. **Satisfiability** of each class, in waves over the workers, most specific classes
//!    first (by their known subsumers): the model of a class `C` gives `C`'s known
//!    subsumers (its deterministic label) and possible ones (its label), and every other
//!    element of it cuts the possible subsumers of the classes in its label. A class seen
//!    in a model is satisfiable and needs no test of its own.
//! 3. **`owl:Thing`:** the classes in every label seen are its candidates.
//! 4. **Subsumption tests** of the candidates `P(C) \ K(C)` of each class, most general
//!    first: `C ⊓ ¬D` refuted is `C ⊑ D` (and `D`'s known subsumers with it); a model of it
//!    rules out `D`'s known subclasses and every candidate its label leaves out.
//!
//! Everything a test can't decide (a budget, a part of the ontology left out) is
//! reported, never guessed: the taxonomy then holds what was proven.

use std::time::Instant;

use nrese_owl::{Normalised, Ontology, Term};
use rayon::prelude::*;

use super::known::{self, Lower};
use super::{Classification, Deadline, Options, Profile, Taxonomy, with_pool};
use crate::tableau::{Answer, At, Labels, Prepared, Probe, ProbeOutcome, Want};

/// A class's state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Not known to be satisfiable yet.
    Open,
    Sat,
    Unsat,
    /// A test couldn't decide (why).
    Unknown(String),
}

pub(crate) struct Driver<'a> {
    ontology: &'a Ontology,
    normalised: &'a Normalised,
    classes: &'a [Term],
    options: &'a Options,
    deadline: Deadline,
    profile: Profile,
    incomplete: Vec<String>,
    status: Vec<Status>,
    /// Known subsumers per class (sorted, without the class).
    known: Vec<Vec<u32>>,
    /// Possible subsumers per class (sorted), `None` before any model had the class.
    possible: Vec<Option<Vec<u32>>>,
    /// The classes in every label seen (`owl:Thing`'s possible subsumers).
    top_possible: Option<Vec<u32>>,
}

impl<'a> Driver<'a> {
    pub(crate) fn new(
        ontology: &'a Ontology,
        normalised: &'a Normalised,
        classes: &'a [Term],
        options: &'a Options,
        deadline: Deadline,
    ) -> Self {
        let n = classes.len();
        Self {
            ontology,
            normalised,
            classes,
            options,
            deadline,
            profile: Profile {
                path: "tableau",
                classes: n,
                threads: options.threads,
                ..Profile::default()
            },
            incomplete: Vec::new(),
            status: vec![Status::Open; n],
            known: vec![Vec::new(); n],
            possible: vec![None; n],
            top_possible: None,
        }
    }

    fn config(&self) -> crate::tableau::Config {
        self.deadline.config(&self.options.tableau)
    }

    pub(crate) fn classify(mut self) -> Taxonomy {
        let started = Instant::now();
        let full = Prepared::new(self.ontology, self.normalised, self.classes, &[]);
        self.profile.compile += full.compile_time();
        // 1. Consistency.
        let t = Instant::now();
        let out = full.probe(
            &Probe {
                at: At::Nothing,
                positive: &[],
                negative: &[],
            },
            &self.config(),
            Want {
                elements: self.options.model_pruning,
                individuals: false,
            },
        );
        self.profile.consistency = t.elapsed();
        self.profile.tests += 1;
        match &out.answer {
            Answer::Inconsistent => return self.inconsistent(),
            Answer::Consistent => {}
            other => self.incomplete.push(format!(
                "consistency not decided ({}): {}",
                other.class(),
                reason(other)
            )),
        }
        if let Some(labels) = &out.labels {
            self.observe(labels, None);
        }
        let tbox;
        let tests =
            if self.options.tbox_only && !full.features().nominals && has_facts(self.normalised) {
                let mut terminology = self.normalised.clone();
                terminology.facts = Default::default();
                tbox = Prepared::new(self.ontology, &terminology, self.classes, &[]);
                self.profile.compile += tbox.compile_time();
                self.profile.tbox_only = true;
                &tbox
            } else {
                &full
            };
        // Known subsumers from the Horn part.
        if self.options.horn_lower_bound {
            let t = Instant::now();
            if let Some(lower) =
                known::horn_lower_bound(self.normalised, self.classes, self.options.threads)
            {
                self.apply_lower(&lower);
            }
            self.profile.lower_bound = t.elapsed();
        }
        // 2. Satisfiability.
        let t = Instant::now();
        self.satisfiability(tests);
        self.profile.satisfiability = t.elapsed();
        // 3. owl:Thing.
        let t = Instant::now();
        let top = self.top(tests);
        self.profile.top = t.elapsed();
        // 4. Subsumption tests.
        let t = Instant::now();
        let subsumers = self.subsumptions(tests, &top);
        self.profile.subsumption = t.elapsed();
        let mut c = Classification {
            classes: self.classes.to_vec(),
            consistent: true,
            ..Classification::default()
        };
        for (i, found) in subsumers.iter().enumerate() {
            if self.status[i] == Status::Unsat {
                c.unsatisfiable.push(self.classes[i]);
                continue;
            }
            // Undecided classes keep what was proven of them.
            let proven = if found.is_empty() {
                &self.known[i]
            } else {
                found
            };
            for &d in proven {
                c.subsumptions
                    .push((self.classes[i], self.classes[d as usize]));
            }
        }
        let unknown = self
            .status
            .iter()
            .filter(|s| matches!(s, Status::Unknown(_) | Status::Open))
            .count();
        if unknown > 0 {
            let why = self.status.iter().find_map(|s| match s {
                Status::Unknown(w) => Some(w.clone()),
                _ => None,
            });
            self.incomplete.push(format!(
                "{unknown} classes not decided ({})",
                why.unwrap_or_else(|| "the deadline".into())
            ));
        }
        c.top = top.iter().map(|&t| self.classes[t as usize]).collect();
        c.subsumptions.sort_unstable();
        c.subsumptions.dedup();
        c.unsatisfiable.sort_unstable();
        c.top.sort_unstable();
        self.profile.total = started.elapsed();
        Taxonomy {
            classification: c,
            incomplete: self.incomplete,
            profile: self.profile,
        }
    }

    fn inconsistent(self) -> Taxonomy {
        let c = Classification {
            classes: self.classes.to_vec(),
            unsatisfiable: self.classes.to_vec(),
            consistent: false,
            ..Classification::default()
        };
        Taxonomy {
            classification: c,
            incomplete: self.incomplete,
            profile: self.profile,
        }
    }

    fn apply_lower(&mut self, lower: &Lower) {
        for (i, k) in lower.known.iter().enumerate() {
            union_into(&mut self.known[i], k);
            if lower.unsat[i] {
                self.status[i] = Status::Unsat;
            }
        }
        self.profile.lower_known = lower.known.iter().map(Vec::len).sum();
        self.profile.lower_unsat = lower.unsat.iter().filter(|&&u| u).count();
    }

    /// Takes what a model shows: the probed class's label (`probed`), every other
    /// element's.
    fn observe(&mut self, labels: &Labels, probed: Option<u32>) {
        if let Some(c) = probed {
            let label = &labels.probe;
            let mut known = label.known.clone();
            known.retain(|&k| k != c);
            union_into(&mut self.known[c as usize], &known);
            self.see(&label.classes);
        }
        if self.options.model_pruning {
            for label in &labels.elements {
                self.see(label);
            }
        }
    }

    /// One element's label: each class in it is satisfiable, with no subsumer outside it.
    fn see(&mut self, label: &[u32]) {
        intersect_opt(&mut self.top_possible, label);
        for &b in label {
            let b = b as usize;
            if self.status[b] == Status::Open {
                self.status[b] = Status::Sat;
            }
            intersect_opt(&mut self.possible[b], label);
        }
        self.profile.labels_seen += 1;
    }

    fn satisfiability(&mut self, tests: &Prepared) {
        let n = self.classes.len();
        // Most specific first: their models show their superclasses.
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_by_key(|&c| std::cmp::Reverse(self.known[c as usize].len()));
        let wave = (self.options.threads * 2).max(1);
        let mut at = 0;
        while at < order.len() {
            if self.deadline.passed() {
                return;
            }
            let mut batch = Vec::with_capacity(wave);
            while at < order.len() && batch.len() < wave {
                let c = order[at];
                at += 1;
                let seen = self.possible[c as usize].is_some();
                match self.status[c as usize] {
                    Status::Open => batch.push(c),
                    Status::Sat if seen && !self.options.skip_seen => batch.push(c),
                    _ => {}
                }
            }
            if batch.is_empty() {
                continue;
            }
            let config = self.config();
            let want = Want {
                elements: self.options.model_pruning,
                individuals: false,
            };
            let outcomes: Vec<(u32, ProbeOutcome)> = with_pool(self.options.threads, || {
                batch
                    .par_iter()
                    .map(|&c| {
                        let probe = Probe {
                            at: At::Fresh,
                            positive: std::slice::from_ref(&c),
                            negative: &[],
                        };
                        (c, tests.probe(&probe, &config, want))
                    })
                    .collect()
            });
            for (c, out) in outcomes {
                self.profile.tests += 1;
                self.profile.sat_tests += 1;
                self.profile.add(&out.telemetry);
                match out.answer {
                    Answer::Inconsistent => self.status[c as usize] = Status::Unsat,
                    Answer::Consistent => {
                        self.status[c as usize] = Status::Sat;
                        if let Some(labels) = &out.labels {
                            self.observe(labels, Some(c));
                        }
                    }
                    other => {
                        self.status[c as usize] =
                            Status::Unknown(format!("{}: {}", other.class(), reason(&other)));
                    }
                }
            }
        }
        self.profile.sat_skipped = n - self.profile.sat_tests as usize;
    }

    /// The classes equivalent to `owl:Thing`.
    fn top(&mut self, tests: &Prepared) -> Vec<u32> {
        let config = self.config();
        let out = tests.probe(
            &Probe {
                at: At::Fresh,
                positive: &[],
                negative: &[],
            },
            &config,
            Want::default(),
        );
        self.profile.tests += 1;
        let mut top: Vec<u32> = Vec::new();
        match (&out.answer, &out.labels) {
            (Answer::Consistent, Some(labels)) => {
                intersect_opt(&mut self.top_possible, &labels.probe.classes);
                top = labels.probe.known.clone();
            }
            (Answer::Inconsistent, _) => {
                // Not reached for a consistent ontology: a fresh element always exists.
                self.incomplete
                    .push("a fresh element is refuted in a consistent ontology".into());
                return top;
            }
            (other, _) => {
                self.incomplete.push(format!(
                    "owl:Thing not decided ({}): {}",
                    other.class(),
                    reason(other)
                ));
                return top;
            }
        }
        let candidates: Vec<u32> = self
            .top_possible
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| top.binary_search(c).is_err())
            .collect();
        for d in candidates {
            if self.deadline.passed() {
                self.incomplete
                    .push("owl:Thing's candidates: the deadline".into());
                break;
            }
            let out = tests.probe(
                &Probe {
                    at: At::Fresh,
                    positive: &[],
                    negative: std::slice::from_ref(&d),
                },
                &config,
                Want::default(),
            );
            self.profile.tests += 1;
            self.profile.candidate_tests += 1;
            match out.answer {
                Answer::Inconsistent => {
                    insert_sorted(&mut top, d);
                }
                Answer::Consistent => {}
                other => self.incomplete.push(format!(
                    "owl:Thing ⊑ class {d} not decided ({})",
                    other.class()
                )),
            }
        }
        top
    }

    /// The subsumers of every satisfiable class.
    fn subsumptions(&mut self, tests: &Prepared, top: &[u32]) -> Vec<Vec<u32>> {
        let n = self.classes.len();
        let unsat: Vec<bool> = self.status.iter().map(|s| *s == Status::Unsat).collect();
        let config = self.config();
        let todo: Vec<u32> = (0..n as u32)
            .filter(|&c| self.status[c as usize] == Status::Sat)
            .collect();
        let this = &*self;
        let results: Vec<(u32, Vec<u32>, Stats)> = with_pool(self.options.threads, || {
            todo.par_iter()
                .map(|&c| {
                    let (s, stats) = this.subsumers_of(tests, c, top, &unsat, &config);
                    (c, s, stats)
                })
                .collect()
        });
        let mut out = vec![Vec::new(); n];
        for (c, s, stats) in results {
            self.profile.tests += stats.tests;
            self.profile.candidate_tests += stats.tests;
            self.profile.candidates += stats.candidates;
            self.profile.positive += stats.positive;
            self.profile.pruned += stats.pruned;
            if let Some(why) = stats.undecided {
                self.status[c as usize] = Status::Unknown(why);
            }
            out[c as usize] = s;
        }
        out
    }

    /// `C`'s subsumers: the known ones, `owl:Thing`'s, and the candidates the tests prove.
    fn subsumers_of(
        &self,
        tests: &Prepared,
        c: u32,
        top: &[u32],
        unsat: &[bool],
        config: &crate::tableau::Config,
    ) -> (Vec<u32>, Stats) {
        let mut stats = Stats::default();
        let mut s = self.known[c as usize].clone();
        union_into(&mut s, top);
        let possible = match &self.possible[c as usize] {
            Some(p) => p.clone(),
            None => {
                stats.undecided = Some("no model of the class".into());
                return (s, stats);
            }
        };
        let mut candidates: Vec<u32> = possible
            .into_iter()
            .filter(|&d| d != c && !unsat[d as usize] && s.binary_search(&d).is_err())
            .collect();
        stats.candidates = candidates.len() as u64;
        // Most general first.
        candidates.sort_by_key(|&d| (self.known[d as usize].len(), d));
        let mut out: Vec<bool> = vec![false; candidates.len()];
        for i in 0..candidates.len() {
            let d = candidates[i];
            if out[i] || s.binary_search(&d).is_ok() {
                continue;
            }
            if self.deadline.passed() {
                stats.undecided = Some("the deadline".into());
                break;
            }
            let probe = Probe {
                at: At::Fresh,
                positive: std::slice::from_ref(&c),
                negative: std::slice::from_ref(&d),
            };
            let result = tests.probe(&probe, config, Want::default());
            stats.tests += 1;
            match result.answer {
                Answer::Inconsistent => {
                    stats.positive += 1;
                    insert_sorted(&mut s, d);
                    union_into(&mut s, &self.known[d as usize]);
                }
                Answer::Consistent => {
                    out[i] = true;
                    let label = result.labels.map(|l| l.probe.classes).unwrap_or_default();
                    for j in i + 1..candidates.len() {
                        if out[j] {
                            continue;
                        }
                        let e = candidates[j];
                        // E ⊑ D known: C ⋢ E. E outside the model's label: C ⋢ E.
                        if self.known[e as usize].binary_search(&d).is_ok()
                            || label.binary_search(&e).is_err()
                        {
                            out[j] = true;
                            stats.pruned += 1;
                        }
                    }
                }
                other => {
                    stats.undecided = Some(format!("{}: {}", other.class(), reason(&other)));
                }
            }
        }
        s.retain(|&d| d != c);
        (s, stats)
    }
}

#[derive(Debug, Default)]
struct Stats {
    tests: u64,
    candidates: u64,
    positive: u64,
    pruned: u64,
    undecided: Option<String>,
}

fn has_facts(n: &Normalised) -> bool {
    let f = &n.facts;
    !(f.concepts.is_empty()
        && f.roles.is_empty()
        && f.data.is_empty()
        && f.same.is_empty()
        && f.different.is_empty()
        && f.not_roles.is_empty()
        && f.not_data.is_empty())
}

pub(crate) fn reason(answer: &Answer) -> &str {
    match answer {
        Answer::Unsupported(why) | Answer::GaveUp(why) => why,
        _ => "",
    }
}

// Sorted sets of class indexes -------------------------------------------------------------

pub(crate) fn union_into(a: &mut Vec<u32>, b: &[u32]) {
    if b.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    *a = out;
}

pub(crate) fn intersect_opt(a: &mut Option<Vec<u32>>, b: &[u32]) {
    match a {
        None => *a = Some(b.to_vec()),
        Some(v) => {
            let mut j = 0;
            v.retain(|&x| {
                while j < b.len() && b[j] < x {
                    j += 1;
                }
                j < b.len() && b[j] == x
            });
        }
    }
}

pub(crate) fn insert_sorted(a: &mut Vec<u32>, x: u32) {
    if let Err(i) = a.binary_search(&x) {
        a.insert(i, x);
    }
}
