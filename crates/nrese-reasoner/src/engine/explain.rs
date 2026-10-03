//! Explanations over the engine: violations for reject reports, why a statement holds,
//! its justifications ([`super::explain_fact`], [`super::justify_fact`]).

use nrese_engine::{GraphSelector, ReadModel, Snapshot, TermId, Transaction};

use super::{Program, decoded, pattern, triple};
use crate::delta::Base;
use crate::ir::{Triple, Violation};

/// What an OWL 2 RL consistency rule's violation means, for reject reports.
fn describe(rule: &str) -> &'static str {
    match rule {
        "cax-dw" => "an instance of two disjoint classes",
        "cax-adc" => "an instance of two classes of an owl:AllDisjointClasses axiom",
        "cls-com" => "an instance of a class and of its complement",
        "cls-nothing2" => "an instance of owl:Nothing",
        "cls-maxc1" | "cls-maxqc1" | "cls-maxqc2" => "a maximum cardinality of 0 is exceeded",
        "prp-irp" => "an irreflexive property relates a resource to itself",
        "prp-asyp" => "an asymmetric property holds in both directions",
        "prp-pdw" => "two disjoint properties relate the same pair",
        "prp-adp" => "two properties of an owl:AllDisjointProperties axiom relate the same pair",
        "prp-npa1" | "prp-npa2" => "a negative property assertion is contradicted",
        "eq-diff1" | "eq-diff2" | "eq-diff3" => "resources declared different are the same",
        "dt-not-type" => "a literal is typed with a datatype whose value space doesn't contain it",
        "dt-diff" => "two different data values would be the same",
        _ => "a consistency rule is violated",
    }
}

