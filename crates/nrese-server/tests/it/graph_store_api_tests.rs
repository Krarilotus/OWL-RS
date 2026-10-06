use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app};

#[tokio::test]
async fn graph_put_rejects_unsupported_content_type_with_problem_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Funsupported")
                .method(Method::PUT)
                .header("content-type", "application/json")
                .body(Body::from("{\"not\":\"rdf\"}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let text = body_text(response).await?;
    assert!(text.contains("unsupported graph content type"));

    Ok(())
}

#[tokio::test]
async fn graph_put_rejects_malformed_turtle_with_problem_json()
-> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/data?graph=http%3A%2F%2Fexample.com%2Fmalformed")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .body(Body::from(
                    "@prefix ex: <http://example.com/> . ex:broken ex:p ",
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let text = body_text(response).await?;
    assert!(text.contains("Bad Request"));

    Ok(())
}

#[tokio::test]
async fn graph_put_honors_content_location_as_base_iri() -> Result<(), Box<dyn std::error::Error>> {
    let app = test_app()?;
    let put_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dataset/data")
                .method(Method::PUT)
                .header("content-type", "text/turtle")
                .header("content-location", "https://www.w3.org/ns/prov.ttl")
                .body(Body::from(
                    "@prefix : <#> . :generated <http://www.w3.org/2002/07/owl#inverseOf> :wasGeneratedBy .",
                ))?,
        )
        .await?;
    assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

    let get_response = app
        .oneshot(
            Request::builder()
                .uri("/dataset/query?query=PREFIX%20owl%3A%20%3Chttp%3A%2F%2Fwww.w3.org%2F2002%2F07%2Fowl%23%3E%20ASK%20WHERE%20%7B%20%3Chttps%3A%2F%2Fwww.w3.org%2Fns%2Fprov.ttl%23generated%3E%20owl%3AinverseOf%20%3Chttps%3A%2F%2Fwww.w3.org%2Fns%2Fprov.ttl%23wasGeneratedBy%3E%20%7D")
                .method(Method::GET)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    assert!(body_text(get_response).await?.contains("true"));

    Ok(())
}
