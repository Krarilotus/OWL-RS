mod bearer_jwt;
mod bearer_static;
mod grants;
mod mtls;
mod oidc_introspection;
pub mod peers;

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
    /// The authentication mode, without its credentials.
    pub fn mode_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BearerStatic(_) => "bearer-static",
            Self::BearerJwt(_) => "bearer-jwt",
            Self::Mtls(_) => "mtls",
            Self::OidcIntrospection(_) => "oidc-introspection",
        }
    }

    /// Checks that the request's credentials allow `action`; returns who it is.
    ///
    /// `also` may allow what the credentials' grants don't (the access state's rules,
    /// [`nrese_store::access::AccessState::grants_read`]).
    pub async fn authorize(
        &self,
        action: PolicyAction,
        headers: &HeaderMap,
        also: &(dyn Fn(&Identity) -> bool + Send + Sync),
    ) -> Result<Identity, ApiError> {
        match self {
            Self::None => Ok(Identity::anonymous()),
            Self::BearerStatic(config) => bearer_static::authorize(config, action, headers, also),
            Self::BearerJwt(config) => bearer_jwt::authorize(config, action, headers, also),
            Self::Mtls(config) => mtls::authorize(config, action, headers, also),
            Self::OidcIntrospection(config) => {
                oidc_introspection::authorize(config, action, headers, also).await
            }
        }
    }
}

/// Who a request is, as its credentials say: an administrator, its role names (claims of
/// a token, `reader` for a static read token, the subject of a client certificate;
/// `anonymous` without authentication) and its user name (a token's subject, a client
/// certificate's subject when it is a valid name). Graph access ([`crate::access`]) is
/// decided by them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub admin: bool,
    pub roles: BTreeSet<String>,
    pub user: Option<String>,
}

impl Identity {
    /// A request without authentication.
    pub fn anonymous() -> Self {
        Self {
            admin: false,
            roles: BTreeSet::from(["anonymous".to_owned()]),
            user: None,
        }
    }

    /// An identity with `grants` and role names `roles`.
    pub fn from_grants(grants: &BTreeSet<AccessGrant>, roles: BTreeSet<String>) -> Self {
        Self {
            admin: grants.contains(&AccessGrant::Admin),
            roles,
            user: None,
        }
    }

    /// This identity named `user`, if that is a valid user name.
    pub fn with_user(mut self, user: Option<&str>) -> Self {
        self.user = user
            .filter(|name| nrese_store::access::valid_user_name(name))
            .map(str::to_owned);
        self
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
