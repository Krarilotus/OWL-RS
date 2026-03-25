use axum::http::HeaderValue;

pub fn header_value_str(value: Option<&HeaderValue>) -> Option<&str> {
    value.and_then(|value| value.to_str().ok())
}

pub fn media_type_matches(header_value: Option<&str>, expected: &str) -> bool {
    header_value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .any(|candidate| media_type_token(candidate).eq_ignore_ascii_case(expected))
}

fn media_type_token(value: &str) -> &str {
    value.split(';').next().map(str::trim).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::{header_value_str, media_type_matches};

    #[test]
    fn media_type_match_ignores_parameters() {
        assert!(media_type_matches(
            Some("application/problem+json; charset=utf-8"),
            "application/problem+json"
        ));
    }

    #[test]
    fn media_type_match_handles_accept_lists() {
        assert!(media_type_matches(
            Some("text/plain, application/sparql-results+xml; q=0.9"),
            "application/sparql-results+xml"
        ));
    }

    #[test]
    fn header_value_str_returns_valid_header_text() {
        let value = HeaderValue::from_static("text/turtle");
        assert_eq!(header_value_str(Some(&value)), Some("text/turtle"));
    }
}
