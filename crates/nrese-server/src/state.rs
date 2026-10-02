use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_reasoner::ReasonerService;
use nrese_store::{MutationPipeline, ReasoningRunRecord, StoreMode, StoreService};

use crate::ai::AiSuggestionService;
use crate::error::ApiError;
use crate::http::request_metrics::RequestMetrics;
use crate::policy::PolicyAction;
use crate::policy::PolicyConfig;
use crate::rate_limit::RateLimiter;
use crate::repositories::{Catalog, DEFAULT_REPOSITORY};
use crate::runtime_posture::{DeploymentPosture, RuntimePosture};
use axum::http::HeaderMap;

#[derive(Clone)]
pub struct AppState {
    /// The repository's write path; a reconfiguration replaces it.
    pipeline: crate::repositories::PipelineSlot,
    ready: Arc<AtomicBool>,
    policy: Arc<PolicyConfig>,
    ai: Arc<AiSuggestionService>,
    deployment_posture: DeploymentPosture,
    rate_limiter: Arc<RateLimiter>,
    request_metrics: Arc<RequestMetrics>,
    repositories: Arc<Catalog>,
    /// Users, workspaces and graph policies ([`crate::access`]), server-wide.
    access: Arc<nrese_store::access::AccessControl>,
    /// The repository this state serves.
    repository: Arc<str>,
    /// Long operations (imports) running or finished ([`nrese_store::jobs`]).
    jobs: Arc<nrese_store::jobs::Jobs>,
}

impl AppState {
    /// [`Self::try_new`]; panics if the access state can't be opened (for tests).
    pub fn new(
        store: StoreService,
        reasoner: ReasonerService,
        policy: PolicyConfig,
        ai: AiSuggestionService,
        deployment_posture: DeploymentPosture,
    ) -> Self {
        Self::try_new(store, reasoner, policy, ai, deployment_posture).expect("access state")
    }

    /// The server's state: the default repository's store, the others found beside it, and
    /// the access state in the system store (`system/` of the data directory, or in memory
    /// with the store), into which a policy file is imported at the first start.
    pub fn try_new(
        store: StoreService,
        reasoner: ReasonerService,
        policy: PolicyConfig,
        ai: AiSuggestionService,
        deployment_posture: DeploymentPosture,
    ) -> anyhow::Result<Self> {
        let access = Arc::new(open_access(store.config(), &policy)?);
        let repositories =
            Arc::new(Catalog::open(store, reasoner).map_err(|error| anyhow::anyhow!(error))?);
        Ok(Self {
            pipeline: Arc::clone(&repositories.default_repository().pipeline),
            ready: Arc::new(AtomicBool::new(false)),
            policy: Arc::new(policy),
            ai: Arc::new(ai),
            deployment_posture,
            rate_limiter: Arc::new(RateLimiter::default()),
            request_metrics: Arc::default(),
            repositories,
            access,
            repository: Arc::from(DEFAULT_REPOSITORY),
            jobs: Arc::default(),
        })
    }

