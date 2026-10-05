//! Many tests over one compiled program, for classification and realisation (package 3.4,
//! docs/design/owl2-dl.md §7): the program is compiled once and shared by every thread;
//! each probe asserts classes and negated classes on a fresh element or an individual and
//! runs the search. A probe that ends in a model hands back the model's labels:
//!
//! - **the probed element's classes**, and those that hold in every model of the probe
//!   (deterministic: derived with an empty dependency set, also through merges): the
//!   known subsumers or types; everything outside the label is refuted by this model;
//! - **every other element's classes** (live nodes that aren't blocked, data values left
//!   out): each is an element of a model of the ontology, so a class `B` in such a label
//!   `L` has no subsumer outside `L` (HermiT's pruning of possible subsumers, Glimm et al.,
//!   JWS 2012);
//! - **each individual's classes**, known and possible, for realisation.
//!
//! Classes are numbered by the caller's class list, individuals by its individual list.

use std::time::{Duration, Instant};

use nrese_owl::{Concept, Normalised, Ontology, Term};

use super::depset::DepSetId;
use super::engine::Engine;
use super::graph::{NONE, flag};
use super::program::{ConceptId, ConceptName, Program};
use super::search::{End, Seed, Site};
use super::{Answer, Config, Features, Telemetry, answer_of, features};

/// A program compiled once for many probes.
pub struct Prepared {
    program: Program,
    features: Features,
    compile: Duration,
    /// By program concept: the class's index in the caller's list, or `NONE`.
    class_of: Vec<u32>,
    /// By class index: its program concept.
    concept_of: Vec<ConceptId>,
    /// By the caller's individual index: the program's individual, or `NONE` (an
    /// individual no assertion or clause mentions).
    individual_of: Vec<u32>,
}

/// Where a probe's classes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// No probe: the ontology's own consistency (with its individuals' labels).
    Nothing,
    /// A fresh element.
    Fresh,
    /// An individual, by the caller's index.
    Individual(u32),
}

/// A probe: classes (by index) asserted and refuted at one element.
#[derive(Debug, Clone, Copy)]
pub struct Probe<'a> {
    pub at: At,
    pub positive: &'a [u32],
    pub negative: &'a [u32],
}

/// What to read off a model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Want {
    /// The other elements' labels.
    pub elements: bool,
    /// The individuals' labels.
    pub individuals: bool,
}

/// A label: its classes, sorted, and the deterministic ones among them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Label {
    pub classes: Vec<u32>,
    pub known: Vec<u32>,
}

/// The labels of a model a probe ended in.
#[derive(Debug, Clone, Default)]
pub struct Labels {
    /// The probed element's (empty for [`At::Nothing`]).
    pub probe: Label,
    /// The other elements' classes, each sorted, without repeats (if wanted).
    pub elements: Vec<Vec<u32>>,
    /// By the caller's individual index (if wanted; `None` for individuals the program
    /// doesn't have: their classes are those of a fresh element without assertions).
    pub individuals: Vec<Option<Label>>,
}

/// A probe's answer: `Inconsistent` refutes it (a subsumption or a type holds),
/// `Consistent` comes with the model's labels.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub answer: Answer,
    pub telemetry: Telemetry,
    pub labels: Option<Labels>,
}

