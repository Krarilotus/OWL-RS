//! What a materialisation hands back: the derived facts (a list or the working set's
//! runs), its counts and its phases.

use super::*;

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates ([`Output::Facts`]).
    pub derived: Vec<Triple>,
    /// The same per predicate, as the working set held them ([`Output::Relations`]).
    pub relations: Vec<DerivedRelation>,
    /// Consistency violations on the closure, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<crate::lists::ListDiagnostic>,
    pub rounds: usize,
    /// Rules after grounding, at the end.
    pub ground_rules: usize,
    /// Predicates closed by the transitive module.
    pub transitive: usize,
    /// Time per phase: grounding, rule joins, modules, merging, consistency.
    pub phases: Phases,
    /// What the rounds held and did, in counts.
    pub counters: Counters,
    /// With equality by representatives ([`materialise_representatives_until`]): the
    /// `owl:sameAs` classes, and in how many rounds (the start included) they merged.
    pub classes: EqualityClasses,
    pub merges: usize,
}

/// How a materialisation hands back what it derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Output {
    /// A sorted list of the facts ([`Materialisation::derived`]).
    #[default]
    Facts,
    /// Each predicate's runs as the working set held them ([`Materialisation::relations`]):
    /// a store builds its indexes from them without a list of every fact, sorted again
    /// (`nrese_engine::PredicateRuns`). At CSPA's 324.5 M facts the list and its sort took
    /// 7.8 GB and 6.8 % of the samples, and the store's copies of it twice that.
    Relations,
}

/// One predicate's runs in their selected physical layout. Width is dispatched at each
/// run's iterator/export boundary, never per pair in the solver.
#[derive(Debug, Default)]
pub struct DerivedRelation {
    pub predicate: u64,
    pub by_subject: Vec<nrese_engine::PairChunks>,
    pub by_object: Option<Vec<nrese_engine::PairChunks>>,
}

impl DerivedRelation {
    pub fn len(&self) -> usize {
        self.by_subject
            .iter()
            .map(nrese_engine::PairChunks::len)
            .sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn facts(&self) -> impl Iterator<Item = Triple> + '_ {
        self.by_subject
            .iter()
            .flat_map(nrese_engine::PairChunks::pairs)
            .map(move |[s, o]| [s, self.predicate, o])
    }

    pub fn with_subjects_from(&self, low: u64) -> impl Iterator<Item = Triple> + '_ {
        self.by_subject
            .iter()
            .flat_map(move |run| run.with_first_from(low))
            .map(move |[s, o]| [s, self.predicate, o])
    }

    pub(super) fn attach_ids(&mut self, table: Arc<[u64]>) {
        for run in self
            .by_subject
            .iter_mut()
            .chain(self.by_object.iter_mut().flatten())
        {
            if let nrese_engine::PairChunks::Numbers { ids, .. } = run {
                *ids = Arc::clone(&table);
            }
        }
    }

    /// Wide output adapter: axioms, equality members and listings impose no rank limit.
    pub fn of_facts(facts: Vec<Triple>) -> Vec<Self> {
        let mut keys: Vec<Triple> = facts.into_iter().map(|[s, p, o]| [p, s, o]).collect();
        let kept = nrese_exec::sort::sort_dedup_keys(&mut keys);
        keys.truncate(kept);
        keys.chunk_by(|a, b| a[0] == b[0])
            .map(|group| Self {
                predicate: group[0][0],
                by_subject: vec![nrese_engine::PairChunks::Ids(vec![
                    group.iter().map(|&[_, s, o]| [s, o]).collect(),
                ])],
                by_object: None,
            })
            .collect()
    }
}

/// A materialisation's input: facts, or facts grouped as the store streams them (each
/// predicate once, its `(object, subject)` pairs sorted and distinct).
pub enum Input {
    Facts(Vec<Triple>),
    Grouped(Vec<(u64, Vec<[u64; 2]>)>),
}

/// What [`materialise_representatives_until`] lists as [`Materialisation::derived`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Every fact of the closure over representatives, the rewritten input included.
    Representatives,
    /// The closure over representatives as an inferred stack keeps it beside the input:
    /// the facts not in the input, and those of the input that mention a class.
    Stored,
    /// The closure expanded to every identity, without the input's facts: what the
    /// replacement rules derive.
    Expanded,
}

