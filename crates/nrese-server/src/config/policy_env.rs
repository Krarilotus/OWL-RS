use std::time::Duration;

use anyhow::Result;

use crate::policy::{
    PolicyConfig, RateLimitConfig, RequestLimits, RequestTimeouts, SparqlParseErrorProfile,
};

use super::auth_env::parse_auth_config;
use super::env_names as names;
use super::env_values::{parse_bool, parse_u64, parse_usize};
use super::source::ConfigSource;

pub(super) fn parse_policy_config(source: &dyn ConfigSource) -> Result<PolicyConfig> {
    Ok(PolicyConfig {
        auth: parse_auth_config(source)?,
        limits: RequestLimits {
            max_query_bytes: parse_usize(source, names::MAX_QUERY_BYTES, 1_048_576)?,
            max_update_bytes: parse_usize(source, names::MAX_UPDATE_BYTES, 1_048_576)?,
            max_rdf_upload_bytes: parse_usize(source, names::MAX_RDF_UPLOAD_BYTES, 10_485_760)?,
        },
        rate_limits: RateLimitConfig {
            window: Duration::from_secs(parse_u64(source, names::RATE_LIMIT_WINDOW_SECS, 60)?),
            read_requests_per_window: parse_usize(source, names::READ_REQUESTS_PER_WINDOW, 0)?,
            write_requests_per_window: parse_usize(source, names::WRITE_REQUESTS_PER_WINDOW, 0)?,
            admin_requests_per_window: parse_usize(source, names::ADMIN_REQUESTS_PER_WINDOW, 0)?,
        },
        timeouts: RequestTimeouts {
            query: Duration::from_millis(parse_u64(source, names::QUERY_TIMEOUT_MS, 30_000)?),
            update: Duration::from_millis(parse_u64(source, names::UPDATE_TIMEOUT_MS, 60_000)?),
            graph_read: Duration::from_millis(parse_u64(
                source,
                names::GRAPH_READ_TIMEOUT_MS,
                30_000,
            )?),
            graph_write: Duration::from_millis(parse_u64(
                source,
                names::GRAPH_WRITE_TIMEOUT_MS,
                60_000,
            )?),
        },
        sparql_parse_error_profile: parse_sparql_parse_error_profile(
            source.get(names::SPARQL_PARSE_ERROR_PROFILE).as_deref(),
        )?,
        expose_operator_ui: parse_bool(source, names::ENABLE_OPERATOR_UI, true)?,
        expose_metrics: parse_bool(source, names::ENABLE_METRICS, true)?,
    })
}

fn parse_sparql_parse_error_profile(value: Option<&str>) -> Result<SparqlParseErrorProfile> {
    match value.unwrap_or("problem-json") {
        "problem-json" => Ok(SparqlParseErrorProfile::ProblemJson),
        "plain-text" | "fuseki-plain-text" => Ok(SparqlParseErrorProfile::PlainText),
        other => anyhow::bail!("invalid SPARQL parse error profile: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::source::KeyValueSource;
    use crate::policy::SparqlParseErrorProfile;

    use super::parse_policy_config;

    #[test]
    fn parses_plain_text_sparql_parse_error_profile() {
        let mut source = KeyValueSource::default();
        source.insert("NRESE_SPARQL_PARSE_ERROR_PROFILE", "plain-text");

        let policy = parse_policy_config(&source).expect("policy");

        assert_eq!(
            policy.sparql_parse_error_profile,
            SparqlParseErrorProfile::PlainText
        );
    }
}
