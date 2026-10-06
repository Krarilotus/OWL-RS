//! The reasoner in the store: what a rematerialisation or a commit-path run reports, and
//! the reasoner's engine verbs (`nrese_reasoner::engine`) the store calls. The store
//! decides when to reason; how is the reasoner's.

use std::time::{Duration, Instant};

use nrese_engine::{ReadModel, TermId, Transaction};
use nrese_reasoner::delta;
use nrese_reasoner::eval::GroundProgram;
use nrese_reasoner::ir::Violation;
use nrese_reasoner::lists::ListDiagnostic;

pub use nrese_reasoner::RuleProgram;
pub use nrese_reasoner::engine::{
    Closure, InferenceStep, JUSTIFICATIONS_AT_MOST, JustificationAnswer, JustificationMode,
    JustifiedStatement, Program, decoded, equality_report, explain_fact, explain_violation,
    justify_fact, materialise, materialise_until,
};

/// The most diagnostics a report lists; `diagnostics_total` counts them all.
pub const MAX_REPORTED_DIAGNOSTICS: usize = 100;

/// A part of the ontology the reasoner couldn't use, with its terms decoded. Today these
/// are list axioms (property chains, keys, intersections, unions, enumerations,
/// AllDisjoint/AllDifferent members) whose list is malformed, cyclic or too large: their
/// rules are missing from the closure, and nothing else is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OntologyDiagnostic {
    /// `malformed-list`, `cyclic-list`, `too-many-list-variants` or `list-too-long`.
    pub kind: &'static str,
    /// The OWL 2 RL rules the axiom feeds.
    pub rules: &'static str,
    /// The axiom: its subject, its predicate and the list's first node.
    pub subject: String,
    pub predicate: String,
    pub list: String,
    /// The node the problem is at, for malformed and cyclic lists.
    pub node: Option<String>,
    pub message: String,
}

/// `found`, decoded for reports (at most [`MAX_REPORTED_DIAGNOSTICS`]).
fn decode_diagnostics(
    found: &[ListDiagnostic],
    decode: &dyn Fn(u64) -> String,
) -> Vec<OntologyDiagnostic> {
    found
        .iter()
        .take(MAX_REPORTED_DIAGNOSTICS)
        .map(|d| OntologyDiagnostic {
            kind: d.problem.kind(),
            rules: d.rules,
            subject: decode(d.subject),
            predicate: decode(d.predicate),
            list: decode(d.head),
            node: d.problem.node().map(decode),
            message: d.describe(decode),
        })
        .collect()
}

/// Logs `diagnostics` as warnings: they mean the closure lacks those axioms' rules.
pub(crate) fn log_diagnostics(diagnostics: &[OntologyDiagnostic], total: usize, context: &str) {
    for diagnostic in diagnostics {
        tracing::warn!(kind = diagnostic.kind, "{context}: {}", diagnostic.message);
    }
    if total > diagnostics.len() {
        tracing::warn!(
            omitted = total - diagnostics.len(),
            "{context}: further ontology diagnostics omitted"
        );
    }
}

/// What a rematerialisation or a commit-path run did.
#[derive(Debug, Clone, Default)]
pub struct MaterialisationReport {
    /// The rule program's name ([`RuleProgram::name`]).
    pub ruleset: String,
    /// The revision holding the new inferred stack (for commits: the commit's).
    pub revision: u64,
    pub asserted: u64,
    pub inferred: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub violations: usize,
    /// Ontology parts the reasoner couldn't use: after a rematerialisation all of them,
    /// after a commit those the commit introduced. At most [`MAX_REPORTED_DIAGNOSTICS`].
    pub diagnostics: Vec<OntologyDiagnostic>,
    pub diagnostics_total: usize,
    pub rounds: usize,
    pub elapsed: Duration,
    /// Time per phase of a full materialisation (grounding, rule joins, the modules for
    /// hierarchies, transitive and equivalence properties and equality, merging,
    /// consistency); zero for a commit.
    pub phases: nrese_reasoner::batch::Phases,
    /// A commit made an unnamed class that was left out consumable: its memberships are
    /// missing until the store rematerialises.
    pub needs_rematerialisation: bool,
}

impl MaterialisationReport {
    /// The report's diagnostics, decoding ids with `decode`.
    pub(crate) fn with_diagnostics(
        mut self,
        found: &[ListDiagnostic],
        decode: &dyn Fn(u64) -> String,
    ) -> Self {
        self.diagnostics = decode_diagnostics(found, decode);
        self.diagnostics_total = found.len();
        self
    }
}

/// [`nrese_reasoner::engine::maintain`] on a commit, with its report: the violations the
/// change introduced, what it did, and the ground program to reuse.
pub(crate) fn apply_delta(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: nrese_reasoner::eval::Stop<'_>,
) -> Result<(Vec<Violation>, MaterialisationReport, Option<GroundProgram>), delta::Interrupted> {
    let started = Instant::now();
    let done = nrese_reasoner::engine::maintain(program, ground, tx, stop)?;
    let report = MaterialisationReport {
        ruleset: program.program.name(),
        revision: tx.base().revision() + 1,
        asserted: tx.len_in(ReadModel::Asserted),
        inferred: tx.len_in(ReadModel::Inferred),
        inferred_inserted: done.inserted,
        inferred_deleted: done.removed,
        violations: done.violations.len(),
        rounds: done.rounds,
        elapsed: started.elapsed(),
        needs_rematerialisation: done.needs_rematerialisation,
        ..MaterialisationReport::default()
    }
    .with_diagnostics(&done.diagnostics, &|id| {
        decoded(tx.decode(TermId::from_raw(id)), id)
    });
    Ok((done.violations, report, done.program))
}
