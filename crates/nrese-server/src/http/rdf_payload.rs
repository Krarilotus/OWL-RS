use axum::extract::RawQuery;

use nrese_store::{GraphResultFormat, GraphTarget};

use crate::error::ApiError;
use crate::state::AppState;

pub fn ensure_ready(state: &AppState) -> Result<(), ApiError> {
    if state.is_ready() {
        Ok(())
    } else {
        Err(ApiError::unavailable("server is not ready yet"))
    }
}

pub fn parse_graph_target(raw_query: &RawQuery) -> Result<GraphTarget, ApiError> {
    let Some(raw) = raw_query.0.as_deref() else {
        return Ok(GraphTarget::DefaultGraph);
    };
    if raw.trim().is_empty() {
        return Ok(GraphTarget::DefaultGraph);
    }

    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;

    let mut graph: Option<String> = None;
    let mut has_default = false;
    for (key, value) in pairs {
        if key == "default" {
            has_default = true;
            continue;
        }
        if key == "graph" {
            if graph.is_some() {
                return Err(ApiError::bad_request(
                    "multiple graph query parameters are not supported",
                ));
            }
            graph = Some(value);
        }
    }

    if has_default && graph.is_some() {
        return Err(ApiError::bad_request(
            "graph and default query parameters are mutually exclusive",
        ));
    }

    if let Some(graph_iri) = graph {
        if graph_iri.trim().is_empty() {
            return Err(ApiError::bad_request(
                "graph query parameter must not be empty",
            ));
        }
        return Ok(GraphTarget::NamedGraph(graph_iri));
    }

    Ok(GraphTarget::DefaultGraph)
}

pub fn parse_graph_content_format(
    content_type: Option<&str>,
) -> Result<GraphResultFormat, ApiError> {
    parse_labeled_rdf_content_format(content_type, "graph")
}

pub fn parse_tell_content_format(
    content_type: Option<&str>,
) -> Result<GraphResultFormat, ApiError> {
    parse_labeled_rdf_content_format(content_type, "tell")
}

fn parse_labeled_rdf_content_format(
    content_type: Option<&str>,
    surface: &str,
) -> Result<GraphResultFormat, ApiError> {
    if crate::http::media::media_type_matches(content_type, "application/n-triples") {
        return Ok(GraphResultFormat::NTriples);
    }
    if crate::http::media::media_type_matches(content_type, "application/rdf+xml") {
        return Ok(GraphResultFormat::RdfXml);
    }
    if crate::http::media::media_type_matches(content_type, "text/turtle")
        || crate::http::media::media_type_matches(content_type, "application/x-turtle")
    {
        return Ok(GraphResultFormat::Turtle);
    }

    Err(ApiError::bad_request(format!(
        "unsupported {surface} content type: {}",
        if content_type.unwrap_or_default().is_empty() {
            "<missing>"
        } else {
            content_type.unwrap_or_default()
        }
    )))
}

#[cfg(test)]
mod tests {
    use axum::extract::RawQuery;
    use nrese_store::{GraphResultFormat, GraphTarget};

    use super::{parse_graph_content_format, parse_graph_target, parse_tell_content_format};

    #[test]
    fn graph_target_defaults_to_default_graph() {
        let target = parse_graph_target(&RawQuery(None)).expect("target should parse");
        assert_eq!(target, GraphTarget::DefaultGraph);
    }

    #[test]
    fn graph_target_accepts_named_graph_parameter() {
        let target = parse_graph_target(&RawQuery(Some(
            "graph=http%3A%2F%2Fexample.com%2Fg".to_owned(),
        )))
        .expect("target should parse");
        assert_eq!(
            target,
            GraphTarget::NamedGraph("http://example.com/g".to_owned())
        );
    }

    #[test]
    fn graph_target_rejects_conflicting_default_and_graph() {
        let result = parse_graph_target(&RawQuery(Some(
            "default=&graph=http%3A%2F%2Fexample.com%2Fg".to_owned(),
        )));
        assert!(result.is_err());
    }

    #[test]
    fn rdf_content_type_rejects_unknown_media_type() {
        let result = parse_graph_content_format(Some("application/json"));
        assert!(result.is_err());
    }

    #[test]
    fn rdf_content_type_accepts_turtle() {
        let format = parse_graph_content_format(Some("text/turtle; charset=utf-8"))
            .expect("format should parse");
        assert_eq!(format, GraphResultFormat::Turtle);
    }

    #[test]
    fn rdf_content_type_accepts_rdf_xml() {
        let format = parse_graph_content_format(Some("application/rdf+xml; charset=utf-8"))
            .expect("format should parse");
        assert_eq!(format, GraphResultFormat::RdfXml);
    }

    #[test]
    fn tell_content_type_uses_tell_surface_label() {
        let error = parse_tell_content_format(Some("application/json"))
            .expect_err("tell content type should fail");
        assert!(error.to_string().contains("unsupported tell content type"));
    }
}
