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
//! Not yet: datatypes (package 3.5),
//! satisfiability caching and completion-graph reuse (3.7).

mod blocking;
mod depset;
mod engine;
mod expand;
mod graph;
mod hyper;
mod merge;
mod model;
mod ni;
mod program;
mod search;
mod telemetry;

use std::time::{Duration, Instant};

use nrese_owl::{Concept, Normalised, Ontology, Options, Term, normalise_with};

pub use model::Model;
pub use telemetry::Telemetry;

use engine::Engine;
use program::{ConceptName, Head, Program};
use search::End;

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
    /// Try a clause's disjuncts that failed less often first, at-least restrictions over a
    /// positive filler last (else in the clause's order).
    pub disjunct_learning: bool,
    /// The most nodes a run may create at once.
    pub max_nodes: usize,
    pub timeout: Option<Duration>,
    /// The most memory a run may hold (its tables, indexes and arenas), in bytes.
    pub max_memory: usize,
    /// Keep the model a consistent run found ([`Outcome::model`]).
    pub keep_model: bool,
    /// `≤ n` up to this `n` is spelled out in clauses; above it, at-most atoms
    /// (`nrese_owl::Options`).
    pub expand_at_most_up_to: u32,
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
            timeout: None,
            max_memory: 4 << 30,
            keep_model: false,
            expand_at_most_up_to: Options::default().expand_at_most_up_to,
        }
    }
}

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
    let normalised = normalise_with(
        &ontology,
        Options {
            expand_at_most_up_to: config.expand_at_most_up_to,
        },
    );
    consistency_of(&ontology, &normalised, config)
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

fn features(p: &Program) -> Features {
    let mut f = Features {
        inverses: !p.simple,
        numbers: !p.at_most.is_empty() || p.at_least.iter().any(|n| n.n > 1),
        nominals: p.nominals,
        disjunctions: false,
        weakened: !p.weakened.is_empty(),
    };
    for c in &p.clauses {
        f.disjunctions |= c.head.len() > 1;
        f.numbers |= c.head.iter().any(|h| matches!(h, Head::Equal(..)));
    }
    f
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
    let end = engine.run(test);
    let answer = match end {
        End::Refuted => Answer::Inconsistent,
        End::GaveUp(why) => Answer::GaveUp(why),
        End::Model if program.weakened.is_empty() => Answer::Consistent,
        End::Model => Answer::Unsupported(program.weakened.join("; ")),
    };
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

fn end_is_model(answer: &Answer, program: &Program) -> bool {
    matches!(answer, Answer::Consistent)
        || (matches!(answer, Answer::Unsupported(_)) && !program.weakened.is_empty())
}
