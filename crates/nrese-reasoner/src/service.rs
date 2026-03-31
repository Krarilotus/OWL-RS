use std::time::Instant;

use nrese_core::{
    DatasetSnapshot, ReasonerCapability, ReasonerEngine, ReasonerExecutionPlan, ReasonerRunMetrics,
    ReasonerRunReport, ReasonerRunStatus, ReasoningOutput,
};

use crate::config::{ReasonerConfig, ReasoningMode};
use crate::output::{InferenceDelta, ReasoningCacheStats};
use crate::profile::profile_for_mode;
use crate::rules::execute_rules_mvp_with_cache;
use crate::rules_mvp_cache::RulesMvpExecutionCache;

#[derive(Debug, Clone)]
pub struct ReasonerService {
    config: ReasonerConfig,
    rules_mvp_cache: std::sync::Arc<RulesMvpExecutionCache>,
}

impl ReasonerService {
    pub fn new(config: ReasonerConfig) -> Self {
        Self {
            config,
            rules_mvp_cache: std::sync::Arc::new(RulesMvpExecutionCache::default()),
        }
    }

    pub fn config(&self) -> &ReasonerConfig {
        &self.config
    }

    pub fn profile_name(&self) -> &'static str {
        self.profile().name
    }

    pub fn mode_name(&self) -> &'static str {
        self.profile().mode
    }

    pub fn rules_mvp_preset(&self) -> crate::RulesMvpPreset {
        self.config.rules_mvp.preset
    }

    pub fn capabilities(&self) -> &'static [ReasonerCapability] {
        self.profile().capabilities
    }

    pub fn rules_mvp_cache_stats(&self) -> ReasoningCacheStats {
        self.rules_mvp_cache.snapshot()
    }

    fn profile(&self) -> crate::profile::ReasonerProfile {
        profile_for_mode(self.config.mode)
    }
}

impl<'a, S> ReasonerEngine<'a, S> for ReasonerService
where
    S: DatasetSnapshot<'a>,
{
    type Inferred = InferenceDelta;

    fn profile_name(&self) -> &'static str {
        ReasonerService::profile_name(self)
    }

    fn mode_name(&self) -> &'static str {
        ReasonerService::mode_name(self)
    }

    fn capabilities(&self) -> &'static [ReasonerCapability] {
        ReasonerService::capabilities(self)
    }

    fn plan(&self, snapshot: &'a S) -> nrese_core::NreseResult<ReasonerExecutionPlan> {
        let revision = snapshot.revision();
        let plan = match self.config.mode {
            ReasoningMode::Disabled => ReasonerExecutionPlan::validation_only(revision),
            ReasoningMode::RulesMvp | ReasoningMode::OwlDlTarget => {
                ReasonerExecutionPlan::full_materialization(revision)
            }
        };

        Ok(plan)
    }

    fn run(
        &self,
        snapshot: &'a S,
        plan: &ReasonerExecutionPlan,
    ) -> nrese_core::NreseResult<ReasoningOutput<Self::Inferred>> {
        let started = Instant::now();
        let asserted_triples = snapshot.asserted_triple_count();

        let (status, notes, inferred) = match self.config.mode {
            ReasoningMode::Disabled => (
                ReasonerRunStatus::Skipped,
                vec!["reasoner mode disabled"],
                InferenceDelta::default(),
            ),
            ReasoningMode::RulesMvp => {
                let cached =
                    execute_rules_mvp_with_cache(snapshot, &self.rules_mvp_cache, &self.config);
                let inferred = cached.inferred;
                let mut notes =
                    vec!["rules-mvp executed bounded RDFS/OWL closure and consistency checks"];
                if inferred.cache.execution_cache_hit {
                    notes.push("rules-mvp reused memoized preparation and inference artifacts");
                } else if inferred.cache.schema_cache_hit {
                    notes.push("rules-mvp reused memoized schema preparation artifacts");
                } else if snapshot.cache_key().is_some() {
                    notes.push("rules-mvp refreshed memoized preparation and inference artifacts");
                }
                let status = if inferred.consistency_violations > 0 {
                    notes.push("rules-mvp rejected update due to consistency violations");
                    ReasonerRunStatus::Rejected
                } else {
                    ReasonerRunStatus::Completed
                };
                if inferred
                    .diagnostics
                    .iter()
                    .any(|message| message.contains("not implemented in rules-mvp"))
                {
                    notes
                        .push("rules-mvp reported deterministic unsupported-construct diagnostics");
                }
                if snapshot.unsupported_triple_count() > 0 {
                    notes.push("rules-mvp skipped unsupported asserted triples");
                }
                if inferred.stats.equality_assertion_count > 0 {
                    notes.push("rules-mvp applied canonical owl:sameAs equality handling");
                }
                if inferred.stats.inferred_equality_link_count > 0 {
                    notes.push(
                        "rules-mvp derived bounded owl:sameAs links from functional or inverse-functional property semantics",
                    );
                }
                if !self
                    .config
                    .rules_mvp
                    .feature_policy
                    .owl_consistency_check_enabled()
                {
                    notes.push(
                        "rules-mvp consistency gates were disabled by external configuration",
                    );
                }
                if !self
                    .config
                    .rules_mvp
                    .feature_policy
                    .unsupported_construct_diagnostics_enabled()
                {
                    notes.push(
                        "rules-mvp unsupported-construct diagnostics were disabled by external configuration",
                    );
                }
                (status, notes, inferred)
            }
            ReasoningMode::OwlDlTarget => (
                ReasonerRunStatus::Skipped,
                vec!["owl-dl target mode scaffolded but not implemented"],
                InferenceDelta::default(),
            ),
        };
        let metrics = ReasonerRunMetrics {
            asserted_triples_seen: asserted_triples,
            inferred_triples_produced: inferred.inferred_triples,
            consistency_violations: inferred.consistency_violations,
            elapsed_millis: started.elapsed().as_millis() as u64,
        };
        let profile = self.profile();

        Ok(ReasoningOutput {
            inferred,
            report: ReasonerRunReport {
                profile: profile.name,
                mode: profile.mode,
                revision: plan.revision,
                status,
                metrics,
                notes,
            },
        })
    }
}

#[cfg(test)]
#[path = "tests/service_tests.rs"]
mod tests;
