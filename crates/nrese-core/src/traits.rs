use crate::error::NreseResult;
use crate::model::TripleRef;
use crate::reasoning::{ReasonerCapability, ReasonerExecutionPlan, ReasoningOutput};

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
}

pub trait ReasonerEngine<'a, S>
where
    S: DatasetSnapshot<'a>,
{
    type Inferred;

    fn profile_name(&self) -> &'static str;

    fn mode_name(&self) -> &'static str;

    fn capabilities(&self) -> &'static [ReasonerCapability];

    fn plan(&self, snapshot: &'a S) -> NreseResult<ReasonerExecutionPlan>;

    fn run(
        &self,
        snapshot: &'a S,
        plan: &ReasonerExecutionPlan,
    ) -> NreseResult<ReasoningOutput<Self::Inferred>>;
}
