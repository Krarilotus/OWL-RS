use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_reasoner::ReasonerService;
use nrese_store::{StoreMode, StoreService};

use crate::ai::AiSuggestionService;
use crate::error::ApiError;
use crate::policy::PolicyAction;
use crate::policy::PolicyConfig;
use crate::rate_limit::RateLimiter;
use crate::reasoning_runtime::LastReasoningRun;
use crate::runtime_posture::RuntimePosture;
use axum::http::HeaderMap;

#[derive(Clone)]
pub struct AppState {
    store: Arc<StoreService>,
    ready: Arc<AtomicBool>,
    reasoner: Arc<ReasonerService>,
    policy: Arc<PolicyConfig>,
    ai: Arc<AiSuggestionService>,
    rate_limiter: Arc<RateLimiter>,
    last_reasoning_run: Arc<RwLock<Option<LastReasoningRun>>>,
    update_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(
        store: StoreService,
        reasoner: ReasonerService,
        policy: PolicyConfig,
        ai: AiSuggestionService,
    ) -> Self {
        Self {
            store: Arc::new(store),
            ready: Arc::new(AtomicBool::new(false)),
            reasoner: Arc::new(reasoner),
            policy: Arc::new(policy),
            ai: Arc::new(ai),
            rate_limiter: Arc::new(RateLimiter::default()),
            last_reasoning_run: Arc::new(RwLock::new(None)),
            update_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn store(&self) -> Arc<StoreService> {
        Arc::clone(&self.store)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn reasoner_profile_name(&self) -> &'static str {
        self.reasoner.profile_name()
    }

    pub fn reasoner_mode_name(&self) -> &'static str {
        self.reasoner.mode_name()
    }

    pub fn reasoner(&self) -> Arc<ReasonerService> {
        Arc::clone(&self.reasoner)
    }

    pub fn policy(&self) -> Arc<PolicyConfig> {
        Arc::clone(&self.policy)
    }

    pub fn ai(&self) -> Arc<AiSuggestionService> {
        Arc::clone(&self.ai)
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

    pub fn set_last_reasoning_run(&self, run: LastReasoningRun) {
        if let Ok(mut slot) = self.last_reasoning_run.write() {
            *slot = Some(run);
        }
    }

    pub fn last_reasoning_run(&self) -> Option<LastReasoningRun> {
        self.last_reasoning_run
            .read()
            .ok()
            .and_then(|slot| slot.clone())
    }

    pub fn update_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.update_lock)
    }

    pub fn store_mode(&self) -> StoreMode {
        self.store.config().mode
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
