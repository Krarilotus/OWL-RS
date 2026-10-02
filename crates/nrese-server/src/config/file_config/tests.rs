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
mode = "owl2-rl"

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

/// A misspelt or unsupported key is a startup error naming the key, not a silently ignored
/// setting; aliases still work.
#[test]
fn unknown_keys_are_rejected_and_aliases_accepted() {
    let temp_dir = tempdir().expect("temp dir");
    let path = temp_dir.path().join("config.toml");
    for (text, key) in [
        ("unknown_table = 1\n", "unknown_table"),
        ("[store]\nmdoe = \"on-disk\"\n", "mdoe"),
        ("[reasoner]\nconsistency = \"report\"\n", "consistency"),
        ("[policy.limits]\nmax_query_byte = 1\n", "max_query_byte"),
    ] {
        fs::write(&path, text).expect("write config");
        let error = format!("{:#}", load_file_source(&path).expect_err(text));
        assert!(error.contains(key), "{key}: {error}");
    }
    // A budget under both its names is ambiguous.
    fs::write(
        &path,
        "[budgets]\nquery_timeout = \"10s\"\n[policy.timeouts]\nquery_ms = 5000\n",
    )
    .expect("write config");
    let error = format!("{:#}", load_file_source(&path).expect_err("set twice"));
    assert!(
        error.contains("set twice") && error.contains("policy.timeouts.query_ms"),
        "{error}"
    );
    fs::write(&path, "[server]\nbind_addr = \"127.0.0.1:9000\"\n").expect("write config");
    let source = load_file_source(&path).expect("alias accepted");
    assert_eq!(
        source.get(names::BIND_ADDR).as_deref(),
        Some("127.0.0.1:9000")
    );
}

/// `[budgets]` holds every resource budget, with units; the values reach the same runtime
/// keys as the older, separate settings.
#[test]
fn budgets_are_one_table_with_units() {
    let temp_dir = tempdir().expect("temp dir");
    let path = temp_dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[budgets]
query_memory = "8GiB"
total_query_memory = "24 GiB"
query_timeout = "2min"
update_timeout = 90000
upload_size = "1GiB"
result_cache = "256MiB"
"#,
    )
    .expect("config file");
    let source = load_file_source(&path).expect("file source");
    for (key, value) in [
        (names::MAX_QUERY_MEMORY_BYTES, "8GiB"),
        (names::MAX_TOTAL_QUERY_MEMORY_BYTES, "24 GiB"),
        (names::QUERY_TIMEOUT_MS, "2min"),
        (names::UPDATE_TIMEOUT_MS, "90000"),
        (names::MAX_RDF_UPLOAD_BYTES, "1GiB"),
        (names::QUERY_CACHE_BYTES, "256MiB"),
    ] {
        assert_eq!(source.get(key).as_deref(), Some(value), "{key}");
    }

    let config = crate::config::ServerConfig::load_isolated(&path, &[]).expect("config");
    assert_eq!(config.policy.limits.max_query_memory_bytes, 8 << 30);
    assert_eq!(config.store.total_query_memory_bytes, 24 << 30);
    assert_eq!(config.policy.timeouts.query.as_secs(), 120);
    assert_eq!(config.policy.timeouts.update.as_secs(), 90);
    assert_eq!(config.policy.limits.max_rdf_upload_bytes, 1 << 30);
    assert_eq!(config.store.query_cache_bytes, 256 << 20);
    let summary = config.summary();
    for line in [
        "budgets.query_memory = 8 GiB",
        "budgets.total_query_memory = 24 GiB",
        "budgets.query_timeout = 120s",
        "budgets.upload_size = 1 GiB",
    ] {
        assert!(summary.contains(line), "{line}: {summary}");
    }

    fs::write(&path, "[budgets]\nquery_memory = \"4 parsecs\"\n").expect("write config");
    let error = format!(
        "{:#}",
        crate::config::ServerConfig::load_isolated(&path, &[]).expect_err("unknown unit")
    );
    assert!(
        error.contains("NRESE_MAX_QUERY_MEMORY_BYTES") && error.contains("parsecs"),
        "{error}"
    );
}

/// A value of the wrong kind names its key; the command line's `--set` goes over the file
/// and the environment, and its keys are checked like the file's.
#[test]
fn values_are_checked_by_kind_and_overrides_win() {
    let temp_dir = tempdir().expect("temp dir");
    let path = temp_dir.path().join("config.toml");
    for text in [
        "[store]\nverify_on_open = \"yes\"\n",
        "[federation]\nmax_rows = -1\n",
        "[auth.mtls]\ntrusted_proxies = [1, 2]\n",
        "[server]\nbind_address = 8080\n",
    ] {
        fs::write(&path, text).expect("write config");
        let error = format!("{:#}", load_file_source(&path).expect_err(text));
        let key = text
            .lines()
            .nth(1)
            .and_then(|line| line.split(' ').next())
            .expect("a key");
        assert!(error.contains(key), "{key}: {error}");
    }

    fs::write(&path, "[budgets]\nquery_timeout = \"10s\"\n").expect("write config");
    let overrides = [("budgets.query_timeout".to_owned(), "45s".to_owned())];
    let config = crate::config::ServerConfig::load_isolated(&path, &overrides).expect("config");
    assert_eq!(config.policy.timeouts.query.as_secs(), 45);
    // An older key works on the command line too.
    let overrides = [("policy.timeouts.query_ms".to_owned(), "7000".to_owned())];
    let config = crate::config::ServerConfig::load_isolated(&path, &overrides).expect("config");
    assert_eq!(config.policy.timeouts.query.as_secs(), 7);
    let overrides = [("store.mdoe".to_owned(), "on-disk".to_owned())];
    let error = format!(
        "{:#}",
        crate::config::ServerConfig::load_isolated(&path, &overrides).expect_err("unknown")
    );
    assert!(error.contains("store.mdoe"), "{error}");
}
