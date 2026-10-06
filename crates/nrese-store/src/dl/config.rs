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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlConfig {
    pub answers: DlAnswers,
    pub consistency: DlConsistency,
    /// Per DL task: a commit's consistency check, a query's exact services together, a
    /// classification (`dl.timeout`).
    pub timeout: Duration,
    /// The most memory one DL task may hold, in bytes (`dl.memory`).
    pub memory_bytes: usize,
    /// Candidate answers a query checks with the exact services at most; the rest are
    /// reported unresolved (`dl.max_candidates`).
    pub max_candidates: usize,
    /// Workers of a classification (`dl.threads`; 0: every core).
    pub threads: usize,
    /// The most nodes one hypertableau run may hold (`dl.max_nodes`): a deterministic
    /// budget beside `timeout`.
    pub max_nodes: usize,
    /// The most branch points one hypertableau run may open (`dl.max_branch_points`): a
    /// deterministic budget on its search, so a decision doesn't depend on the machine's
    /// speed. A run past it gives no answer (unresolved, sound-only), never a wrong one.
    pub max_branch_points: u64,
}

impl Default for DlConfig {
    fn default() -> Self {
        Self {
            answers: DlAnswers::default(),
            consistency: DlConsistency::default(),
            timeout: Duration::from_secs(30 * 60),
            memory_bytes: 4 << 30,
            max_candidates: 1_000,
            threads: 0,
            max_nodes: 2_000_000,
            max_branch_points: 1_000_000,
        }
    }
}

impl DlConfig {
    /// The hypertableau's configuration of a DL task under these settings, each test
    /// within `timeout` and `memory`.
    pub fn tableau(&self, timeout: Duration, memory: usize) -> nrese_dl::tableau::Config {
        nrese_dl::tableau::Config {
            timeout: Some(timeout),
            max_memory: memory,
            max_nodes: self.max_nodes,
            max_branch_points: Some(self.max_branch_points),
            ..nrese_dl::tableau::Config::default()
        }
    }

    /// The workers a task gets.
    pub fn workers(&self) -> usize {
        match self.threads {
            0 => std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            n => n,
        }
    }
}
