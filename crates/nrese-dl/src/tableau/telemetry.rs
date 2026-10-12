//! What a hypertableau run did (docs/design/owl2-dl.md §11, owl2-dl-performance.md §5):
//! per-phase times and the search counters, so that thresholds and gates come from runs,
//! not guesses.

use std::fmt;
use std::time::Duration;

/// Per-phase times and counters of one run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Telemetry {
    /// Compiling the DL-clauses into HT-clauses and join plans.
    pub compile: Duration,
    /// Deterministic saturation: hyperresolution, merges, clash checks.
    pub saturate: Duration,
    /// Computing the blocking status.
    pub blocking: Duration,
    /// The at-least and at-most rules.
    pub expand: Duration,
    /// Choosing disjuncts and backtracking.
    pub search: Duration,
    /// The datatype theory's checks.
    pub datatypes: Duration,
    pub total: Duration,

    pub nodes_created: u64,
    pub peak_nodes: u64,
    pub branch_points: u64,
    /// Backtracks that skipped at least one branch level, and the levels skipped.
    pub backjumps: u64,
    pub levels_skipped: u64,
    pub clashes: u64,
    pub merges: u64,
    /// Applications of the NI rule.
    pub ni_applications: u64,
    /// Nodes whose blocking status was computed, and those found directly blocked.
    pub blocking_tests: u64,
    pub blocking_hits: u64,
    pub clauses_fired: u64,
    /// Levels retracted alone (dynamic backtracking), and searches started again from
    /// the first branch point because a backtrack would have crossed a retraction.
    pub retractions: u64,
    pub restarts: u64,
    /// Join plans run (each new fact runs those its trigger matches).
    pub plans_tried: u64,
    pub facts: u64,
    /// Components of data values checked, and key rule instances applied.
    pub data_checks: u64,
    pub key_firings: u64,
    /// The hot node's size (one cache line) and the amortised bytes per node at the peak
    /// (every table, index and the trail, by capacity).
    pub bytes_per_hot_node: u64,
    pub bytes_per_node: u64,
    pub dependency_sets: u64,
}

impl Telemetry {
    /// Work across sequential attempts; peak sizes remain maxima, not sums.
    pub(crate) fn accumulate(&mut self, other: &Self) {
        macro_rules! sum {
            ($($field:ident),* $(,)?) => { $(self.$field += other.$field;)* };
        }
        sum!(
            compile,
            saturate,
            blocking,
            expand,
            search,
            datatypes,
            total,
            nodes_created,
            branch_points,
            backjumps,
            levels_skipped,
            clashes,
            merges,
            ni_applications,
            blocking_tests,
            blocking_hits,
            clauses_fired,
            retractions,
            restarts,
            plans_tried,
            facts,
            data_checks,
            key_firings
        );
        self.peak_nodes = self.peak_nodes.max(other.peak_nodes);
        self.bytes_per_hot_node = self.bytes_per_hot_node.max(other.bytes_per_hot_node);
        self.bytes_per_node = self.bytes_per_node.max(other.bytes_per_node);
        self.dependency_sets = self.dependency_sets.max(other.dependency_sets);
    }
}

impl fmt::Display for Telemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "compile_ms={:.3} saturate_ms={:.3} blocking_ms={:.3} expand_ms={:.3} search_ms={:.3} \
             datatypes_ms={:.3} total_ms={:.3} nodes_created={} peak_nodes={} branch_points={} \
             backjumps={} levels_skipped={} clashes={} merges={} ni={} blocking_tests={} \
             blocking_hits={} clauses_fired={} retractions={} restarts={} plans_tried={} facts={} data_checks={} \
             key_firings={} \
             bytes_per_hot_node={} bytes_per_node={} dependency_sets={}",
            ms(self.compile),
            ms(self.saturate),
            ms(self.blocking),
            ms(self.expand),
            ms(self.search),
            ms(self.datatypes),
            ms(self.total),
            self.nodes_created,
            self.peak_nodes,
            self.branch_points,
            self.backjumps,
            self.levels_skipped,
            self.clashes,
            self.merges,
            self.ni_applications,
            self.blocking_tests,
            self.blocking_hits,
            self.clauses_fired,
            self.retractions,
            self.restarts,
            self.plans_tried,
            self.facts,
            self.data_checks,
            self.key_firings,
            self.bytes_per_hot_node,
            self.bytes_per_node,
            self.dependency_sets
        )
    }
}
