use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_reasoner::ReasonerService;
use nrese_store::{MutationPipeline, ReasoningRunRecord, StoreMode, StoreService};

use crate::ai::AiSuggestionService;
use crate::error::ApiError;
use crate::policy::PolicyAction;
use crate::policy::PolicyConfig;
use crate::rate_limit::RateLimiter;
use crate::runtime_posture::{DeploymentPosture, RuntimePosture};
use axum::http::HeaderMap;

#[derive(Clone)]
pub struct AppState {
    pipeline: Arc<MutationPipeline>,
    ready: Arc<AtomicBool>,
    policy: Arc<PolicyConfig>,
    ai: Arc<AiSuggestionService>,
    deployment_posture: DeploymentPosture,
    rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(
        store: StoreService,
        reasoner: ReasonerService,
        policy: PolicyConfig,
        ai: AiSuggestionService,
        deployment_posture: DeploymentPosture,
    ) -> Self {
        Self {
            pipeline: Arc::new(MutationPipeline::new(Arc::new(store), Arc::new(reasoner))),
            ready: Arc::new(AtomicBool::new(false)),
            policy: Arc::new(policy),
            ai: Arc::new(ai),
            deployment_posture,
            rate_limiter: Arc::new(RateLimiter::default()),
        }
    }

    pub fn store(&self) -> Arc<StoreService> {
        Arc::clone(self.pipeline.store())
    }

    /// The store-owned write path; every mutation goes through it.
    pub fn pipeline(&self) -> Arc<MutationPipeline> {
        Arc::clone(&self.pipeline)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn reasoner_profile_name(&self) -> &'static str {
        self.pipeline.reasoner().profile_name()
    }

    pub fn reasoner_mode_name(&self) -> &'static str {
        self.pipeline.reasoner().mode_name()
    }

    pub fn reasoner_read_model_name(&self) -> &'static str {
        self.pipeline.reasoner().read_model_name()
    }

    pub fn reasoner(&self) -> Arc<ReasonerService> {
        Arc::clone(self.pipeline.reasoner())
    }

    pub fn policy(&self) -> Arc<PolicyConfig> {
        Arc::clone(&self.policy)
    }

    pub fn ai(&self) -> Arc<AiSuggestionService> {
        Arc::clone(&self.ai)
    }

    pub fn deployment_posture(&self) -> DeploymentPosture {
        self.deployment_posture
    }

    pub fn runtime_posture(&self) -> RuntimePosture {
        RuntimePosture::from_state(self)
    }

    pub async fn enforce_policy_action(
        &self,
        action: PolicyAction,
        headers: &HeaderMap,
    ) -> Result<(), ApiError> {
        self.policy.authorize(action, headers).await?;
        self.rate_limiter.enforce(action, self.policy.rate_limits)
    }

    pub fn last_reasoning_run(&self) -> Option<ReasoningRunRecord> {
        self.pipeline.last_reasoning_run()
    }

    pub fn store_mode(&self) -> StoreMode {
        self.pipeline.store().config().mode
    }

    pub fn store_mode_name(&self) -> &'static str {
        match self.store_mode() {
            StoreMode::InMemory => "in-memory",
            StoreMode::OnDisk => "on-disk",
        }
    }

    pub fn durability_name(&self) -> &'static str {
        match self.store_mode() {
            StoreMode::InMemory => "ephemeral",
            StoreMode::OnDisk => "durable",
        }
    }
}