/// Deterministic counts of a materialisation's rounds, for the guards of
/// `docs/design/performance.md` §0: they don't depend on the machine or the thread count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    /// Per round: the working set's bytes after it.
    pub store_bytes: Vec<StoreBytes>,
    /// Bytes of driver matches the rule jobs copied out of the working set before they
    /// ran (P1-F1: none, they read the runs in place).
    pub driver_bytes_copied: usize,
    /// Per round: how often the rule jobs' candidates were checked against the working
    /// set (P1-F8: once per distinct candidate of a morsel).
    pub probes: Vec<u64>,
    /// Checks that came out of (predicate, subject, object) order within their morsel
    /// (P1-F8: none).
    pub unordered_probes: u64,
    /// Where the checks started a relation's runs from the beginning: once per relation
    /// and morsel, the other checks reading each run forward from the last one's place
    /// (G1; checked one by one, every check searched every run from the root).
    pub probe_starts: u64,
    /// Lookups by object alone in relations kept without their object order (P1-F11:
    /// none).
    pub lookups_without_order: u64,
    /// Per round: the pairs the transitive module read to close its predicates (#11).
    pub closure_pairs: Vec<u64>,
    /// Scans for `sameAs` partners by the equality module (equality by copying, #12).
    pub member_scans: u64,
    /// The pairs the relations' merges wrote into their added runs (both orders): the
    /// merge phase's work.
    pub merged_pairs: u64,
    /// Per round: the complete bindings the rule jobs enumerated, each a candidate fact
    /// per head atom (the joins' work).
    pub bindings: Vec<u64>,
    /// The bindings of all rounds by the name of the rule the jobs' instances come from.
    pub bindings_by_rule: std::collections::BTreeMap<String, u64>,
    /// The bytes of the table that numbers the materialisation's ids densely, held beside
    /// the working set while it runs (`batch::dense`).
    pub ids_bytes: usize,
}

/// The bytes the working set's runs hold (both orders, spare capacity included; a run
/// shared by two roles counted once).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoreBytes {
    /// The store's input, apart.
    pub input: usize,
    /// What was added since, before the last round: the added runs.
    pub added: usize,
    /// The delta where it doesn't share another run.
    pub delta: usize,
}

impl StoreBytes {
    pub fn total(&self) -> usize {
        self.input + self.added + self.delta
    }
}

/// Time spent per phase of [`materialise`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Phases {
    pub load: std::time::Duration,
    pub grounding: std::time::Duration,
    pub joins: std::time::Duration,
    pub modules: std::time::Duration,
    pub merge: std::time::Duration,
    pub consistency: std::time::Duration,
    /// Head facts the rule joins' bindings produced, before any deduplication: with
    /// `new_facts`, the bindings per derived fact (tautological and re-derived ones
    /// included; the fast suite's rule-work cases).
    pub bindings: u64,
    /// Membership probes of the rule joins' facts against the working set.
    pub probes: u64,
    /// The joins' morsels' time summed over threads: enumerating, sorting, probing,
    /// handing over (`eval::Probes::nanos`).
    pub join_parts: [std::time::Duration; 4],
    /// The merge phase's parts: the candidates' new pairs (wall), and summed over the
    /// relations (each on every core): the deltas' object orders, the runs' merges.
    pub merge_parts: [std::time::Duration; 3],
    /// The new facts the rounds added.
    pub new_facts: u64,
    /// Whole materialisations run: one (equality by representatives merges classes
    /// within the rounds since R4; it ran one per pass that derived a new `owl:sameAs`).
    pub passes: u64,
}

impl Phases {
    /// Adds `other`'s times and work to these.
    pub fn add(&mut self, other: &Self) {
        self.load += other.load;
        self.grounding += other.grounding;
        self.joins += other.joins;
        self.modules += other.modules;
        self.merge += other.merge;
        self.consistency += other.consistency;
        self.bindings += other.bindings;
        self.probes += other.probes;
        for (a, b) in self.join_parts.iter_mut().zip(other.join_parts) {
            *a += b;
        }
        for (a, b) in self.merge_parts.iter_mut().zip(other.merge_parts) {
            *a += b;
        }
        self.new_facts += other.new_facts;
        self.passes += other.passes;
    }
}
