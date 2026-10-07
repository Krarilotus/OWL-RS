//! The hypertableau (docs/design/owl2-dl.md §6): the complete engine for SROIQ(D), the
//! correctness anchor (package 3.3).
//!
//! The calculus is Motik, Shearer and Horrocks's (*Hypertableau Reasoning for Description
//! Logics*, JAIR 2009) over `nrese-owl`'s DL-clauses: the Hyp-rule as compiled
//! hyperresolution joins, the ≥-rule, the ≈-rule with pruning, clashes, and pairwise
//! anywhere blocking (single blocking where the clauses are simple). The engineering is
//! report D's: append-only fact tables with dependency sets and proofs kept apart,
//! 64-byte hot nodes, a trail instead of copies, interned dependency sets, semantic
//! branching and dependency-directed backjumping, each optimisation with a switch.
//!
//! **Answers are three-valued** ([`Answer`]): consistent, inconsistent, or why not
//! (`unsupported`, `gave-up`). Never a wrong answer: what the engine can't take
//! (datatypes, keys, the NI rule) is left out only where that weakens the ontology, so an
//! inconsistency found without it stands and a model found without it is `unsupported`;
//! a budget ends a run as `gave-up`.
//!
//! Datatypes (package 3.5) are concrete nodes checked by the datatype theory
//! (`data.rs`, `crate::datatypes`); keys are DL-safe rules over the named individuals
//! (`keys.rs`). Not yet: satisfiability caching and completion-graph reuse (3.7).

mod blocking;
mod data;
pub(crate) mod depset;
mod engine;
mod expand;
mod graph;
mod hyper;
mod keys;
mod merge;
mod model;
mod ni;
mod portfolio;
mod probe;
mod program;
mod retract;
mod search;
mod telemetry;

use std::time::{Duration, Instant};

use nrese_owl::{Concept, Normalised, Ontology, Options, Term, normalise_with};

pub use model::Model;
pub use portfolio::Cancel;
pub use probe::{At, Base, From, Label, Labels, Prepared, Probe, ProbeOutcome, Want};
pub use program::left_out;
pub use telemetry::Telemetry;

use engine::Engine;
use program::{ConceptName, Head, Program};
use search::{End, Seed, Site};

/// What a run may use, and which optimisations are on (each has a switch: "on" and
/// "off" must give the same answers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// After an alternative failed, take the later ones with its negation.
    pub semantic_branching: bool,
    /// Jump back to the newest branch point a clash depends on (else chronologically).
    pub backjumping: bool,
    /// Any earlier node may block (else only ancestors).
    pub anywhere_blocking: bool,
    /// Single blocking where the clauses allow it (else pairwise always).
    pub single_blocking: bool,
    /// Decide disjunctions before the ≥-rule expands (else the design's order: the
    /// ≥-rule first, disjunctions when nothing else is left).
    pub disjunctions_first: bool,
    /// Recheck only the nodes a change can affect for blocking (else every node from the
    /// lowest changed one).
    pub incremental_blocking: bool,
    /// Compare every blocking pass with a recomputation from scratch, and panic on a
    /// difference (for tests: the oracle of the incremental pass).
    pub check_blocking: bool,
    /// Try a clause's disjuncts that failed less often first, the clause's order on ties
    /// (else always in the clause's order). Known search variance (A/B, 5 October 2026):
    /// the learned order can lead a search to larger models, DL-663 (47 ms vs 21 ms,
    /// 37,000 branch points vs 13,000) and ore_ont_2901 (30 ms vs 27 ms, 8% more nodes);
    /// against those, the W3C suite and the ORE development set take half the time and 13
    /// tests are faster, DL-202, 206 and 664 within the budget they ran out of.
    pub disjunct_learning: bool,
    /// The most nodes a run may create at once.
    pub max_nodes: usize,
    /// The most branch points a run may open (`None`: no limit): a deterministic bound
    /// on the search, for tests that must not depend on the machine's speed.
    pub max_branch_points: Option<u64>,
    /// The ≤-rule leaves out merges that would clash at once (a concept against its
    /// negation, a disjointness clause), with their reasons in its premise: a
    /// pigeonhole of disjoint successors is a clash, not a branch per pair.
    pub merge_filter: bool,
    /// Conflict-guided choice order: each clash raises the activity of the clauses whose
    /// choices it depended on, and the open disjunction of the most active clause is
    /// decided first (else the oldest open one).
    pub conflict_order: bool,
    /// Dynamic backtracking: a clash retracts its culprit level alone and re-decides it on
    /// top, keeping the levels above (docs/design/owl2-dl-dynamic-backtracking.md).
    pub dynamic_backtracking: bool,
    /// After each retraction, assert that nothing alive depends on the retracted level
    /// (on in debug builds; the campaigns set it).
    pub check_retraction: bool,
    /// Sizes of counted classes compared before the search ([`crate::numbers`]): a class
    /// with two sizes refutes the program without a branch.
    pub counting: bool,
    pub timeout: Option<Duration>,
    /// The most memory a run may hold (its tables, indexes and arenas), in bytes.
    pub max_memory: usize,
    /// Keep the model a consistent run found ([`Outcome::model`]).
    pub keep_model: bool,
    /// `≤ n` up to this `n` is spelled out in clauses; above it, at-most atoms
    /// (`nrese_owl::Options`).
    pub expand_at_most_up_to: u32,
    /// Unfold a class's only, acyclic Boolean definition lazily
    /// (`nrese_owl::Options::lazy_definitions`); never when the model is kept, as it
    /// under-approximates such classes. Which clauses decide sooner depends on the
    /// ontology: with [`Config::portfolio`] both run side by side.
    pub lazy_definitions: bool,
    /// Where lazy unfolding changes the clauses and two cores are free, race the plain and
    /// the unfolded clauses (`portfolio`), each with half the memory budget; else the
    /// unfolded clauses alone if this is off, the plain ones if cores are short.
    pub portfolio: bool,
    /// Stops the run at its next budget check, answering `GaveUp`.
    pub cancel: Option<Cancel>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            semantic_branching: true,
            backjumping: true,
            anywhere_blocking: true,
            single_blocking: true,
            disjunctions_first: false,
            disjunct_learning: true,
            incremental_blocking: true,
            check_blocking: false,
            max_nodes: 2_000_000,
            max_branch_points: None,
            merge_filter: true,
            conflict_order: false,
            dynamic_backtracking: false,
            check_retraction: cfg!(debug_assertions),
            counting: true,
            timeout: None,
            max_memory: 4 << 30,
            keep_model: false,
            lazy_definitions: true,
            portfolio: true,
            cancel: None,
            expand_at_most_up_to: Options::default().expand_at_most_up_to,
        }
    }
}

