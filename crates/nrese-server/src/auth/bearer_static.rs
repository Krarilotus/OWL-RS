use std::collections::BTreeSet;

use axum::http::HeaderMap;

use crate::auth::{AccessGrant, Identity, authorize_grants, extract_bearer_token};
use crate::error::ApiError;
use crate::policy::PolicyAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticBearerConfig {
    pub read_token: Option<String>,
    pub admin_token: String,
}

pub fn authorize(
    config: &StaticBearerConfig,
    action: PolicyAction,
    headers: &HeaderMap,
    also: &(dyn Fn(&Identity) -> bool + Send + Sync),
) -> Result<Identity, ApiError> {
    let token = extract_bearer_token(headers)?;
    let grants = grants_for_token(config, token);
    let mut roles = BTreeSet::new();
    if grants.contains(&AccessGrant::Read) {
        roles.insert("reader".to_owned());
    }
    let identity = Identity::from_grants(&grants, roles);

    if authorize_grants(action, &grants) || (!grants.is_empty() && also(&identity)) {
        Ok(identity)
    } else {
        Err(ApiError::forbidden(
            "bearer token does not grant access to this endpoint",
        ))
    }
}

fn grants_for_token(config: &StaticBearerConfig, token: &str) -> BTreeSet<AccessGrant> {
    let mut grants = BTreeSet::new();
    if config.admin_token == token {
        grants.insert(AccessGrant::Admin);
    }
    if config.read_token.as_deref() == Some(token) {
        grants.insert(AccessGrant::Read);
    }
    grants
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{StaticBearerConfig, grants_for_token};
    use crate::auth::AccessGrant;

    #[test]
    fn admin_token_maps_to_admin_grant() {
        let config = StaticBearerConfig {
            read_token: Some("reader".to_owned()),
            admin_token: "admin".to_owned(),
        };

        assert_eq!(
            grants_for_token(&config, "admin"),
            BTreeSet::from([AccessGrant::Admin])
        );
    }
}