/// A violation decoded for a reject report: the violated rule's premises under its
/// bindings (the facts that clash), each marked asserted or inferred.
pub fn explain_violation(
    program: &Program,
    violation: &Violation,
    tx: &Transaction<'_>,
) -> crate::RejectExplanation {
    use crate::ir::{Head, Term};
    let decode = |id: u64| decoded(tx.decode(TermId::from_raw(id)), id);
    let value = |term: Term| match term {
        Term::Const(c) => Some(c),
        Term::Var(v) => violation.bindings.get(usize::from(v)).copied(),
    };
    let evidence: Vec<crate::RejectEvidence> = program
        .rules
        .iter()
        .find(|r| r.name == violation.rule && r.head == Head::Inconsistent)
        .map(|rule| {
            rule.body
                .iter()
                .filter_map(|atom| {
                    let [s, p, o] = [value(atom.0[0])?, value(atom.0[1])?, value(atom.0[2])?];
                    let asserted = tx
                        .quads_for_pattern_in(
                            ReadModel::Asserted,
                            &pattern([Some(s), Some(p), Some(o)], GraphSelector::Any),
                        )
                        .next()
                        .is_some();
                    Some(crate::RejectEvidence {
                        role: "premise",
                        subject: decode(s),
                        predicate: decode(p),
                        object: decode(o),
                        origin: if asserted { "asserted" } else { "inferred" }.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // The instance the violation is about: the subject of the last premise (OWL 2 RL
    // rules list schema premises first), else the first binding.
    let focus = evidence
        .last()
        .map(|e| e.subject.clone())
        .or_else(|| violation.bindings.first().map(|&id| decode(id)))
        .unwrap_or_default();
    let premises: Vec<String> = evidence
        .iter()
        .map(|e| format!("{} {} {} ({})", e.subject, e.predicate, e.object, e.origin))
        .collect();
    let summary = if premises.is_empty() {
        let bindings: Vec<String> = violation.bindings.iter().map(|&id| decode(id)).collect();
        format!(
            "{} ({}): {}",
            violation.rule,
            describe(&violation.rule),
            bindings.join(", ")
        )
    } else {
        format!(
            "{} ({}): {}",
            violation.rule,
            describe(&violation.rule),
            premises.join("; ")
        )
    };
    crate::RejectExplanation {
        summary,
        violated_constraint: violation.rule.clone(),
        focus_resource: focus,
        evidence,
    }
}

/// One step of an inference's explanation ([`explain_fact`]), decoded: the fact, whether it
/// is asserted or inferred, the rule that derives it and its premises (indexes of steps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceStep {
    /// The statement's terms (N-Triples); empty for a `hidden` step.
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `asserted`, `inferred`, or `hidden`: asserted in no graph the requester may read
    /// (the step stays, so the proof's shape does, without the statement).
    pub origin: &'static str,
    /// The rule (`cax-sco`, `prp-trp`, ...); `None` for an asserted fact.
    pub rule: Option<String>,
    pub premises: Vec<usize>,
}

/// Facts an explanation examines at most.
const EXPLANATION_BUDGET: usize = 4096;

/// Why the fact `[s, p, o]` holds in `snapshot` under `program`: a derivation of it from
/// asserted facts ([`crate::explain`]), the fact first. `None` if it doesn't
/// hold, or no derivation is found within the budget.
///
/// `readable`, where the requester may not read every graph, tells whether an asserted
/// statement is in a graph it may read: steps on others are `hidden`.
pub fn explain_fact(
    program: &Program,
    snapshot: &Snapshot,
    fact: Triple,
    readable: Option<&dyn Fn(Triple) -> bool>,
) -> Option<Vec<InferenceStep>> {
    let base = SnapshotBase {
        snapshot,
        axioms: &program.axioms,
    };
    let explanation = crate::explain::explain(&base, program.rules(), fact, EXPLANATION_BUDGET)?;
    let decode = |id: u64| decoded(snapshot.decode(TermId::from_raw(id)), id);
    Some(
        explanation
            .steps
            .into_iter()
            .map(|step| {
                let [s, p, o] = step.fact;
                if step.rule.is_none() && readable.is_some_and(|readable| !readable(step.fact)) {
                    return InferenceStep {
                        subject: String::new(),
                        predicate: String::new(),
                        object: String::new(),
                        origin: "hidden",
                        rule: None,
                        premises: step.premises,
                    };
                }
                InferenceStep {
                    subject: decode(s),
                    predicate: decode(p),
                    object: decode(o),
                    origin: if step.rule.is_some() {
                        "inferred"
                    } else {
                        "asserted"
                    },
                    rule: step.rule,
                    premises: step.premises,
                }
            })
            .collect(),
    )
}

/// Which justifications of an inferred statement to compute ([`justify_fact`]), in order
/// of cost (docs/design/owl2-dl.md §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustificationMode {
    /// One minimal set of asserted statements the statement follows from.
    One,
    /// The statements every justification has.
    Core,
    /// The statements some justification has (the relevant ones).
    Union,
    /// The `k` smallest justifications.
    Top(usize),
    /// Every justification, smallest first (up to [`JUSTIFICATIONS_AT_MOST`]).
    All,
}

/// Justifications listed at most (`all`, `top-k`): past it the answer says it isn't
/// complete.
pub const JUSTIFICATIONS_AT_MOST: usize = 1000;

/// Resolution steps a justification enumeration takes at most.
const JUSTIFICATION_BUDGET: usize = 500_000;

/// A statement of a justification, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JustifiedStatement {
    /// The statement's terms (N-Triples); empty when `hidden`.
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `asserted`, or `hidden`: asserted in no graph the requester may read.
    pub origin: &'static str,
}

/// The justifications of an inferred statement ([`justify_fact`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JustificationAnswer {
    pub mode: JustificationMode,
    /// `one`, `top-k`, `all`: the justifications, smallest first; `core`, `union`: one set.
    pub sets: Vec<Vec<JustifiedStatement>>,
    /// Whether the answer is exact: every derivation was collected, and the enumeration
    /// (`top-k`, `all`, `union`) ended within its limits.
    pub complete: bool,
    /// For `one`, `top-k` and `all`: whether a proof of the statement from each listed
    /// justification was checked step by step against the rules (the proof checker:
    /// always, so a `false` is an engine error to report). `None` for `core` and `union`.
    pub verified: Option<bool>,
}

/// The justifications of the fact `[s, p, o]` in `snapshot` under `program`: minimal sets
/// of asserted statements it follows from, computed on its proof graph
/// ([`crate::explain::proof_graph`], the proof IR of `nrese-owl`). `None` if it
/// doesn't hold. `readable` as for [`explain_fact`].
pub fn justify_fact(
    program: &Program,
    snapshot: &Snapshot,
    fact: Triple,
    mode: JustificationMode,
    readable: Option<&dyn Fn(Triple) -> bool>,
) -> Option<JustificationAnswer> {
    let base = SnapshotBase {
        snapshot,
        axioms: &program.axioms,
    };
    let ground = crate::delta::program(&base, program.rules());
    let graph = crate::explain::proof_graph(&base, &ground, fact, EXPLANATION_BUDGET)?;
    let (sets, complete) = match mode {
        JustificationMode::One => (graph.one().into_iter().collect(), graph.complete),
        JustificationMode::Core => (vec![graph.core()], graph.complete),
        JustificationMode::Union => {
            let (union, complete) = graph.union(JUSTIFICATION_BUDGET);
            (vec![union], complete)
        }
        JustificationMode::Top(_) | JustificationMode::All => {
            let limit = match mode {
                JustificationMode::Top(k) => k.min(JUSTIFICATIONS_AT_MOST),
                _ => JUSTIFICATIONS_AT_MOST,
            };
            let found = graph.justifications(limit, JUSTIFICATION_BUDGET);
            // Top-k asks for k: having them is the whole answer.
            let complete = found.complete
                || matches!(mode, JustificationMode::Top(k) if found.found.len() == k && graph.complete);
            (found.found, complete)
        }
    };
    let verified = match mode {
        JustificationMode::Core | JustificationMode::Union => None,
        _ => Some(sets.iter().all(|set| {
            graph
                .proof_from(&|a| set.binary_search(a).is_ok())
                .is_some_and(|proof| {
                    proof
                        .check(&|step| crate::explain::valid_step(&base, &ground, step))
                        .is_ok()
                })
        })),
    };
    let decode = |id: u64| decoded(snapshot.decode(TermId::from_raw(id)), id);
    let statement = |fact: Triple| {
        if readable.is_some_and(|readable| !readable(fact)) {
            return JustifiedStatement {
                subject: String::new(),
                predicate: String::new(),
                object: String::new(),
                origin: "hidden",
            };
        }
        let [s, p, o] = fact;
        JustifiedStatement {
            subject: decode(s),
            predicate: decode(p),
            object: decode(o),
            origin: "asserted",
        }
    };
    Some(JustificationAnswer {
        mode,
        sets: sets
            .into_iter()
            .map(|set| set.into_iter().map(statement).collect())
            .collect(),
        complete,
        verified,
    })
}

/// A committed state as the reasoner reads it: asserted and inferred statements of every
/// graph, the ruleset's axioms counting as asserted.
pub(crate) struct SnapshotBase<'a> {
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) axioms: &'a [Triple],
}

impl Base for SnapshotBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any))
        {
            f(triple(quad));
        }
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let count = self
            .snapshot
            .estimate_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any));
        usize::try_from(count).unwrap_or(usize::MAX)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.snapshot.exists_in(
            ReadModel::Materialised,
            &pattern(fact.map(Some), GraphSelector::Any),
        )
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.axioms.binary_search(&fact).is_ok()
            || self.snapshot.exists_in(
                ReadModel::Asserted,
                &pattern(fact.map(Some), GraphSelector::Any),
            )
    }
}
