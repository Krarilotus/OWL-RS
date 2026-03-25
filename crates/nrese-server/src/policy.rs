use std::time::Duration;

use axum::http::HeaderMap;

use crate::auth::AuthConfig;
pub use crate::auth::{JwtBearerConfig, MtlsConfig, OidcIntrospectionConfig, StaticBearerConfig};
use crate::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    pub auth: AuthConfig,
    pub limits: RequestLimits,
    pub rate_limits: RateLimitConfig,
    pub timeouts: RequestTimeouts,
    pub expose_operator_ui: bool,
    pub expose_metrics: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auth: AuthConfig::default(),
            limits: RequestLimits::default(),
            rate_limits: RateLimitConfig::default(),
            timeouts: RequestTimeouts::default(),
            expose_operator_ui: true,
            expose_metrics: true,
        }
    }
}

impl PolicyConfig {
    pub async fn authorize(
        &self,
        action: PolicyAction,
        headers: &HeaderMap,
    ) -> Result<(), ApiError> {
        self.auth.authorize(action, headers).await
    }

    pub fn ensure_operator_ui_enabled(&self) -> Result<(), ApiError> {
        if self.expose_operator_ui {
            Ok(())
        } else {
            Err(ApiError::not_found("operator UI is disabled by policy"))
        }
    }

    pub fn ensure_metrics_enabled(&self) -> Result<(), ApiError> {
        if self.expose_metrics {
            Ok(())
        } else {
            Err(ApiError::not_found(
                "metrics endpoint is disabled by policy",
            ))
        }
    }

    pub fn enforce_query_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("query", size, self.limits.max_query_bytes)
    }

    pub fn enforce_update_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("update", size, self.limits.max_update_bytes)
    }

    pub fn enforce_rdf_upload_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("RDF upload", size, self.limits.max_rdf_upload_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLimits {
    pub max_query_bytes: usize,
    pub max_update_bytes: usize,
    pub max_rdf_upload_bytes: usize,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_query_bytes: 1_048_576,
            max_update_bytes: 1_048_576,
            max_rdf_upload_bytes: 10_485_760,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub window: Duration,
    pub read_requests_per_window: usize,
    pub write_requests_per_window: usize,
    pub admin_requests_per_window: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            read_requests_per_window: 0,
            write_requests_per_window: 0,
            admin_requests_per_window: 0,
        }
    }
}

impl RateLimitConfig {
    pub fn limit_for(self, action: PolicyAction) -> Option<usize> {
        let limit = match action {
            PolicyAction::QueryRead
            | PolicyAction::GraphRead
            | PolicyAction::ServiceDescriptionRead => self.read_requests_per_window,
            PolicyAction::UpdateWrite | PolicyAction::TellWrite | PolicyAction::GraphWrite => {
                self.write_requests_per_window
            }
            PolicyAction::OperatorRead | PolicyAction::AdminWrite | PolicyAction::MetricsRead => {
                self.admin_requests_per_window
            }
        };

        (limit > 0).then_some(limit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTimeouts {
    pub query: Duration,
    pub update: Duration,
    pub graph_read: Duration,
    pub graph_write: Duration,
}

impl Default for RequestTimeouts {
    fn default() -> Self {
        Self {
            query: Duration::from_millis(30_000),
            update: Duration::from_millis(60_000),
            graph_read: Duration::from_millis(30_000),
            graph_write: Duration::from_millis(60_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    QueryRead,
    UpdateWrite,
    TellWrite,
    GraphRead,
    GraphWrite,
    OperatorRead,
    AdminWrite,
    MetricsRead,
    ServiceDescriptionRead,
}

fn enforce_size_limit(kind: &str, size: usize, limit: usize) -> Result<(), ApiError> {
    if size > limit {
        return Err(ApiError::payload_too_large(format!(
            "{kind} payload exceeds policy limit ({size} > {limit} bytes)"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::PolicyAction;
    use crate::auth::{AccessGrant, authorize_grants};

    #[test]
    fn read_grant_authorizes_query_reads() {
        let grants = BTreeSet::from([AccessGrant::Read]);
        assert!(authorize_grants(PolicyAction::QueryRead, &grants));
    }

    #[test]
    fn read_grant_does_not_authorize_update_writes() {
        let grants = BTreeSet::from([AccessGrant::Read]);
        assert!(!authorize_grants(PolicyAction::UpdateWrite, &grants));
    }
}
