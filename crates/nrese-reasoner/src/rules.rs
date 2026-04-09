use nrese_core::DatasetSnapshot;

use crate::config::ReasonerConfig;
use crate::rules_mvp_cache::{CacheExecutionResult, RulesMvpExecutionCache};

#[cfg(test)]
use crate::output::InferenceDelta;

#[cfg(test)]
pub fn execute_rules_mvp<'a, S>(snapshot: &'a S) -> InferenceDelta
where
    S: DatasetSnapshot<'a>,
{
    execute_rules_mvp_with_cache(
        snapshot,
        &RulesMvpExecutionCache::default(),
        &crate::config::ReasonerConfig::for_mode(crate::config::ReasoningMode::RulesMvp),
    )
    .inferred
}

pub fn execute_rules_mvp_with_cache<'a, S>(
    snapshot: &'a S,
    cache: &RulesMvpExecutionCache,
    config: &ReasonerConfig,
) -> CacheExecutionResult
where
    S: DatasetSnapshot<'a>,
{
    let policy = config
        .rules_mvp_feature_policy()
        .expect("rules-mvp execution requires a rules-mvp profile");
    cache.execute(snapshot, &policy)
}

#[cfg(test)]
#[path = "tests/rules_tests.rs"]
mod tests;
