//! Media types: what the server produces and accepts, and content negotiation.
//!
//! Clients differ a lot in what they send as `Accept`: browsers and RDF4J send long
//! weighted lists, scripts send `application/json`, some send nothing. One negotiator
//! ([`negotiate`], RFC 9110 §12.5.1) serves every endpoint, over the tables below.

use axum::http::HeaderValue;
use nrese_store::{GraphResultFormat, SolutionsResultFormat};

use crate::error::ApiError;

/// A media type and what it stands for. Earlier entries win ties, so the first one is
/// what a client without preferences gets.
pub type Offers<T> = &'static [(&'static str, T)];

/// `SELECT` results.
pub const SOLUTIONS: Offers<SolutionsResultFormat> = &[
    (
        "application/sparql-results+json",
        SolutionsResultFormat::Json,
    ),
    ("application/sparql-results+xml", SolutionsResultFormat::Xml),
    ("text/csv", SolutionsResultFormat::Csv),
    ("text/tab-separated-values", SolutionsResultFormat::Tsv),
    ("application/json", SolutionsResultFormat::Json),
    ("application/xml", SolutionsResultFormat::Xml),
];

/// `ASK` results: CSV and TSV don't define a boolean result.
pub const BOOLEAN: Offers<SolutionsResultFormat> = &[
    (
        "application/sparql-results+json",
        SolutionsResultFormat::Json,
    ),
    ("application/sparql-results+xml", SolutionsResultFormat::Xml),
    ("application/json", SolutionsResultFormat::Json),
    ("application/xml", SolutionsResultFormat::Xml),
];

/// One graph: `CONSTRUCT` and `DESCRIBE` results, Graph Store reads, and RDF payloads.
pub const GRAPHS: Offers<GraphResultFormat> = &[
    ("application/n-triples", GraphResultFormat::NTriples),
    ("text/turtle", GraphResultFormat::Turtle),
    ("application/rdf+xml", GraphResultFormat::RdfXml),
    ("application/ld+json", GraphResultFormat::JsonLd),
    ("application/n-quads", GraphResultFormat::NQuads),
    ("application/trig", GraphResultFormat::TriG),
    ("application/x-binary-rdf", GraphResultFormat::BinaryRdf),
    ("application/x-turtle", GraphResultFormat::Turtle),
    ("text/plain", GraphResultFormat::NTriples),
];

pub fn header_value_str(value: Option<&HeaderValue>) -> Option<&str> {
    value.and_then(|value| value.to_str().ok())
}

/// Whether a `Content-Type` (or a list of media types) names `expected`; parameters such
/// as `charset` are ignored.
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

/// What a `Content-Type` stands for in `offers`, if it is one of them.
pub fn content_format<T: Copy>(content_type: Option<&str>, offers: Offers<T>) -> Option<T> {
    let sent = media_type_token(content_type?);
    offers
        .iter()
        .find(|(media_type, _)| media_type.eq_ignore_ascii_case(sent))
        .map(|&(_, format)| format)
}

/// One media range of an `Accept` header: `type/subtype;q=…`, in thousandths.
struct Range<'a> {
    kind: &'a str,
    subtype: &'a str,
    quality: u16,
}

impl<'a> Range<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let mut parts = text.split(';').map(str::trim);
        let (kind, subtype) = parts.next()?.split_once('/')?;
        let quality = parts
            .filter_map(|parameter| parameter.split_once('='))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
            .map_or(Some(1000), |(_, value)| {
                // Three decimals at most (RFC 9110); anything else is a malformed range.
                let value: f32 = value.trim().parse().ok()?;
                (0.0..=1.0)
                    .contains(&value)
                    .then(|| (value * 1000.0).round() as u16)
            })?;
        Some(Self {
            kind: kind.trim(),
            subtype: subtype.trim(),
            quality,
        })
    }

    /// How specifically the range names `media_type`: 3 exactly, 2 by type, 1 by `*/*`.
    fn specificity(&self, media_type: &str) -> u8 {
        let Some((kind, subtype)) = media_type.split_once('/') else {
            return 0;
        };
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        match (self.kind, self.subtype) {
            ("*", "*") => 1,
            (range_kind, "*") if same(range_kind, kind) => 2,
            (range_kind, range_subtype)
                if same(range_kind, kind) && same(range_subtype, subtype) =>
            {
                3
            }
            _ => 0,
        }
    }
}

/// What to send: the offer the client weights highest, where the most specific range
/// that names an offer decides its weight. Ties go to the earlier offer. Without an
/// `Accept` header the first offer is sent. `None`: the client accepts none of them.
pub fn negotiate<T: Copy>(accept: Option<&str>, offers: Offers<T>) -> Option<T> {
    let ranges: Vec<Range<'_>> = accept
        .unwrap_or_default()
        .split(',')
        .filter_map(Range::parse)
        .collect();
    if ranges.is_empty() {
        return offers.first().map(|&(_, format)| format);
    }
    let mut best: Option<(u16, T)> = None;
    for &(media_type, format) in offers {
        let quality = ranges
            .iter()
            .map(|range| (range.specificity(media_type), range.quality))
            .filter(|&(specificity, _)| specificity > 0)
            .max_by_key(|&(specificity, _)| specificity)
            .map_or(0, |(_, quality)| quality);
        if quality > 0 && best.is_none_or(|(best, _)| quality > best) {
            best = Some((quality, format));
        }
    }
    best.map(|(_, format)| format)
}

