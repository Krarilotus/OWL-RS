//! Where a saturation spent its time and what it did (docs/design/owl2-dl.md §4 and §11):
//! the phases' times and the telemetry from which the fallback budgets will be set.
//! Kept apart from the classification, so that equal results compare equal.

use std::fmt::Write as _;
use std::time::Duration;

use super::compile::CompileStats;
use super::state::Counters;

/// The phases and counters of one classification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// `nrese-owl`'s normalisation into DL-clauses.
    pub normalise: Duration,
    /// The DL-clauses into the program (renaming, shapes, indexes).
    pub compile: Duration,
    pub saturate: Duration,
    /// The taxonomy from the contexts.
    pub assemble: Duration,
    pub compiled: CompileStats,
    pub dl_clauses: usize,
    pub concepts: usize,
    pub roles: usize,
    /// Successor functions (existential restrictions).
    pub functions: usize,
    pub contexts_created: usize,
    /// Contexts that were started (some message reached them).
    pub contexts_saturated: usize,
    /// Conclusions of the rules, redundant ones included.
    pub clauses_generated: u64,
    pub clauses_kept: u64,
    pub redundant_forward: u64,
    pub redundant_backward: u64,
    pub hyper_inferences: u64,
    pub pred_inferences: u64,
    /// One for Horn clauses (zero if no clause has a head).
    pub max_clause_head_width: u64,
    pub max_clause_body_width: u64,
    pub successor_edges: u64,
    /// The longest agenda any context had.
    pub peak_agenda_size: u64,
    pub messages: u64,
    pub slots: u64,
    /// Derivations recorded for proofs.
    pub proof_steps: u64,
    /// The Eq rule's copies of clauses onto merged terms (§4's list).
    pub equality_literals_generated: u64,
    /// Inclusion tests of the redundancy checks (per derived clause: `clauses_generated`).
    pub subset_checks: u64,
    /// Merges of neighbour terms the Eq rule made.
    pub merges: u64,
    /// Contexts where a merge of `x` with a neighbour was due and not made, and where a
    /// Pred join was left past its step budget.
    pub contexts_unmerged: u64,
    pub contexts_joins_left: u64,
    pub nominal_contexts: u64,
    pub threads: usize,
    /// The most clauses one context holds (a context shared by many successors grows).
    pub largest_context: u64,
}

impl Profile {
    pub(super) fn add(&mut self, c: &Counters) {
        self.clauses_generated += c.generated;
        self.clauses_kept += c.kept;
        self.redundant_forward += c.forward;
        self.redundant_backward += c.backward;
        self.hyper_inferences += c.hyper;
        self.pred_inferences += c.pred;
        self.successor_edges += c.edges;
        self.messages += c.messages;
        self.slots += c.slots;
        self.equality_literals_generated += c.eq;
        self.merges += c.merges;
        self.subset_checks += c.subset_checks;
        self.peak_agenda_size = self.peak_agenda_size.max(c.peak_agenda);
        self.max_clause_body_width = self.max_clause_body_width.max(c.max_body);
        if c.kept > 0 {
            self.max_clause_head_width = 1;
        }
    }

    /// `name=value` pairs, times in milliseconds (the DL lab's `profile` line).
    pub fn line(&self) -> String {
        let ms = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1000.0);
        let mut out = String::new();
        let _ = write!(
            out,
            "normalise={} compile={} saturate={} assemble={} dl_clauses={} concepts={} roles={} \
             functions={} renamed={} split={} recentred={} dropped_data={} contexts_created={} \
             contexts_saturated={} clauses_generated={} clauses_kept={} redundant_forward={} \
             redundant_backward={} hyper={} pred={} max_head={} max_body={} edges={} \
             peak_agenda={} messages={} slots={} proof_steps={} threads={} largest_context={} \
             merges={} eq={} unmerged={} joins_left={} subset_checks={}",
            ms(self.normalise),
            ms(self.compile),
            ms(self.saturate),
            ms(self.assemble),
            self.dl_clauses,
            self.concepts,
            self.roles,
            self.functions,
            self.compiled.renamed,
            self.compiled.split,
            self.compiled.recentred,
            self.compiled.dropped_data,
            self.contexts_created,
            self.contexts_saturated,
            self.clauses_generated,
            self.clauses_kept,
            self.redundant_forward,
            self.redundant_backward,
            self.hyper_inferences,
            self.pred_inferences,
            self.max_clause_head_width,
            self.max_clause_body_width,
            self.successor_edges,
            self.peak_agenda_size,
            self.messages,
            self.slots,
            self.proof_steps,
            self.threads,
            self.largest_context,
            self.merges,
            self.equality_literals_generated,
            self.contexts_unmerged,
            self.contexts_joins_left,
            self.subset_checks,
        );
        out
    }
}