/// Why a run gave up when [`Config::max_branch_points`] ran out.
pub const BRANCH_BUDGET: &str = "the branch-point budget ran out";

/// The answer of a consistency or satisfiability test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Consistent,
    Inconsistent,
    /// No inconsistency found, but parts of the ontology were left out (why).
    Unsupported(String),
    /// A budget ran out, or a rule the engine lacks is needed (why).
    GaveUp(String),
}

impl Answer {
    /// The answer's class as the DL lab writes it.
    pub fn class(&self) -> &'static str {
        match self {
            Answer::Consistent => "consistent",
            Answer::Inconsistent => "inconsistent",
            Answer::Unsupported(_) => "unsupported",
            Answer::GaveUp(_) => "gave-up",
        }
    }
}

/// What the clauses use, for reports by fragment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    /// Edges towards `x` or inverse roles in number restrictions.
    pub inverses: bool,
    /// Equalities, at-most atoms or at-least restrictions above one.
    pub numbers: bool,
    pub nominals: bool,
    /// Disjunctive clauses.
    pub disjunctions: bool,
    /// Data values (the datatype theory) or keys.
    pub datatypes: bool,
    /// Something was left out.
    pub weakened: bool,
}

/// A run's answer with its telemetry (and model, if asked for).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub answer: Answer,
    pub telemetry: Telemetry,
    pub model: Option<Model>,
    pub features: Features,
}

/// Whether `ontology` is consistent.
pub fn consistency(ontology: &Ontology, config: &Config) -> Outcome {
    let ontology = prepared(ontology);
    // The engine reads no provenance: minimal automata.
    let options = |lazy_definitions| Options {
        expand_at_most_up_to: config.expand_at_most_up_to,
        lazy_definitions,
        exact_provenance: false,
    };
    if !config.lazy_definitions || config.keep_model {
        return consistency_of(
            &ontology,
            &normalise_with(&ontology, options(false)),
            config,
        );
    }
    let unfolded = normalise_with(&ontology, options(true));
    if unfolded.unfolded == 0 || !config.portfolio {
        return consistency_of(&ontology, &unfolded, config);
    }
    let plain = normalise_with(&ontology, options(false));
    if !portfolio::cores_to_spare() {
        return consistency_of(&ontology, &plain, config);
    }
    portfolio::race(&ontology, &[&plain, &unfolded], config)
}