impl Prepared {
    /// Compiles `normalised` (of `ontology`) for probes over `classes` and `individuals`.
    pub fn new(
        ontology: &Ontology,
        normalised: &Normalised,
        classes: &[Term],
        individuals: &[Term],
    ) -> Self {
        let started = Instant::now();
        let mut program = Program::compile(ontology, normalised);
        let concept_of: Vec<ConceptId> = classes
            .iter()
            .map(|&c| program.concept(ConceptName::Clause(Concept::Named(c))))
            .collect();
        program.ensure_tables();
        let mut class_of = vec![NONE; program.concepts.len()];
        for (i, &c) in concept_of.iter().enumerate() {
            class_of[c as usize] = i as u32;
        }
        let at: hashbrown::HashMap<Term, u32> = program
            .individuals
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, i as u32))
            .collect();
        let individual_of = individuals
            .iter()
            .map(|t| at.get(t).copied().unwrap_or(NONE))
            .collect();
        let features = features(&program);
        Self {
            program,
            features,
            compile: started.elapsed(),
            class_of,
            concept_of,
            individual_of,
        }
    }

    pub fn features(&self) -> Features {
        self.features
    }

    /// What was left out of the program (an empty list: nothing).
    pub fn weakened(&self) -> &[String] {
        &self.program.weakened
    }

    /// The time compiling took.
    pub fn compile_time(&self) -> Duration {
        self.compile
    }

    /// Whether the program has the individual (by the caller's index).
    pub fn has_individual(&self, individual: u32) -> bool {
        self.individual_of[individual as usize] != NONE
    }

    /// Runs `probe`; a model's labels as `want` asks.
    pub fn probe(&self, probe: &Probe<'_>, config: &Config, want: Want) -> ProbeOutcome {
        let started = Instant::now();
        let site = match probe.at {
            At::Nothing => Site::Nothing,
            At::Fresh => Site::Fresh,
            At::Individual(i) => match self.individual_of[i as usize] {
                NONE => Site::Fresh,
                p => Site::Individual(p),
            },
        };
        let seed = Seed {
            site,
            positive: probe
                .positive
                .iter()
                .map(|&c| self.concept_of[c as usize])
                .collect(),
            negative: probe
                .negative
                .iter()
                .map(|&c| self.concept_of[c as usize])
                .collect(),
        };
        let mut engine = Engine::new(&self.program, config);
        let end = engine.run(&seed);
        let model = end == End::Model;
        let answer = answer_of(end, &self.program, &engine);
        let labels =
            (answer == Answer::Consistent && model).then(|| self.labels(&mut engine, want));
        let mut telemetry = engine.stats.clone();
        telemetry.total = started.elapsed();
        ProbeOutcome {
            answer,
            telemetry,
            labels,
        }
    }

    /// The labels of the model `engine` ended in.
    fn labels(&self, engine: &mut Engine<'_>, want: Want) -> Labels {
        engine.recompute_blocking();
        let mut out = Labels::default();
        // The nodes as asserted: `label` follows their merges with what those depend on.
        let probe = if engine.probe == NONE {
            NONE
        } else {
            out.probe = self.label(engine, engine.probe);
            engine.g.find(engine.probe)
        };
        if want.individuals {
            out.individuals = self
                .individual_of
                .iter()
                .map(|&p| (p != NONE).then(|| self.label(engine, engine.roots[p as usize])))
                .collect();
        }
        if want.elements {
            let mut seen: hashbrown::HashSet<Vec<u32>> = hashbrown::HashSet::new();
            for n in 0..engine.g.nodes.len() as u32 {
                let node = &engine.g.nodes[n as usize];
                if n == probe || !node.live() || node.flags & (flag::BLOCKED | flag::CONCRETE) != 0
                {
                    continue;
                }
                let mut classes: Vec<u32> = engine
                    .g
                    .labels(n)
                    .map(|f| self.class_of[f.concept as usize])
                    .filter(|&c| c != NONE)
                    .collect();
                classes.sort_unstable();
                classes.dedup();
                seen.insert(classes);
            }
            out.elements = seen.into_iter().collect();
            out.elements.sort_unstable();
        }
        out
    }

    /// The classes of the node `n` stands for now, and the deterministic ones: derived
    /// with no choice, and the merges from `n` to it with none either (`n` must be the
    /// node as asserted, not its representative, or those merges are missed).
    fn label(&self, engine: &mut Engine<'_>, n: u32) -> Label {
        let (n, merged) = engine.canonical(n);
        let mut label = Label::default();
        for f in engine.g.labels(n) {
            let c = self.class_of[f.concept as usize];
            if c == NONE {
                continue;
            }
            label.classes.push(c);
            if f.dep == DepSetId::EMPTY && merged == DepSetId::EMPTY {
                label.known.push(c);
            }
        }
        label.classes.sort_unstable();
        label.classes.dedup();
        label.known.sort_unstable();
        label.known.dedup();
        label
    }
}
