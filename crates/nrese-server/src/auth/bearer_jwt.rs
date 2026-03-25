use std::collections::BTreeSet;

use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;

use crate::auth::grants::{StringOrMany, grants_from_claim_parts};
use crate::auth::{AccessGrant, authorize_grants, extract_bearer_token};
use crate::error::ApiError;
use crate::policy::PolicyAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtBearerConfig {
    pub shared_secret: String,
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub read_role: String,
    pub admin_role: String,
    pub leeway_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct JwtClaims {
    #[serde(rename = "exp")]
    _exp: usize,
    #[serde(rename = "nbf")]
    _nbf: Option<usize>,
    #[serde(rename = "iss")]
    _iss: Option<String>,
    #[serde(rename = "aud")]
    _aud: Option<StringOrMany>,
    scope: Option<String>,
    scp: Option<StringOrMany>,
    role: Option<String>,
    roles: Option<StringOrMany>,
}

pub fn authorize(
    config: &JwtBearerConfig,
    action: PolicyAction,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let token = extract_bearer_token(headers)?;
    let claims = decode_claims(config, token)?;
    let grants = grants_from_claims(config, &claims);

    if authorize_grants(action, &grants) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "bearer token does not grant access to this endpoint",
        ))
    }
}

fn decode_claims(config: &JwtBearerConfig, token: &str) -> Result<JwtClaims, ApiError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = config.leeway_seconds;
    if let Some(issuer) = &config.issuer {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = &config.audience {
        validation.set_audience(&[audience]);
    }

    decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(config.shared_secret.as_bytes()),
        &validation,
    )
    .map(|token_data| token_data.claims)
    .map_err(|error| ApiError::unauthorized(format!("invalid bearer token: {error}")))
}

fn grants_from_claims(config: &JwtBearerConfig, claims: &JwtClaims) -> BTreeSet<AccessGrant> {
    grants_from_claim_parts(
        &config.read_role,
        &config.admin_role,
        claims.scope.as_deref(),
        claims.scp.as_ref(),
        claims.role.as_deref(),
        claims.roles.as_ref(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{JwtBearerConfig, JwtClaims, StringOrMany, grants_from_claims};
    use crate::auth::AccessGrant;

    #[test]
    fn roles_are_collected_from_scope_and_roles_claims() {
        let config = JwtBearerConfig {
            shared_secret: "secret".to_owned(),
            issuer: None,
            audience: None,
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        };
        let claims = JwtClaims {
            _exp: 4_102_444_800,
            _nbf: None,
            _iss: None,
            _aud: None,
            scope: Some("nrese.read".to_owned()),
            scp: None,
            role: None,
            roles: Some(StringOrMany::Many(vec!["nrese.admin".to_owned()])),
        };

        assert_eq!(
            grants_from_claims(&config, &claims),
            BTreeSet::from([AccessGrant::Read, AccessGrant::Admin])
        );
    }
}
