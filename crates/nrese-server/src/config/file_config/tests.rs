use std::fs;

use tempfile::tempdir;

use super::load_file_source;
use crate::config::env_names as names;
use crate::config::source::ConfigSource;

#[test]
fn file_config_maps_sections_to_runtime_keys() {
    let temp_dir = tempdir().expect("temp dir");
    let path = temp_dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[server]
bind_address = "0.0.0.0:9191"

[store]
mode = "on-disk"
data_dir = "/var/lib/nrese/data"

[reasoner]
mode = "rules-mvp"

[reasoner.rules_mvp]
preset = "bounded-owl"
features = ["rdfs-subclass-closure", "owl-consistency-check"]

[ai]
enabled = true
provider = "openrouter"
model = "openai/gpt-4o-mini"

[ai.openrouter]
api_key = "test-openrouter-key"
site_url = "https://example.com"

[policy.limits]
max_query_bytes = 2048

[policy]
sparql_parse_error_profile = "plain-text"

[auth]
mode = "mtls"

[auth.mtls]
subject_header = "x-ssl-client-s-dn"
admin_subjects = ["CN=admin,O=Test"]
"#,
    )
    .expect("config file");

    let source = load_file_source(&path).expect("file source");

    assert_eq!(
        source.get(names::BIND_ADDR).as_deref(),
        Some("0.0.0.0:9191")
    );
    assert_eq!(source.get(names::STORE_MODE).as_deref(), Some("on-disk"));
    assert_eq!(
        source.get(names::REASONER_RULES_MVP_FEATURES).as_deref(),
        Some("rdfs-subclass-closure,owl-consistency-check")
    );
    assert_eq!(
        source.get(names::REASONER_RULES_MVP_PRESET).as_deref(),
        Some("bounded-owl")
    );
    assert_eq!(
        source.get(names::AI_PROVIDER).as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        source.get(names::AI_OPENROUTER_SITE_URL).as_deref(),
        Some("https://example.com")
    );
    assert_eq!(
        source.get(names::AUTH_MTLS_ADMIN_SUBJECTS).as_deref(),
        Some("CN=admin,O=Test")
    );
    assert_eq!(
        source.get(names::SPARQL_PARSE_ERROR_PROFILE).as_deref(),
        Some("plain-text")
    );
}
