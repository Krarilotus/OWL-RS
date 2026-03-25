use super::raw::{RawFileConfig, StringOrMany};
use crate::config::env_names as names;
use crate::config::source::KeyValueSource;

pub(super) fn into_key_value_source(config: RawFileConfig) -> KeyValueSource {
    let mut source = KeyValueSource::default();

    insert_option(&mut source, names::BIND_ADDR, config.server.bind_address);
    insert_option(&mut source, names::DATA_DIR, config.store.data_dir);
    insert_option(&mut source, names::STORE_MODE, config.store.mode);
    insert_option(
        &mut source,
        names::ONTOLOGY_PATH,
        config.store.ontology_path,
    );

    insert_option(&mut source, names::REASONING_MODE, config.reasoner.mode);
    insert_option(
        &mut source,
        names::REASONER_RULES_MVP_PRESET,
        config.reasoner.rules_mvp.preset,
    );
    insert_joined(
        &mut source,
        names::REASONER_RULES_MVP_FEATURES,
        config.reasoner.rules_mvp.features,
        ",",
    );

    insert_numeric(
        &mut source,
        names::MAX_QUERY_BYTES,
        config.policy.limits.max_query_bytes,
    );
    insert_numeric(
        &mut source,
        names::MAX_UPDATE_BYTES,
        config.policy.limits.max_update_bytes,
    );
    insert_numeric(
        &mut source,
        names::MAX_RDF_UPLOAD_BYTES,
        config.policy.limits.max_rdf_upload_bytes,
    );
    insert_numeric(
        &mut source,
        names::RATE_LIMIT_WINDOW_SECS,
        config.policy.rate_limits.window_secs,
    );
    insert_numeric(
        &mut source,
        names::READ_REQUESTS_PER_WINDOW,
        config.policy.rate_limits.read_requests_per_window,
    );
    insert_numeric(
        &mut source,
        names::WRITE_REQUESTS_PER_WINDOW,
        config.policy.rate_limits.write_requests_per_window,
    );
    insert_numeric(
        &mut source,
        names::ADMIN_REQUESTS_PER_WINDOW,
        config.policy.rate_limits.admin_requests_per_window,
    );
    insert_numeric(
        &mut source,
        names::QUERY_TIMEOUT_MS,
        config.policy.timeouts.query_ms,
    );
    insert_numeric(
        &mut source,
        names::UPDATE_TIMEOUT_MS,
        config.policy.timeouts.update_ms,
    );
    insert_numeric(
        &mut source,
        names::GRAPH_READ_TIMEOUT_MS,
        config.policy.timeouts.graph_read_ms,
    );
    insert_numeric(
        &mut source,
        names::GRAPH_WRITE_TIMEOUT_MS,
        config.policy.timeouts.graph_write_ms,
    );
    insert_bool(
        &mut source,
        names::ENABLE_OPERATOR_UI,
        config.policy.exposure.operator_ui,
    );
    insert_bool(
        &mut source,
        names::ENABLE_METRICS,
        config.policy.exposure.metrics,
    );

    insert_bool(&mut source, names::AI_ENABLED, config.ai.enabled);
    insert_option(&mut source, names::AI_PROVIDER, config.ai.provider);
    insert_option(&mut source, names::AI_MODEL, config.ai.model);
    insert_numeric(&mut source, names::AI_TIMEOUT_MS, config.ai.timeout_ms);
    insert_numeric(
        &mut source,
        names::AI_MAX_SUGGESTIONS,
        config.ai.max_suggestions,
    );
    insert_option(
        &mut source,
        names::AI_SYSTEM_PROMPT,
        config.ai.system_prompt,
    );
    insert_option(
        &mut source,
        names::AI_GOOGLE_API_KEY,
        config.ai.gemini.api_key,
    );
    insert_option(
        &mut source,
        names::AI_GOOGLE_API_BASE,
        config.ai.gemini.api_base,
    );
    insert_option(
        &mut source,
        names::AI_OPENROUTER_API_KEY,
        config.ai.openrouter.api_key,
    );
    insert_option(
        &mut source,
        names::AI_OPENROUTER_API_BASE,
        config.ai.openrouter.api_base,
    );
    insert_option(
        &mut source,
        names::AI_OPENROUTER_SITE_URL,
        config.ai.openrouter.site_url,
    );
    insert_option(
        &mut source,
        names::AI_OPENROUTER_APP_NAME,
        config.ai.openrouter.app_name,
    );

    insert_option(&mut source, names::AUTH_MODE, config.auth.mode);
    insert_option(
        &mut source,
        names::AUTH_READ_TOKEN,
        config.auth.bearer_static.read_token,
    );
    insert_option(
        &mut source,
        names::AUTH_ADMIN_TOKEN,
        config.auth.bearer_static.admin_token,
    );
    insert_option(
        &mut source,
        names::AUTH_JWT_SECRET,
        config.auth.bearer_jwt.shared_secret,
    );
    insert_option(
        &mut source,
        names::AUTH_JWT_ISSUER,
        config.auth.bearer_jwt.issuer,
    );
    insert_option(
        &mut source,
        names::AUTH_JWT_AUDIENCE,
        config.auth.bearer_jwt.audience,
    );
    insert_option(
        &mut source,
        names::AUTH_JWT_READ_ROLE,
        config.auth.bearer_jwt.read_role,
    );
    insert_option(
        &mut source,
        names::AUTH_JWT_ADMIN_ROLE,
        config.auth.bearer_jwt.admin_role,
    );
    insert_numeric(
        &mut source,
        names::AUTH_JWT_LEEWAY_SECS,
        config.auth.bearer_jwt.leeway_seconds,
    );
    insert_option(
        &mut source,
        names::AUTH_MTLS_SUBJECT_HEADER,
        config.auth.mtls.subject_header,
    );
    insert_joined(
        &mut source,
        names::AUTH_MTLS_READ_SUBJECTS,
        config.auth.mtls.read_subjects,
        ";",
    );
    insert_joined(
        &mut source,
        names::AUTH_MTLS_ADMIN_SUBJECTS,
        config.auth.mtls.admin_subjects,
        ";",
    );
    insert_option(
        &mut source,
        names::AUTH_OIDC_INTROSPECTION_URL,
        config.auth.oidc_introspection.introspection_url,
    );
    insert_option(
        &mut source,
        names::AUTH_OIDC_CLIENT_ID,
        config.auth.oidc_introspection.client_id,
    );
    insert_option(
        &mut source,
        names::AUTH_OIDC_CLIENT_SECRET,
        config.auth.oidc_introspection.client_secret,
    );
    insert_option(
        &mut source,
        names::AUTH_OIDC_READ_ROLE,
        config.auth.oidc_introspection.read_role,
    );
    insert_option(
        &mut source,
        names::AUTH_OIDC_ADMIN_ROLE,
        config.auth.oidc_introspection.admin_role,
    );
    insert_numeric(
        &mut source,
        names::AUTH_OIDC_TIMEOUT_MS,
        config.auth.oidc_introspection.timeout_ms,
    );

    source
}

fn insert_option(source: &mut KeyValueSource, key: &str, value: Option<String>) {
    if let Some(value) = value {
        source.insert(key, value);
    }
}

fn insert_numeric<T>(source: &mut KeyValueSource, key: &str, value: Option<T>)
where
    T: ToString,
{
    if let Some(value) = value {
        source.insert(key, value.to_string());
    }
}

fn insert_bool(source: &mut KeyValueSource, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        source.insert(key, if value { "true" } else { "false" });
    }
}

fn insert_joined(
    source: &mut KeyValueSource,
    key: &str,
    value: Option<StringOrMany>,
    delimiter: &str,
) {
    if let Some(value) = value {
        source.insert(key, value.join(delimiter));
    }
}
