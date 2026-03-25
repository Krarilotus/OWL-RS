mod bearer_jwt;
mod bearer_static;
mod grants;
mod mtls;
mod oidc_introspection;

use std::collections::BTreeSet;

use axum::http::{HeaderMap, header};

use crate::error::ApiError;
use crate::policy::PolicyAction;

pub use bearer_jwt::JwtBearerConfig;
pub use bearer_static::StaticBearerConfig;
pub use mtls::MtlsConfig;
pub use oidc_introspection::OidcIntrospectionConfig;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AuthConfig {
    #[default]
    None,
    BearerStatic(StaticBearerConfig),
    BearerJwt(JwtBearerConfig),
    Mtls(MtlsConfig),
    OidcIntrospection(OidcIntrospectionConfig),
}

impl AuthConfig {
    pub async fn authorize(
        &self,
        action: PolicyAction,
        headers: &HeaderMap,
    ) -> Result<(), ApiError> {
        match self {
            Self::None => Ok(()),
            Self::BearerStatic(config) => bearer_static::authorize(config, action, headers),
            Self::BearerJwt(config) => bearer_jwt::authorize(config, action, headers),
            Self::Mtls(config) => mtls::authorize(config, action, headers),
            Self::OidcIntrospection(config) => {
                oidc_introspection::authorize(config, action, headers).await
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessGrant {
    Read,
    Admin,
}

pub fn authorize_grants(action: PolicyAction, grants: &BTreeSet<AccessGrant>) -> bool {
    if grants.contains(&AccessGrant::Admin) {
        return true;
    }

    grants.contains(&AccessGrant::Read)
        && matches!(
            action,
            PolicyAction::QueryRead
                | PolicyAction::GraphRead
                | PolicyAction::ServiceDescriptionRead
        )
}

pub fn extract_bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let header_value = headers
        .get(header::AUTHORIZATION)
        .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    let token = header_value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;

    Ok(token)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AccessGrant, authorize_grants};
    use crate::policy::PolicyAction;

    #[test]
    fn admin_grant_authorizes_any_action() {
        let grants = BTreeSet::from([AccessGrant::Admin]);
        assert!(authorize_grants(PolicyAction::AdminWrite, &grants));
        assert!(authorize_grants(PolicyAction::UpdateWrite, &grants));
    }

    #[test]
    fn read_grant_only_authorizes_read_actions() {
        let grants = BTreeSet::from([AccessGrant::Read]);
        assert!(authorize_grants(PolicyAction::QueryRead, &grants));
        assert!(!authorize_grants(PolicyAction::GraphWrite, &grants));
    }
}
