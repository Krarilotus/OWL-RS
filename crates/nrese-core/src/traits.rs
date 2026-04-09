use crate::error::NreseResult;
use crate::model::TripleRef;
use crate::reasoning::{ReasonerCapability, ReasonerExecutionPlan, ReasoningOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotCoverageStats {
    pub supported_triples: u64,
    pub unsupported_triples: u64,
    pub unsupported_blank_node_subjects: u64,
    pub unsupported_blank_node_objects: u64,
    pub unsupported_literal_objects: u64,
    pub flattened_named_graph_quads: u64,
}

pub trait TripleSource<'a> {
    type Iter: Iterator<Item = TripleRef<'a>>;

    fn triples(&'a self) -> Self::Iter;
}

pub trait DatasetSnapshot<'a>: TripleSource<'a> {
    fn revision(&self) -> u64;

    fn cache_key(&self) -> Option<u64> {
        None
    }

    fn asserted_triple_count(&'a self) -> u64 {
        self.triples().count() as u64
    }

    fn unsupported_triple_count(&self) -> u64 {
        0
    }

    fn coverage_stats(&'a self) -> SnapshotCoverageStats {
        SnapshotCoverageStats {
            supported_triples: self.asserted_triple_count(),
            unsupported_triples: self.unsupported_triple_count(),
            ..SnapshotCoverageStats::default()
        }
    }
}

pub trait ReasonerEngine<'a, S>
where
    S: DatasetSnapshot<'a>,
{
    type Inferred;

    fn profile_name(&self) -> &'static str;

    fn mode_name(&self) -> &'static str;

    fn capabilities(&self) -> &[ReasonerCapability];

    fn plan(&self, snapshot: &'a S) -> NreseResult<ReasonerExecutionPlan>;

    fn run(
        &self,
        snapshot: &'a S,
        plan: &ReasonerExecutionPlan,
    ) -> NreseResult<ReasoningOutput<Self::Inferred>>;
}
