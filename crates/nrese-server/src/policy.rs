use std::time::Duration;

use crate::auth::AuthConfig;
pub use crate::auth::{JwtBearerConfig, MtlsConfig, OidcIntrospectionConfig, StaticBearerConfig};
use crate::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    pub auth: AuthConfig,
    pub limits: RequestLimits,
    pub rate_limits: RateLimitConfig,
    pub timeouts: RequestTimeouts,
    pub sparql_parse_error_profile: SparqlParseErrorProfile,
    pub expose_operator_ui: bool,
    pub expose_metrics: bool,
    /// A policy file to import into the access state at the first start
    /// ([`crate::access`]); without one, enforcement stays off until the engine API turns
    /// it on.
    pub access: Option<std::sync::Arc<crate::access::AccessPolicy>>,
    /// What workspace graph prefixes start with (`urn:nrese:` by default): a personal
    /// space is `{base}space/{user}/`, a workspace `{base}workspace/{name}/`.
    pub workspace_base: String,
    /// Whether users of the access state log in with their passwords (`Basic`
    /// credentials, or a session from `POST /api/v1/access/login`), besides the
    /// authentication mode.
    pub local_logins: bool,
    /// The directory administrators import files from by name
    /// (`POST /api/v1/repositories/{id}/import/files`); `None`: no server-side imports.
    pub import_directory: Option<std::path::PathBuf>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auth: AuthConfig::default(),
            limits: RequestLimits::default(),
            rate_limits: RateLimitConfig::default(),
            timeouts: RequestTimeouts::default(),
            sparql_parse_error_profile: SparqlParseErrorProfile::default(),
            expose_operator_ui: true,
            expose_metrics: true,
            access: None,
            workspace_base: "urn:nrese:".to_owned(),
            local_logins: true,
            import_directory: None,
        }
    }
}

impl PolicyConfig {
    pub fn enforce_query_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("query", size, self.limits.max_query_bytes)
    }

    pub fn enforce_update_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("update", size, self.limits.max_update_bytes)
    }

    pub fn enforce_rdf_upload_bytes(&self, size: usize) -> Result<(), ApiError> {
        enforce_size_limit("RDF upload", size, self.limits.max_rdf_upload_bytes)
    }

    pub fn bad_request_for_sparql_parse_error(&self, message: impl Into<String>) -> ApiError {
        match self.sparql_parse_error_profile {
            SparqlParseErrorProfile::ProblemJson => ApiError::bad_request(message),
            SparqlParseErrorProfile::PlainText => ApiError::bad_request_plain_text(message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SparqlParseErrorProfile {
    #[default]
    ProblemJson,
    PlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestLimits {
    pub max_query_bytes: usize,
    /// Bytes of intermediate results one query may hold; 0 is unlimited.
    pub max_query_memory_bytes: usize,
    pub max_update_bytes: usize,
    pub max_rdf_upload_bytes: usize,
}

/// Default per-query memory: 4 GiB.
pub const DEFAULT_QUERY_MEMORY_BYTES: usize = 4 << 30;
/// Default longest query text: 1 MiB.
pub const DEFAULT_MAX_QUERY_BYTES: usize = 1 << 20;
/// Default largest SPARQL update: 16 MiB. Clients that write a whole graph in one update
/// (RDF4J's, and so ResearchSpace) send several megabytes.
pub const DEFAULT_MAX_UPDATE_BYTES: usize = 16 << 20;
/// Default largest RDF payload (Graph Store, TELL, SHACL shapes): 128 MiB, enough for an
/// exported graph of about a million statements. Larger data goes through `load`.
pub const DEFAULT_MAX_RDF_UPLOAD_BYTES: usize = 128 << 20;

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_query_bytes: DEFAULT_MAX_QUERY_BYTES,
            max_query_memory_bytes: DEFAULT_QUERY_MEMORY_BYTES,
            max_update_bytes: DEFAULT_MAX_UPDATE_BYTES,
            max_rdf_upload_bytes: DEFAULT_MAX_RDF_UPLOAD_BYTES,
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
