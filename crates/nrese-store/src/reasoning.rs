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
    JustifiedStatement, Program, decoded, explain_fact, explain_violation, justify_fact,
    materialise, materialise_until,
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
    /// What a commit's maintenance did, in counts (zero for a rematerialisation).
    pub work: nrese_reasoner::delta::policy::Counts,
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
        work: done.work,
        ..MaterialisationReport::default()
    }
    .with_diagnostics(&done.diagnostics, &|id| {
        decoded(tx.decode(TermId::from_raw(id)), id)
    });
    Ok((done.violations, report, done.program))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoreConfig, StoreService};
    use nrese_engine::{EncodedTriple, QuadPattern};
    use nrese_rdf::{GraphName, Literal, NamedNode, Quad, vocab::owl};
    use nrese_reasoner::rulesets::Ruleset;
    use std::collections::HashSet;

    /// `eq-giant-split`'s shape: a class of `size` members (two `owl:sameAs` chains and a
    /// bridge) beside classes of ten. Deleting the bridge splits the class; over a compact
    /// stack, rebuilding the state reads fewer facts than recomputing the class's copies,
    /// so the maintenance policy rebuilds in the commit's transaction (B3a) and the delta
    /// executor never runs. Its stack is the fresh closure, and the commit's work is linear
    /// in the class: its `stop` polls and its changes to the stack, never the class's
    /// pairs. Counts, not times. Before B3a, the delta ran first and was thrown away: its
    /// polls were 2,442 / 10,205 / 38,997 for 20 / 40 / 80 members.
    #[test]
    fn a_split_commit_changes_the_compact_stack_linearly_in_the_class() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let split = |size: usize| -> u64 {
            let store = StoreService::new(StoreConfig {
                equality_by_representatives: true,
                equality_compact: true,
                ..StoreConfig::in_memory()
            })
            .unwrap();
            let ex = |s: String| NamedNode::new_unchecked(format!("http://example.com/{s}"));
            let mut facts = Vec::new();
            let mut class = |name: &str, members: usize| {
                let same = |j: usize| {
                    let [a, b] = [j, j + 1].map(|j| ex(format!("{name}{j}")));
                    Quad::new(a, owl::SAME_AS, b, GraphName::DefaultGraph)
                };
                for j in 0..members {
                    let value = Literal::new_simple_literal(format!("{name}{j}"));
                    let member = ex(format!("{name}{j}"));
                    facts.push(Quad::new(
                        member,
                        ex("val".into()),
                        value,
                        GraphName::DefaultGraph,
                    ));
                    if j + 1 < members {
                        facts.push(same(j));
                    }
                }
                same(members / 2 - 1)
            };
            let bridge = class("g", size);
            for g in 0..20 {
                class(&format!("s{g}_"), 10);
            }
            let mut tx = store.engine().transaction();
            for quad in &facts {
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
            store.rematerialise(Ruleset::Owl2Rl).unwrap();

            let mut tx = store.engine().transaction();
            let config = store.config();
            let program = Program::compile(&Ruleset::Owl2Rl.into(), &|term| tx.intern(term))
                .hiding_unnamed_classes(config.hide_unnamed_classes)
                .by_representatives(config.equality_by_representatives)
                .storing_representatives(config.equality_compact);
            assert!(program.stores_representatives());
            tx.remove(bridge.as_ref());
            let polls = AtomicU64::new(0);
            let (violations, report, _) = apply_delta(&program, None, &mut tx, &|| {
                polls.fetch_add(1, Ordering::Relaxed);
                false
            })
            .unwrap();
            assert!(violations.is_empty());
            let pending = tx.pending_snapshot();
            let stored: HashSet<EncodedTriple> = pending
                .stored()
                .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
                .map(Into::into)
                .collect();
            let fresh: HashSet<EncodedTriple> = materialise(&program, &pending)
                .inferred
                .into_iter()
                .filter(|t| !pending.contains_in(ReadModel::Asserted, &t.in_default_graph()))
                .collect();
            assert_eq!(
                stored, fresh,
                "{size}: the split's stack is the fresh closure"
            );
            assert_eq!(
                report.work.plan,
                nrese_reasoner::delta::policy::Plan::Rebuild,
                "{size}: {report:?}"
            );
            assert!(
                report.inferred_inserted + report.inferred_deleted < 2 * size as u64,
                "{size}: {report:?}"
            );
            polls.into_inner()
        };
        let polls = [20, 40, 80].map(split);
        // Linear work, a + b·size, at most doubles when the class doubles; the class's
        // pairs would quadruple it.
        for pair in polls.windows(2) {
            assert!(
                pair[1] <= 2 * pair[0],
                "polls by size 20, 40, 80: {polls:?}"
            );
        }
    }
}