    /// The identity of a local login the request carries: `Basic` credentials of a user
    /// with a password, or a session token. `None` without either (the authentication
    /// mode decides); 401 for wrong ones, 429 after too many.
    async fn local_identity(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<crate::auth::Identity>, ApiError> {
        use base64::Engine;
        if !self.policy.local_logins {
            return Ok(None);
        }
        let Some(value) = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(None);
        };
        let identity = |user: String| crate::auth::Identity {
            admin: false,
            roles: std::collections::BTreeSet::new(),
            user: Some(user),
        };
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|token| token.starts_with(nrese_store::access::SESSION_PREFIX))
        {
            return match self.access.session(token) {
                Some(principal) => Ok(principal.user.map(identity)),
                None => Err(ApiError::unauthorized(
                    "the session has ended; log in again",
                )),
            };
        }
        let Some(encoded) = value.strip_prefix("Basic ") else {
            return Ok(None);
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| ApiError::unauthorized("malformed Basic credentials"))?;
        let (user, password) = decoded
            .split_once(':')
            .ok_or_else(|| ApiError::unauthorized("malformed Basic credentials"))?;
        let (user, password) = (user.to_owned(), password.to_owned());
        let access = Arc::clone(&self.access);
        let principal = tokio::task::spawn_blocking(move || access.login(&user, &password))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        match principal {
            Ok(principal) => Ok(principal.user.map(identity)),
            Err(nrese_store::access::AccessError::Throttled(message)) => {
                Err(ApiError::too_many_requests(message))
            }
            Err(_) => Err(ApiError::unauthorized("wrong user name or password")),
        }
    }

    /// Long operations (imports), server-wide.
    pub fn jobs(&self) -> &Arc<nrese_store::jobs::Jobs> {
        &self.jobs
    }

    /// The access state ([`crate::access`]).
    pub fn access(&self) -> &nrese_store::access::AccessControl {
        &self.access
    }

    /// The id of the repository this state serves.
    pub fn repository_id(&self) -> &str {
        &self.repository
    }

    /// What `identity` may read and write in this state's repository.
    pub fn access_view(&self, identity: &crate::auth::Identity) -> crate::access::AccessView {
        let principal = crate::access::principal(identity);
        let mut view = self.access.view(&principal, &self.repository);
        view.origin = Some(principal.display());
        view
    }

    /// `identity` as the access state's changes see it: an administrator also when the
    /// server has no authentication (every endpoint is open then).
    pub fn access_principal(
        &self,
        identity: &crate::auth::Identity,
    ) -> nrese_store::access::Principal {
        let mut principal = crate::access::principal(identity);
        principal.admin |= matches!(self.policy.auth, crate::auth::AuthConfig::None);
        principal
    }

    /// The repositories ([`nrese_store::catalog`]).
    pub fn repositories(&self) -> &Catalog {
        &self.repositories
    }

    /// This server's state as repository `id` sees it: the default repository's own, or a
    /// copy with that repository's store and RDF4J state. Called on the default's state.
    pub fn for_repository(&self, id: &str) -> Result<Self, ApiError> {
        if id == DEFAULT_REPOSITORY {
            return Ok(self.clone());
        }
        let repository = self
            .repositories
            .get(id)
            .ok_or_else(|| ApiError::not_found(format!("no repository '{id}'")))?;
        Ok(Self {
            pipeline: repository.pipeline,
            repository: Arc::from(id),
            ..self.clone()
        })
    }

    /// Request outcomes and latencies, for `/metrics`.
    pub fn request_metrics(&self) -> &RequestMetrics {
        &self.request_metrics
    }

    pub fn store(&self) -> Arc<StoreService> {
        Arc::clone(self.pipeline.read().store())
    }

    /// The store-owned write path; every mutation goes through it.
    pub fn pipeline(&self) -> Arc<MutationPipeline> {
        Arc::clone(&self.pipeline.read())
    }

    /// The repository's settings: those it was created with, or for the default
    /// repository those changed through the engine API (empty: the server's).
    pub fn repository_settings(&self) -> crate::repository_config::RepositorySettings {
        self.repositories
            .settings(self.repository_id())
            .unwrap_or_default()
    }

    /// Changes this state's repository's settings: its title, and its reasoning, which
    /// takes effect at once (the inferences are recomputed). The default repository's are
    /// kept in the data directory (on disk) and override the configuration's reasoning
    /// at the next start.
    pub fn change_repository_settings(
        &self,
        settings: crate::repository_config::RepositorySettings,
    ) -> Result<(), ApiError> {
        Ok(self.repositories.change(self.repository_id(), settings)?)
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    /// Started and serving strictly: not in reasoning quarantine (see
    /// `nrese_store::reasoning_state`).
    pub fn is_ready(&self) -> bool {
        self.is_started() && !self.is_quarantined()
    }

    /// Startup has finished (the server accepts requests).
    pub fn is_started(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Requests are served once startup has finished, also in quarantine: reads there
    /// serve diagnosis, and writes the repair. Only `/readyz` and health report strict
    /// readiness ([`Self::is_ready`]).
    pub fn ensure_serving(&self) -> Result<(), ApiError> {
        if self.is_started() {
            Ok(())
        } else {
            Err(ApiError::unavailable("server is not ready yet"))
        }
    }

    /// The data is inconsistent under the configured reasoning: readable and repairable,
    /// but not ready.
    pub fn is_quarantined(&self) -> bool {
        matches!(
            self.store().consistency(),
            nrese_store::ConsistencyStatus::Inconsistent { .. }
        )
    }

    pub fn reasoner_profile_name(&self) -> &'static str {
        self.pipeline().reasoner().profile_name()
    }

    pub fn reasoner_mode_name(&self) -> &'static str {
        self.pipeline().reasoner().mode_name()
    }

    pub fn reasoner_read_model_name(&self) -> &'static str {
        self.pipeline().reasoner().read_model_name()
    }

    pub fn reasoner(&self) -> Arc<ReasonerService> {
        Arc::clone(self.pipeline().reasoner())
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
    ) -> Result<crate::auth::Identity, ApiError> {
        let state = self.access.state();
        let also = |identity: &crate::auth::Identity| {
            let principal = crate::access::principal(identity);
            match action {
                PolicyAction::QueryRead
                | PolicyAction::GraphRead
                | PolicyAction::ServiceDescriptionRead => state.grants_read(&principal),
                PolicyAction::UpdateWrite | PolicyAction::GraphWrite | PolicyAction::TellWrite => {
                    state.grants_write(&principal)
                }
                PolicyAction::OperatorRead
                | PolicyAction::AdminWrite
                | PolicyAction::MetricsRead => state.is_admin(&principal),
            }
        };
        let identity = match self.local_identity(headers).await? {
            Some(mut identity) => {
                let principal = crate::access::principal(&identity);
                identity.admin = state.is_admin(&principal);
                let mut grants = std::collections::BTreeSet::from([crate::auth::AccessGrant::Read]);
                if identity.admin {
                    grants.insert(crate::auth::AccessGrant::Admin);
                }
                if !(crate::auth::authorize_grants(action, &grants) || also(&identity)) {
                    return Err(ApiError::forbidden(
                        "the local login does not grant access to this endpoint",
                    ));
                }
                identity
            }
            None => self.policy.auth.authorize(action, headers, &also).await?,
        };
        self.rate_limiter.enforce(action, self.policy.rate_limits)?;
        Ok(identity)
    }

    pub fn last_reasoning_run(&self) -> Option<ReasoningRunRecord> {
        self.pipeline().last_reasoning_run()
    }

    pub fn store_mode(&self) -> StoreMode {
        self.pipeline().store().config().mode
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

/// The access state of a server whose default store has `template`'s settings, with the
/// policy file of `policy` imported if the state is new.
fn open_access(
    template: &nrese_store::StoreConfig,
    policy: &PolicyConfig,
) -> anyhow::Result<nrese_store::access::AccessControl> {
    use anyhow::Context;
    use nrese_store::access::{AccessControl, Change, Principal};
    let config = match template.mode {
        StoreMode::OnDisk => nrese_store::StoreConfig::on_disk(template.data_dir.join("system")),
        StoreMode::InMemory => nrese_store::StoreConfig::in_memory(),
    };
    let store = StoreService::new(config).context("the system store (access state)")?;
    let access = AccessControl::open(store, &policy.workspace_base)
        .map_err(|error| anyhow::anyhow!("the access state: {error}"))?;
    if let Some(file) = &policy.access {
        if access.is_new() {
            let server = Principal {
                user: Some("nrese-server".to_owned()),
                admin: true,
                ..Principal::default()
            };
            access
                .apply(
                    &server,
                    Change::Import((**file).clone()),
                    "the access policy file, at the first start",
                )
                .map_err(|error| anyhow::anyhow!("the access policy file: {error}"))?;
        } else if access.state().export() != **file {
            tracing::warn!(
                "the access policy file differs from the access state, which applies; \
                 import the file with POST /api/v1/access/import to apply it"
            );
        }
    }
    Ok(access)
}