/// [`negotiate`], or 406 naming what the endpoint can send.
pub fn negotiated<T: Copy>(accept: Option<&str>, offers: Offers<T>) -> Result<T, ApiError> {
    negotiate(accept, offers).ok_or_else(|| {
        let mut names: Vec<&str> = offers.iter().map(|(media_type, _)| *media_type).collect();
        names.dedup();
        ApiError::not_acceptable(format!(
            "none of the accepted media types can be produced; available: {}",
            names.join(", ")
        ))
    })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use nrese_store::{GraphResultFormat, SolutionsResultFormat};

    use super::{
        BOOLEAN, GRAPHS, SOLUTIONS, content_format, header_value_str, media_type_matches, negotiate,
    };

    #[test]
    fn media_type_match_ignores_parameters() {
        assert!(media_type_matches(
            Some("application/problem+json; charset=utf-8"),
            "application/problem+json"
        ));
    }

    #[test]
    fn header_value_str_returns_valid_header_text() {
        let value = HeaderValue::from_static("text/turtle");
        assert_eq!(header_value_str(Some(&value)), Some("text/turtle"));
    }

    #[test]
    fn content_types_are_looked_up_without_their_parameters() {
        assert_eq!(
            content_format(Some("Text/Turtle; charset=utf-8"), GRAPHS),
            Some(GraphResultFormat::Turtle)
        );
        assert_eq!(content_format(Some("text/html"), GRAPHS), None);
        assert_eq!(content_format(None, GRAPHS), None);
    }

    #[test]
    fn negotiation_follows_weights_specificity_and_order() {
        use GraphResultFormat::{JsonLd, NTriples, RdfXml, Turtle};
        use SolutionsResultFormat::{Csv, Json, Tsv, Xml};
        let solutions = |accept| negotiate(accept, SOLUTIONS);
        let graphs = |accept| negotiate(accept, GRAPHS);

        // No preference: the first offer.
        assert_eq!(solutions(None), Some(Json));
        assert_eq!(solutions(Some("")), Some(Json));
        assert_eq!(solutions(Some("*/*")), Some(Json));
        assert_eq!(graphs(Some("*/*")), Some(NTriples));
        // The highest weight wins, wherever it stands in the list.
        assert_eq!(
            solutions(Some("application/sparql-results+xml;q=0.5, text/csv")),
            Some(Csv)
        );
        assert_eq!(
            graphs(Some(
                "application/rdf+xml;q=0.2, text/turtle;q=0.9, */*;q=0.1"
            )),
            Some(Turtle)
        );
        // Equal weights: the server's order.
        assert_eq!(
            solutions(Some("text/csv, text/tab-separated-values")),
            Some(Csv)
        );
        assert_eq!(solutions(Some("text/tab-separated-values")), Some(Tsv));
        // A more specific range overrides a wildcard, also to exclude.
        assert_eq!(
            solutions(Some("*/*, application/sparql-results+json;q=0")),
            Some(Xml)
        );
        assert_eq!(graphs(Some("text/*")), Some(Turtle));
        // Common aliases.
        assert_eq!(solutions(Some("application/json")), Some(Json));
        assert_eq!(graphs(Some("application/ld+json")), Some(JsonLd));
        // Nothing acceptable, and formats the form doesn't have.
        assert_eq!(solutions(Some("image/png")), None);
        assert_eq!(negotiate(Some("text/csv"), BOOLEAN), None);
        assert_eq!(negotiate(Some("text/csv, */*;q=0.1"), BOOLEAN), Some(Json));
        // Malformed ranges are skipped.
        assert_eq!(
            graphs(Some("nonsense, application/rdf+xml;q=2")),
            Some(NTriples)
        );
        assert_eq!(graphs(Some("nonsense, application/rdf+xml")), Some(RdfXml));
    }

    /// What RDF4J's SPARQL repository (ResearchSpace) and a browser send.
    #[test]
    fn real_clients_get_a_format_they_asked_for() {
        let rdf4j_tuple = "application/x-binary-rdf-results-table, \
             application/sparql-results+xml;q=0.8, application/sparql-results+json;q=0.8, \
             text/csv;q=0.8, text/tab-separated-values;q=0.8, application/x-sparqlstar-results+json;q=0.8";
        assert_eq!(
            negotiate(Some(rdf4j_tuple), SOLUTIONS),
            Some(SolutionsResultFormat::Json)
        );
        let rdf4j_graph = "application/x-binary-rdf, application/n-triples;q=0.8, \
             text/turtle;q=0.8, application/rdf+xml;q=0.5, application/trig;q=0.8, \
             application/n-quads;q=0.8, application/ld+json;q=0.5";
        assert_eq!(
            negotiate(Some(rdf4j_graph), GRAPHS),
            Some(GraphResultFormat::NTriples)
        );
        let rdf4j_boolean = "text/boolean, application/sparql-results+json;q=0.8, \
             application/sparql-results+xml;q=0.8";
        assert_eq!(
            negotiate(Some(rdf4j_boolean), BOOLEAN),
            Some(SolutionsResultFormat::Json)
        );
        let browser = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
        assert_eq!(
            negotiate(Some(browser), SOLUTIONS),
            Some(SolutionsResultFormat::Xml)
        );
    }
}
