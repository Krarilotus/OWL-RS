mod support;

use std::collections::BTreeSet;
use std::net::SocketAddr;

use axum::Json;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::post;
use axum::{Router, extract::Form};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use tower::util::ServiceExt;

use nrese_server::auth::{
    AuthConfig, JwtBearerConfig, MtlsConfig, OidcIntrospectionConfig, StaticBearerConfig,
};
use nrese_server::policy::{PolicyConfig, RateLimitConfig, RequestLimits};

use support::{test_app, test_app_with_policy};

#[tokio::test]
async fn operator_diagnostics_requires_auth_when_static_bearer_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(static_policy())?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/runtime")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn query_endpoint_requires_auth_when_static_bearer_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(static_policy())?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn metrics_endpoint_can_be_disabled_by_policy() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        expose_metrics: false,
        ..PolicyConfig::default()
    })?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn query_endpoint_rejects_payloads_above_policy_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        limits: RequestLimits {
            max_query_bytes: 8,
            ..RequestLimits::default()
        },
        ..PolicyConfig::default()
    })?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query")
                .method(Method::POST)
                .header("content-type", "application/sparql-query")
                .body(Body::from("SELECT * WHERE { ?s ?p ?o }"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn update_endpoint_rejects_payloads_above_policy_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        limits: RequestLimits {
            max_update_bytes: 8,
            ..RequestLimits::default()
        },
        ..PolicyConfig::default()
    })?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    Ok(())
}

#[tokio::test]
async fn graph_store_put_rejects_payloads_above_policy_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        limits: RequestLimits {
            max_rdf_upload_bytes: 8,
            ..RequestLimits::default()
        },
        ..PolicyConfig::default()
    })?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:s ex:p ex:o .",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    Ok(())
}

#[tokio::test]
async fn query_endpoint_enforces_rate_limit_policy() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(PolicyConfig {
        rate_limits: RateLimitConfig {
            read_requests_per_window: 1,
            ..RateLimitConfig::default()
        },
        ..PolicyConfig::default()
    })?;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );

    Ok(())
}

#[tokio::test]
async fn graph_write_endpoint_enforces_rate_limit_policy() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app_with_policy(PolicyConfig {
        rate_limits: RateLimitConfig {
            write_requests_per_window: 1,
            ..RateLimitConfig::default()
        },
        ..PolicyConfig::default()
    })?;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:s ex:p ex:o .",
                ))?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);

    let second = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?default")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:s2 ex:p ex:o2 .",
                ))?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );

    Ok(())
}

