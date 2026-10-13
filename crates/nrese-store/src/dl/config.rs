//! The `owl2-dl` mode's settings (docs/design/owl2-dl.md §13), typed here, parsed in
//! `nrese-server/src/config/`.

use std::time::Duration;

/// Which answers a query under `owl2-dl` returns by default (`dl.answers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DlAnswers {
    /// The certain answers where the plan has a complete path, else the sound ones; the
    /// status says which (the owner's decision of 2 October).
    #[default]
    CertainWhereComplete,
    /// The lower bound alone: sound, never checked against the upper bound.
    Sound,
    /// Certain answers or an error: a query whose answers the store can't prove complete
    /// fails with `incomplete`.
    Exact,
}

impl DlAnswers {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CertainWhereComplete => "certain-where-complete",
            Self::Sound => "sound",
            Self::Exact => "exact",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::CertainWhereComplete, Self::Sound, Self::Exact]
            .into_iter()
            .find(|a| a.as_str() == name)
    }
}

/// How commits are checked for consistency under OWL 2 DL (`dl.consistency`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DlConsistency {
    /// In the commit: an inconsistent commit is rejected, as the RL gate rejects.
    #[default]
    Inline,
    /// Not checked: the store's DL status is `unknown`, and so every answer's
    /// completeness (an inconsistent ontology entails everything).
    Off,
}

impl DlConsistency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Off => "off",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Inline, Self::Off]
            .into_iter()
            .find(|c| c.as_str() == name)
    }
}

/// The `owl2-dl` mode's settings. They apply when the reasoner's mode is `owl2-dl`.
///
/// Every budget is a setting and `None` is unlimited (`nrese_dl::limits`: nothing is
/// built into the engines). The defaults and their reasons are in
/// docs/ops/config-reference.md: time keeps the server's safety default, memory comes from
/// the request's envelope, the deterministic budgets are unlimited, the candidates a query
/// checks are bounded so that its answer comes within the query's timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlConfig {
    pub answers: DlAnswers,
    pub consistency: DlConsistency,
    /// Per DL task: a commit's consistency check, a query's exact services together, a
    /// classification (`dl.timeout`). Default 30 minutes.
    pub timeout: Option<Duration>,
    /// What one engine run may hold of its own (`dl.memory`, bytes): context saturation
    /// checks sampled capacity estimates, the tableau its owned capacity at checkpoints.
    /// Default none: a request's envelope bounds its runs together.
    pub memory_bytes: Option<usize>,
    /// Candidate answers a query checks with the exact services at most; the rest are
    /// reported unresolved (`dl.max_candidates`). Default 1,000.
    pub max_candidates: Option<usize>,
    /// Maximum workers of a DL operation (`dl.threads`; 0: inherit its runtime).
    pub threads: usize,
    /// The most nodes one hypertableau run may hold (`dl.max_nodes`). Default none.
    pub max_nodes: Option<usize>,
    /// The most branch points one hypertableau run may open (`dl.max_branch_points`): a
    /// deterministic budget, so a decision doesn't depend on the machine's speed. A run
    /// past it gives no answer (unresolved, sound-only), never a wrong one. Default none.
    pub max_branch_points: Option<u64>,
    /// The most merges one hypertableau run may make (`dl.max_merges`). Default none.
    pub max_merges: Option<u64>,
    /// The entailment tests one explanation runs at most (`dl.explanation_tests`); past
    /// them the justification is known-entailing, not minimal. Default none.
    pub explanation_tests: Option<usize>,
    /// Route thresholds, not budgets: past them the work goes to the hypertableau, no
    /// answer is lost. The Horn lower bound's time in a classification
    /// (`dl.lower_bound_timeout`, default 60 s), and the context core's join sizes
    /// (`dl.max_join`, default 2^20 conclusions; `dl.max_join_steps`, default 2^16 steps).
    pub lower_bound_timeout: Option<Duration>,
    pub max_join: Option<usize>,
    pub max_join_steps: Option<usize>,
}

impl Default for DlConfig {
    fn default() -> Self {
        let routes = nrese_dl::classify::Options::default();
        Self {
            answers: DlAnswers::default(),
            consistency: DlConsistency::default(),
            timeout: Some(Duration::from_secs(30 * 60)),
            memory_bytes: None,
            max_candidates: Some(1_000),
            threads: 0,
            max_nodes: None,
            max_branch_points: None,
            max_merges: None,
            explanation_tests: None,
            lower_bound_timeout: routes.lower_bound_timeout,
            max_join: routes.max_join,
            max_join_steps: routes.max_join_steps,
        }
    }
}

impl DlConfig {
    /// A configured memory budget divided among its workers; none stays none. A
    /// too-small share stays zero.
    pub(crate) fn memory_per_worker(&self, workers: usize) -> Option<usize> {
        self.memory_bytes.map(|bytes| bytes / workers.max(1))
    }

    /// The driver's options for a classification or realisation within `envelope`.
    pub(crate) fn classification_options(
        &self,
        workers: nrese_exec::workers::Workers,
        envelope: Option<nrese_exec::Envelope>,
    ) -> nrese_dl::classify::Options {
        let width = workers.width();
        let mut tableau = self.tableau(self.timeout, self.memory_per_worker(width));
        tableau.memory = envelope;
        nrese_dl::classify::Options {
            workers,
            timeout: self.timeout,
            task_memory: self.memory_per_worker(1),
            tableau,
            lower_bound_timeout: self.lower_bound_timeout,
            max_join: self.max_join,
            max_join_steps: self.max_join_steps,
            ..nrese_dl::classify::Options::default()
        }
    }

    /// The hypertableau's configuration of a DL task under these settings, each test
    /// within `timeout` and `memory`.
    pub fn tableau(
        &self,
        timeout: Option<Duration>,
        memory: Option<usize>,
    ) -> nrese_dl::tableau::Config {
        nrese_dl::tableau::Config {
            timeout,
            max_memory: memory,
            max_nodes: self.max_nodes,
            max_branch_points: self.max_branch_points,
            max_merges: self.max_merges,
            ..nrese_dl::tableau::Config::default()
        }
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn configured_memory_is_shared_by_the_existing_paths_and_none_is_unlimited() {
        let mut config = DlConfig {
            memory_bytes: Some(1024),
            threads: 4,
            ..DlConfig::default()
        };
        let workers = nrese_exec::workers::Workers::new(4).limited(2);
        let options = config.classification_options(workers.clone(), None);
        assert_eq!(options.workers, workers);
        assert_eq!(options.task_memory, Some(1024));
        assert_eq!(options.tableau.max_memory, Some(512));
        config.memory_bytes = None;
        let options = config.classification_options(workers, None);
        assert_eq!(options.task_memory, None);
        assert_eq!(options.tableau.max_memory, None);
        assert_eq!(config.memory_per_worker(4000), None);
        config.memory_bytes = Some(1);
        assert_eq!(
            config.memory_per_worker(4),
            Some(0),
            "a tiny share is not unlimited"
        );
    }
}
