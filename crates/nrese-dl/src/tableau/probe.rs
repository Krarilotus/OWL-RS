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
    /// Whether the probed element's part of the model reaches an individual
    /// ([`Labels::detached`]); with it, `elements` holds that part's elements only.
    pub detached: bool,
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
    /// If asked for ([`Want::detached`]): the probed element's connected part of the
    /// model (by edges, parents and merges) holds no individual. Such a part, with any
    /// model of the individuals beside it as a disjoint union, satisfies every clause:
    /// clause bodies are trees joined by role atoms, a nominal in a head would have merged
    /// an element into its individual, and guards hold at individuals only. So a model of
    /// the clauses without the assertions is then one with them, as far as the probed
    /// element goes.
    pub detached: bool,
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
        let seed = self.seed(probe);
        let mut engine = Engine::new(&self.program, config);
        let end = engine.run(&seed);
        self.outcome(&mut engine, end, want, started, None)
    }

    /// The engine's test for `probe`.
    fn seed(&self, probe: &Probe<'_>) -> Seed {
        let site = match probe.at {
            At::Nothing => Site::Nothing,
            At::Fresh => Site::Fresh,
            At::Individual(i) => match self.individual_of[i as usize] {
                NONE => Site::Fresh,
                p => Site::Individual(p),
            },
        };
        Seed {
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
        }
    }

    /// A run's answer, telemetry and labels.
    fn outcome(
        &self,
        engine: &mut Engine<'_>,
        end: End,
        want: Want,
        started: Instant,
        since: Option<(u32, u32)>,
    ) -> ProbeOutcome {
        let model = end == End::Model;
        let answer = answer_of(end, &self.program, engine);
        let labels =
            (answer == Answer::Consistent && model).then(|| self.labels_since(engine, want, since));
        let mut telemetry = engine.stats.clone();
        telemetry.total = started.elapsed();
        ProbeOutcome {
            answer,
            telemetry,
            labels,
        }
    }

    /// The base of probes on this program's individuals ([`Base`]); `None` where the
    /// program has none, or their consistency isn't a model (refuted, out of budget).
    pub fn base<'p>(&'p self, config: &'p Config) -> Option<Base<'p>> {
        if self.program.individuals.is_empty() {
            return None;
        }
        let mut engine = Engine::new(&self.program, config);
        engine.init(&Seed::none()).ok()?;
        engine.saturate().ok()?;
        let deterministic = engine.clone();
        let end = engine.search(None);
        let model = (end == End::Model).then_some(engine);
        let point = model.as_ref().map(|m| (m.checkpoint(), m.frames.len()));
        Some(Base {
            prepared: self,
            deterministic,
            model,
            point,
            pool: std::sync::Mutex::new(Vec::new()),
        })
    }
    /// The labels of the model `engine` ended in. With `since` (a base's node and
    /// unary-fact counts), the other elements are only those the run added or gave new
    /// facts: the base's own elements were read once ([`Base::labels`]), and leaving
    /// labels out only prunes less.
    fn labels_since(
        &self,
        engine: &mut Engine<'_>,
        want: Want,
        since: Option<(u32, u32)>,
    ) -> Labels {
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
        let part = (want.detached && probe != NONE).then(|| self.part(engine, probe));
        if let Some(part) = &part {
            out.detached = !part.iter().enumerate().any(|(n, &inside)| {
                inside && engine.g.nodes[n].named != NONE && engine.g.nodes[n].live()
            });
        }
        if want.elements {
            let changed: Option<Vec<bool>> = since.map(|(nodes, unary)| {
                let mut c = vec![false; engine.g.nodes.len()];
                for slot in c.iter_mut().skip(nodes as usize) {
                    *slot = true;
                }
                for f in &engine.g.unary[(unary as usize).min(engine.g.unary.len())..] {
                    if let Some(slot) = c.get_mut(f.node as usize) {
                        *slot = true;
                    }
                }
                c
            });
            let mut seen: hashbrown::HashSet<Vec<u32>> = hashbrown::HashSet::new();
            for n in 0..engine.g.nodes.len() as u32 {
                if changed.as_ref().is_some_and(|c| !c[n as usize]) {
                    continue;
                }
                let node = &engine.g.nodes[n as usize];
                if n == probe || !node.live() || node.flags & (flag::BLOCKED | flag::CONCRETE) != 0
                {
                    continue;
                }
                if part.as_ref().is_some_and(|p| !p[n as usize]) {
                    // Outside the probed part: individuals without their assertions.
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

    /// The nodes connected to `probe` (a representative) by edges and parents, over
    /// representatives.
    fn part(&self, engine: &Engine<'_>, probe: u32) -> Vec<bool> {
        let g = &engine.g;
        let len = g.nodes.len();
        let mut parent: Vec<u32> = (0..len as u32).collect();
        fn root(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        let join = |p: &mut Vec<u32>, a: u32, b: u32| {
            let (a, b) = (root(p, a), root(p, b));
            if a != b {
                p[a as usize] = b;
            }
        };
        for n in 0..len as u32 {
            let node = &g.nodes[n as usize];
            let me = g.find(n);
            if node.parent != NONE {
                join(&mut parent, me, g.find(node.parent));
            }
        }
        for e in &g.edges {
            join(&mut parent, g.find(e.from), g.find(e.to));
        }
        let mine = root(&mut parent, probe);
        (0..len as u32)
            .map(|n| {
                let r = g.find(n);
                root(&mut parent, r) == mine
            })
            .collect()
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

/// Completion-graph reuse for probes on a program with individuals (Steigmiller's thesis,
/// §7.1: Det-C, and the model of the consistency test as a base): the individuals' part is
/// built once, and each probe starts from a copy.
///
/// - **From the model** of the individuals: the probe's concepts are added and the search
///   goes on above the model's choices ([`Engine::floor`]). A refutation that depends on
///   none of them stands (the probe's concepts and the deterministic facts hold in every
///   branch); a model is a model. Where a clash would undo a choice of the base, the run
///   stops and the probe goes on:
/// - **from the deterministic state:** the assertions saturated before any choice, a
///   complete search from there.
pub struct Base<'p> {
    prepared: &'p Prepared,
    deterministic: Engine<'p>,
    model: Option<Engine<'p>>,
    /// The model's state to roll back to, and its open branch points.
    point: Option<(super::engine::Frame, usize)>,
    /// Engines at the model's state, one per probe running at once: a probe runs on one
    /// and rolls it back afterwards (the trail undoes it, as a backtrack does), instead
    /// of copying the model each time.
    pool: std::sync::Mutex<Vec<Engine<'p>>>,
}

/// How a probe from a base was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    Model,
    Deterministic,
}

impl Base<'_> {
    /// The labels of the individuals' model (`None` if it isn't one): read once, so
    /// that probes from it read only what they change.
    pub fn labels(&self, want: Want) -> Option<Labels> {
        let mut engine = self.model.as_ref()?.clone();
        let started = Instant::now();
        let out = self
            .prepared
            .outcome(&mut engine, End::Model, want, started, None);
        out.labels
    }

    /// Runs `probe` from the base (its configuration is the base's).
    pub fn probe(&self, probe: &Probe<'_>, want: Want) -> (ProbeOutcome, From) {
        let started = Instant::now();
        let seed = self.prepared.seed(probe);
        fn fresh<'q>(base: &Engine<'q>) -> Engine<'q> {
            let mut engine = base.clone();
            engine.started = Instant::now();
            engine.stats = Telemetry::default();
            engine
        }
        if let (Some(model), Some((point, frames))) = (&self.model, &self.point) {
            let pooled = self
                .pool
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop();
            let mut engine = match pooled {
                Some(mut e) => {
                    e.started = Instant::now();
                    e.stats = Telemetry::default();
                    e
                }
                None => fresh(model),
            };
            let approximate = engine.data_approximate.clone();
            // The base's levels: its top frame's and below.
            engine.floor = engine.frames.last().map_or(0, |f| f.id);
            let since = (model.g.nodes.len() as u32, model.g.unary.len() as u32);
            let end = engine.resume(&seed);
            let done = end != End::GaveUp(super::search::FLOOR.into());
            let out = done.then(|| {
                self.prepared
                    .outcome(&mut engine, end, want, started, Some(since))
            });
            engine.rollback(point, *frames);
            engine.data_approximate = approximate;
            engine.floor = 0;
            self.pool
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(engine);
            if let Some(out) = out {
                return (out, From::Model);
            }
        }
        let mut engine = fresh(&self.deterministic);
        let end = engine.resume(&seed);
        let out = self.prepared.outcome(&mut engine, end, want, started, None);
        (out, From::Deterministic)
    }
}
