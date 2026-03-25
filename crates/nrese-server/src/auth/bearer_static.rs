use std::collections::BTreeSet;

use axum::http::HeaderMap;

use crate::auth::{AccessGrant, authorize_grants, extract_bearer_token};
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
) -> Result<(), ApiError> {
    let token = extract_bearer_token(headers)?;
    let grants = grants_for_token(config, token);

    if authorize_grants(action, &grants) {
        Ok(())
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