#[tokio::test]
async fn invalid_update_returns_problem_json() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("content-type", "application/unsupported")
                .body(Body::from("abc"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    Ok(())
}

#[tokio::test]
async fn bearer_jwt_allows_read_role_on_query_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(jwt_policy())?;
    let token = test_jwt(&["nrese.read"]);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn bearer_jwt_rejects_read_role_on_write_endpoint() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app_with_policy(jwt_policy())?;
    let token = test_jwt(&["nrese.read"]);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn bearer_jwt_allows_admin_role_on_admin_endpoint() -> Result<(), Box<dyn std::error::Error>>
{
    let app = test_app_with_policy(jwt_policy())?;
    let token = test_jwt(&["nrese.admin"]);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/runtime")
                .method(Method::GET)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn bearer_jwt_rejects_invalid_signature() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(jwt_policy())?;
    let token = encode(
        &Header::default(),
        &TestClaims::new(vec!["nrese.read".to_owned()]),
        &EncodingKey::from_secret(b"wrong-secret"),
    )?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn mtls_allows_read_subject_on_query_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(mtls_policy())?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .header("x-client-cert-subject", "CN=reader-1,O=Test")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn mtls_rejects_read_subject_on_write_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(mtls_policy())?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("x-client-cert-subject", "CN=reader-1,O=Test")
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn mtls_allows_admin_subject_on_admin_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(mtls_policy())?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/runtime")
                .method(Method::GET)
                .header("x-client-cert-subject", "CN=admin-1,O=Test")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn mtls_rejects_missing_subject_header() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(mtls_policy())?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn mtls_rejects_unknown_subject() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app_with_policy(mtls_policy())?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .header("x-client-cert-subject", "CN=unknown,O=Test")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn oidc_introspection_allows_read_role_on_query_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockIntrospectionServer::spawn(IntrospectionReply {
        active: true,
        scope: Some("nrese.read".to_owned()),
        scp: None,
        role: None,
        roles: None,
    })
    .await?;
    let app = test_app_with_policy(oidc_policy(server.url())?)?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=ASK%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .method(Method::GET)
                .header("authorization", "Bearer oidc-read-token")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn oidc_introspection_rejects_read_role_on_write_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockIntrospectionServer::spawn(IntrospectionReply {
        active: true,
        scope: Some("nrese.read".to_owned()),
        scp: None,
        role: None,
        roles: None,
    })
    .await?;
    let app = test_app_with_policy(oidc_policy(server.url())?)?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/update")
                .method(Method::POST)
                .header("authorization", "Bearer oidc-read-token")
                .header("content-type", "application/sparql-update")
                .body(Body::from(
                    "INSERT DATA { <http://example.com/s> <http://example.com/p> <http://example.com/o> }",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn oidc_introspection_rejects_inactive_token() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockIntrospectionServer::spawn(IntrospectionReply {
        active: false,
        scope: Some("nrese.admin".to_owned()),
        scp: None,
        role: None,
        roles: None,
    })
    .await?;
    let app = test_app_with_policy(oidc_policy(server.url())?)?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ops/api/diagnostics/runtime")
                .method(Method::GET)
                .header("authorization", "Bearer oidc-admin-token")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[derive(Serialize)]
struct TestClaims {
    exp: usize,
    iss: &'static str,
    aud: &'static str,
    roles: Vec<String>,
}

impl TestClaims {
    fn new(roles: Vec<String>) -> Self {
        Self {
            exp: 4_102_444_800,
            iss: "nrese-tests",
            aud: "nrese-api",
            roles,
        }
    }
}

fn test_jwt(roles: &[&str]) -> String {
    encode(
        &Header::default(),
        &TestClaims::new(roles.iter().map(|role| (*role).to_owned()).collect()),
        &EncodingKey::from_secret(b"test-secret"),
    )
    .expect("jwt")
}

fn static_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: Some("reader".to_owned()),
            admin_token: "admin".to_owned(),
        }),
        ..PolicyConfig::default()
    }
}

fn jwt_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: "test-secret".to_owned(),
            issuer: Some("nrese-tests".to_owned()),
            audience: Some("nrese-api".to_owned()),
            read_role: "nrese.read".to_owned(),
            admin_role: "nrese.admin".to_owned(),
            leeway_seconds: 0,
        }),
        ..PolicyConfig::default()
    }
}

fn mtls_policy() -> PolicyConfig {
    PolicyConfig {
        auth: AuthConfig::Mtls(MtlsConfig {
            subject_header: "x-client-cert-subject".to_owned(),
            read_subjects: BTreeSet::from(["CN=reader-1,O=Test".to_owned()]),
            admin_subjects: BTreeSet::from(["CN=admin-1,O=Test".to_owned()]),
        }),
        ..PolicyConfig::default()
    }
}

fn oidc_policy(url: String) -> Result<PolicyConfig, Box<dyn std::error::Error>> {
    Ok(PolicyConfig {
        auth: AuthConfig::OidcIntrospection(OidcIntrospectionConfig::new(
            url.parse()?,
            None,
            None,
            "nrese.read".to_owned(),
            "nrese.admin".to_owned(),
            5_000,
        )?),
        ..PolicyConfig::default()
    })
}

#[derive(Debug, Clone, Serialize)]
struct IntrospectionReply {
    active: bool,
    scope: Option<String>,
    scp: Option<Vec<String>>,
    role: Option<String>,
    roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct IntrospectionForm {
    token: String,
    token_type_hint: Option<String>,
}

struct MockIntrospectionServer {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl MockIntrospectionServer {
    async fn spawn(reply: IntrospectionReply) -> Result<Self, Box<dyn std::error::Error>> {
        let app = Router::new().route(
            "/introspect",
            post(move |Form(form): Form<IntrospectionForm>| {
                let reply = reply.clone();
                async move {
                    assert!(!form.token.is_empty());
                    assert_eq!(form.token_type_hint.as_deref(), Some("access_token"));
                    Json(reply)
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock oidc server");
        });

        Ok(Self { address, task })
    }

    fn url(&self) -> String {
        format!("http://{}/introspect", self.address)
    }
}

impl Drop for MockIntrospectionServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
