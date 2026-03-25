use axum::http::HeaderMap;
use reqwest::Url;
use serde::Deserialize;

use crate::auth::grants::{StringOrMany, grants_from_claim_parts};
use crate::auth::{authorize_grants, extract_bearer_token};
use crate::error::ApiError;
use crate::policy::PolicyAction;

#[derive(Debug, Clone)]
pub struct OidcIntrospectionConfig {
    pub introspection_url: Url,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub read_role: String,
    pub admin_role: String,
    pub timeout_millis: u64,
    client: reqwest::Client,
}

impl OidcIntrospectionConfig {
    pub fn new(
        introspection_url: Url,
        client_id: Option<String>,
        client_secret: Option<String>,
        read_role: String,
        admin_role: String,
        timeout_millis: u64,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_millis))
            .build()?;

        Ok(Self {
            introspection_url,
            client_id,
            client_secret,
            read_role,
            admin_role,
            timeout_millis,
            client,
        })
    }
}

impl PartialEq for OidcIntrospectionConfig {
    fn eq(&self, other: &Self) -> bool {
        self.introspection_url == other.introspection_url
            && self.client_id == other.client_id
            && self.client_secret == other.client_secret
            && self.read_role == other.read_role
            && self.admin_role == other.admin_role
            && self.timeout_millis == other.timeout_millis
    }
}

impl Eq for OidcIntrospectionConfig {}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    scope: Option<String>,
    scp: Option<StringOrMany>,
    role: Option<String>,
    roles: Option<StringOrMany>,
}

pub async fn authorize(
    config: &OidcIntrospectionConfig,
    action: PolicyAction,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let token = extract_bearer_token(headers)?;
    let response = introspect_token(config, token).await?;
    if !response.active {
        return Err(ApiError::unauthorized(
            "introspection endpoint marked bearer token as inactive",
        ));
    }

    let grants = grants_from_claim_parts(
        &config.read_role,
        &config.admin_role,
        response.scope.as_deref(),
        response.scp.as_ref(),
        response.role.as_deref(),
        response.roles.as_ref(),
    );

    if authorize_grants(action, &grants) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "bearer token does not grant access to this endpoint",
        ))
    }
}

async fn introspect_token(
    config: &OidcIntrospectionConfig,
    token: &str,
) -> Result<IntrospectionResponse, ApiError> {
    let mut request = config
        .client
        .post(config.introspection_url.clone())
        .form(&[("token", token), ("token_type_hint", "access_token")]);

    if let Some(client_id) = &config.client_id {
        request = request.basic_auth(client_id, config.client_secret.as_ref());
    }

    let response = request
        .send()
        .await
        .map_err(|error| ApiError::unauthorized(format!("token introspection failed: {error}")))?;

    if !response.status().is_success() {
        return Err(ApiError::unauthorized(format!(
            "token introspection returned {}",
            response.status()
        )));
    }

    response
        .json::<IntrospectionResponse>()
        .await
        .map_err(|error| ApiError::unauthorized(format!("invalid introspection response: {error}")))
}
