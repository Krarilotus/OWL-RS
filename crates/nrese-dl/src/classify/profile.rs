//! What a classification or realisation did: per-phase times and the counters that show
//! where tests were saved (docs/design/owl2-dl.md §11). Kept apart from the result, so
//! that equal results compare equal.

use std::fmt::Write as _;
use std::time::Duration;

use crate::tableau::Telemetry;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// `context-core` (the Horn stage took the ontology) or `tableau`.
    pub path: &'static str,
    pub normalise: Duration,
    /// The hypertableau programs' compilation.
    pub compile: Duration,
    /// The Horn part's saturation (or, on the context-core path, all of it).
    pub lower_bound: Duration,
    pub consistency: Duration,
    pub satisfiability: Duration,
    pub top: Duration,
    pub subsumption: Duration,
    /// Realisation's own phase after the classification.
    pub realisation: Duration,
    pub total: Duration,
    pub threads: usize,
    pub classes: usize,
    /// Whether the tests ran without the individuals.
    pub tbox_only: bool,
    /// Subsumptions and unsatisfiable classes the Horn part proved.
    pub lower_known: usize,
    pub lower_unsat: usize,
    /// Hypertableau runs, of them satisfiability tests, and classes that needed none.
    pub tests: u64,
    pub sat_tests: u64,
    pub sat_skipped: usize,
    /// Labels taken from models (each prunes possible subsumers).
    pub labels_seen: u64,
    /// Candidates left after the known and possible subsumers, tests on them, the
    /// subsumptions they proved, and candidates ruled out without a test.
    pub candidates: u64,
    pub candidate_tests: u64,
    pub positive: u64,
    pub pruned: u64,
    /// Realisation: possible types left to test, tests, types proven.
    pub type_candidates: u64,
    pub type_tests: u64,
    pub type_positive: u64,
    /// Summed over the tests.
    pub nodes_created: u64,
    pub branch_points: u64,
    pub clashes: u64,
}

impl Profile {
    pub(crate) fn add(&mut self, t: &Telemetry) {
        self.nodes_created += t.nodes_created;
        self.branch_points += t.branch_points;
        self.clashes += t.clashes;
    }

    /// `name=value` pairs, times in ms.
    pub fn line(&self) -> String {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let mut s = String::new();
        let _ = write!(
            s,
            "path={} normalise={:.1} compile={:.1} lower_bound={:.1} consistency={:.1} \
             satisfiability={:.1} top={:.1} subsumption={:.1} realisation={:.1} total={:.1} \
             threads={} classes={} tbox_only={} lower_known={} lower_unsat={} tests={} \
             sat_tests={} sat_skipped={} labels_seen={} candidates={} candidate_tests={} \
             positive={} pruned={} type_candidates={} type_tests={} type_positive={} \
             nodes_created={} branch_points={} clashes={}",
            self.path,
            ms(self.normalise),
            ms(self.compile),
            ms(self.lower_bound),
            ms(self.consistency),
            ms(self.satisfiability),
            ms(self.top),
            ms(self.subsumption),
            ms(self.realisation),
            ms(self.total),
            self.threads,
            self.classes,
            self.tbox_only,
            self.lower_known,
            self.lower_unsat,
            self.tests,
            self.sat_tests,
            self.sat_skipped,
            self.labels_seen,
            self.candidates,
            self.candidate_tests,
            self.positive,
            self.pruned,
            self.type_candidates,
            self.type_tests,
            self.type_positive,
            self.nodes_created,
            self.branch_points,
            self.clashes
        );
        s
    }
}