/// `ontology` with each negative assertion over a non-simple property `¬R(a, b)` as the
/// equivalent `a : ∀R.¬{b}`, which the normalisation takes through `R`'s automaton.
pub fn prepared(ontology: &Ontology) -> std::borrow::Cow<'_, Ontology> {
    use nrese_owl::{Axiom, ClassExpr, ExprId, ObjProp};
    let non_simple = program::non_simple(ontology);
    let affected = |a: &Axiom| matches!(a, Axiom::NegativeObjectPropertyAssertion(p, _, _) if non_simple.contains(p));
    if !ontology.axioms.iter().any(affected) {
        return std::borrow::Cow::Borrowed(ontology);
    }
    let mut o = ontology.clone();
    for i in 0..o.axioms.len() {
        if let Axiom::NegativeObjectPropertyAssertion(p, a, b) = o.axioms[i] {
            if !non_simple.contains(&p) {
                continue;
            }
            let one = ExprId(o.classes.intern(ClassExpr::OneOf(vec![b])));
            let not = ExprId(o.classes.intern(ClassExpr::Not(one)));
            let all = ExprId(o.classes.intern(ClassExpr::All(ObjProp::Named(p), not)));
            o.axioms[i] = Axiom::ClassAssertion(all, a);
        }
    }
    std::borrow::Cow::Owned(o)
}

/// Whether `ontology`, normalised into `normalised`, is consistent.
pub fn consistency_of(ontology: &Ontology, normalised: &Normalised, config: &Config) -> Outcome {
    run(ontology, normalised, None, config)
}

/// Whether the class `class` is satisfiable in `ontology` (with its ABox): `Consistent`
/// means satisfiable.
pub fn satisfiable(
    ontology: &Ontology,
    normalised: &Normalised,
    class: Term,
    config: &Config,
) -> Outcome {
    run(ontology, normalised, Some(Concept::Named(class)), config)
}

pub(crate) fn features(p: &Program) -> Features {
    let mut f = Features {
        inverses: !p.simple,
        numbers: !p.at_most.is_empty() || p.at_least.iter().any(|n| n.n > 1),
        nominals: p.nominals,
        disjunctions: false,
        datatypes: p.data.is_some() || !p.keys.is_empty(),
        weakened: !p.weakened.is_empty(),
    };
    for c in &p.clauses {
        f.disjunctions |= c.head.len() > 1;
        f.numbers |= c.head.iter().any(|h| matches!(h, Head::Equal(..)));
    }
    f
}

/// The answer a run's end gives: a model is an answer only if nothing was left out.
pub(crate) fn answer_of(end: End, program: &Program, engine: &Engine<'_>) -> Answer {
    match end {
        End::Refuted => Answer::Inconsistent,
        End::GaveUp(why) => Answer::GaveUp(why),
        End::Model if program.weakened.is_empty() && engine.data_approximate.is_none() => {
            Answer::Consistent
        }
        End::Model => {
            let mut why = program.weakened.clone();
            if let Some(a) = &engine.data_approximate {
                why.push(format!("datatypes approximated: {a}"));
            }
            Answer::Unsupported(why.join("; "))
        }
    }
}

fn run(
    ontology: &Ontology,
    normalised: &Normalised,
    test: Option<Concept>,
    config: &Config,
) -> Outcome {
    let started = Instant::now();
    let mut program = Program::compile(ontology, normalised);
    let test = test.map(|c| program.concept(ConceptName::Clause(c)));
    program.ensure_tables();
    let compiled = started.elapsed();
    let features = features(&program);
    let mut engine = Engine::new(&program, config);
    engine.stats.compile = compiled;
    let seed = match test {
        Some(c) => Seed {
            site: Site::Fresh,
            positive: vec![c],
            negative: Vec::new(),
        },
        None => Seed::none(),
    };
    let end = engine.run(&seed);
    let answer = answer_of(end, &program, &engine);
    let model = (config.keep_model && end_is_model(&answer, &program)).then(|| engine.model());
    let mut telemetry = engine.stats.clone();
    telemetry.total = started.elapsed();
    telemetry.bytes_per_hot_node = std::mem::size_of::<graph::HotNode>() as u64;
    telemetry.bytes_per_node = (engine.g.bytes() as u64) / telemetry.peak_nodes.max(1);
    telemetry.dependency_sets = engine.deps.len() as u64;
    Outcome {
        answer,
        telemetry,
        model,
        features,
    }
}

fn end_is_model(answer: &Answer, _program: &Program) -> bool {
    matches!(answer, Answer::Consistent | Answer::Unsupported(_))
}
