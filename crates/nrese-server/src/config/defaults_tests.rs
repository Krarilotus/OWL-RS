//! The defaults the configuration file's schema states (`default`, from the registry in
//! [`super::settings`]) are the configuration's own, and the operator reference's Default
//! column states the same ones.
//!
//! Spec: the default of every setting is in the schema, generated from the registry the
//! reference documents (owner's decision of 14 Nov); the one exception is listed below with
//! its reason, and no other may join it unlisted: a setting the schema gives no default
//! must have none in the configuration ([`absences`]).

use serde_json::Value;

use super::ServerConfig;
use super::env_names as names;
use super::settings::{self, SETTINGS, Unset};
use super::source::KeyValueSource;
use crate::ai::AiProviderConfig;
use crate::auth::AuthConfig;

/// The settings whose default the schema doesn't state, each with why: no value an
/// operator can write gives it on every host.
const UNSTATED_DEFAULTS: &[(&str, &str)] = &[(
    "budgets.result_cache",
    "2% of the process's memory kept between 64 MiB and 8 GiB; a share set explicitly is \
     taken as written, without the bounds",
)];

/// The schema's `default` of the setting `key`.
fn schema_default(schema: &Value, key: &str) -> Option<Value> {
    let mut node = &schema["properties"];
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        node = match parts.peek() {
            Some(_) => &node[part]["properties"],
            None => &node[part],
        };
    }
    node.get("default").cloned()
}

/// What a setting read in one mode only needs besides, so that the mode reads it.
fn mode_of(key: &str) -> &'static [(&'static str, &'static str)] {
    if key == "shacl.gate_severity" {
        return &[(names::SHACL_GATE, "enforce")];
    }
    match key.rsplit_once('.').map_or("", |(table, _)| table) {
        "auth.bearer_jwt" => &[
            (names::AUTH_MODE, "bearer-jwt"),
            (names::AUTH_JWT_SECRET, "s"),
        ],
        "auth.mtls" => &[
            (names::AUTH_MODE, "mtls"),
            (names::AUTH_MTLS_ADMIN_SUBJECTS, "CN=a"),
        ],
        "auth.oidc_introspection" => &[
            (names::AUTH_MODE, "oidc-introspection"),
            (names::AUTH_OIDC_INTROSPECTION_URL, "https://idp/introspect"),
        ],
        "ai.gemini" => &[
            (names::AI_ENABLED, "true"),
            (names::AI_PROVIDER, "gemini"),
            (names::AI_GOOGLE_API_KEY, "k"),
        ],
        "ai.openrouter" => &[
            (names::AI_ENABLED, "true"),
            (names::AI_PROVIDER, "openrouter"),
            (names::AI_OPENROUTER_API_KEY, "k"),
        ],
        _ => &[],
    }
}

/// A configuration file setting `key` to `value` alone.
fn file_with(key: &str, value: &Value) -> toml::Table {
    let mut node = toml::Value::try_from(value).expect("a scalar");
    for part in key.rsplit('.') {
        let mut table = toml::Table::new();
        table.insert(part.to_owned(), node);
        node = toml::Value::Table(table);
    }
    match node {
        toml::Value::Table(table) => table,
        _ => unreachable!("a key has a part"),
    }
}

/// A setting set in the file to its schema default gives the configuration it has unset
/// (a setting read in one mode only, in that mode). A setting the schema gives no default,
/// but the one of [`UNSTATED_DEFAULTS`], has none in the configuration: [`absences`] says
/// what the configuration holds unset, and the check fails for a setting it doesn't name.
/// Every setting is checked; the failures are reported together.
#[test]
fn every_schema_default_is_the_configurations_default() {
    let schema = settings::schema();
    let absences = absences();
    let mut wrong = Vec::new();
    for (key, ..) in &absences {
        if schema_default(&schema, key).is_some() || !SETTINGS.iter().any(|s| s.key == *key) {
            wrong.push(format!(
                "{key}: an absence check, but no setting without a default"
            ));
        }
    }
    for setting in SETTINGS {
        if UNSTATED_DEFAULTS.iter().any(|(key, _)| *key == setting.key) {
            continue;
        }
        let Some(default) = schema_default(&schema, setting.key) else {
            check_absence(setting, &absences, &mut wrong);
            continue;
        };
        let mut unset = KeyValueSource::default();
        for &(env, value) in mode_of(setting.key) {
            unset.insert(env, value);
        }
        let mut set = match settings::from_file(&file_with(setting.key, &default)) {
            Ok(source) => source,
            Err(error) => {
                wrong.push(format!("{} = {default}: {error:#}", setting.key));
                continue;
            }
        };
        for &(env, value) in mode_of(setting.key) {
            set.insert(env, value);
        }
        let unset = ServerConfig::from_source(&unset)
            .unwrap_or_else(|error| panic!("{}: its mode alone: {error:#}", setting.key));
        match ServerConfig::from_source(&set) {
            Ok(config) if config == unset => {}
            Ok(_) => wrong.push(format!(
                "{} = {default}: not the configuration it has unset",
                setting.key
            )),
            Err(error) => wrong.push(format!("{} = {default}: {error:#}", setting.key)),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The settings a mode needs besides, as environment names and values.
type Mode = &'static [(&'static str, &'static str)];

/// What the configuration holds for a setting without a default when nothing sets it.
enum Absent {
    /// In its mode, the configuration has no value for it: `None`, or empty.
    Holds(fn(&ServerConfig) -> bool),
    /// Its mode needs it: unset, the configuration is refused; set to this, it is taken.
    Refused(&'static str),
}

/// Every setting without a default: the mode it is read in and what the configuration holds
/// there unset, written from the configuration's types, apart from the registry.
fn absences() -> Vec<(&'static str, Mode, Absent)> {
    use Absent::{Holds, Refused};
    const ANY: Mode = &[];
    const REPLICA: Mode = &[
        (names::REPLICATION_MODE, "replica"),
        (names::STORE_MODE, "on-disk"),
    ];
    const STATIC: Mode = &[(names::AUTH_MODE, "bearer-static")];
    const STATIC_ADMIN: Mode = &[
        (names::AUTH_MODE, "bearer-static"),
        (names::AUTH_ADMIN_TOKEN, "a"),
    ];
    const JWT: Mode = &[(names::AUTH_MODE, "bearer-jwt")];
    const JWT_SECRET: Mode = &[
        (names::AUTH_MODE, "bearer-jwt"),
        (names::AUTH_JWT_SECRET, "s"),
    ];
    const MTLS: Mode = &[(names::AUTH_MODE, "mtls")];
    const MTLS_ADMIN: Mode = &[
        (names::AUTH_MODE, "mtls"),
        (names::AUTH_MTLS_ADMIN_SUBJECTS, "CN=a"),
    ];
    const OIDC: Mode = &[(names::AUTH_MODE, "oidc-introspection")];
    const OIDC_URL: Mode = &[
        (names::AUTH_MODE, "oidc-introspection"),
        (names::AUTH_OIDC_INTROSPECTION_URL, "https://idp/introspect"),
    ];
    const GEMINI: Mode = &[(names::AI_ENABLED, "true"), (names::AI_PROVIDER, "gemini")];
    const OPENROUTER: Mode = &[
        (names::AI_ENABLED, "true"),
        (names::AI_PROVIDER, "openrouter"),
    ];
    const OPENROUTER_KEY: Mode = &[
        (names::AI_ENABLED, "true"),
        (names::AI_PROVIDER, "openrouter"),
        (names::AI_OPENROUTER_API_KEY, "k"),
    ];
    vec![
        (
            "store.import_directory",
            ANY,
            Holds(|c| c.policy.import_directory.is_none()),
        ),
        (
            "store.ontology_path",
            ANY,
            Holds(|c| c.store.ontology_path.is_none()),
        ),
        ("reasoner.rules", ANY, Holds(|c| c.reasoner.rules.is_none())),
        (
            "auth.access_policy",
            ANY,
            Holds(|c| c.policy.access.is_none()),
        ),
        (
            "federation.allow",
            ANY,
            Holds(|c| c.store.federation.allow.is_empty()),
        ),
        (
            "budgets.result_cache_pins",
            ANY,
            Holds(|c| c.store.pinned_queries.is_empty()),
        ),
        (
            "replication.primary",
            REPLICA,
            Refused("https://primary:8080"),
        ),
        (
            "replication.token",
            ANY,
            Holds(|c| c.replication.token.is_none()),
        ),
        (
            "auth.bearer_static.read_token",
            STATIC_ADMIN,
            Holds(
                |c| matches!(&c.policy.auth, AuthConfig::BearerStatic(s) if s.read_token.is_none()),
            ),
        ),
        ("auth.bearer_static.admin_token", STATIC, Refused("a")),
        ("auth.bearer_jwt.shared_secret", JWT, Refused("s")),
        (
            "auth.bearer_jwt.issuer",
            JWT_SECRET,
            Holds(|c| matches!(&c.policy.auth, AuthConfig::BearerJwt(j) if j.issuer.is_none())),
        ),
        (
            "auth.bearer_jwt.audience",
            JWT_SECRET,
            Holds(|c| matches!(&c.policy.auth, AuthConfig::BearerJwt(j) if j.audience.is_none())),
        ),
        (
            "auth.mtls.read_subjects",
            MTLS_ADMIN,
            Holds(|c| matches!(&c.policy.auth, AuthConfig::Mtls(m) if m.read_subjects.is_empty())),
        ),
        ("auth.mtls.admin_subjects", MTLS, Refused("CN=a")),
        (
            "auth.oidc_introspection.introspection_url",
            OIDC,
            Refused("https://idp/introspect"),
        ),
        (
            "auth.oidc_introspection.client_id",
            OIDC_URL,
            Holds(
                |c| matches!(&c.policy.auth, AuthConfig::OidcIntrospection(o) if o.client_id.is_none()),
            ),
        ),
        (
            "auth.oidc_introspection.client_secret",
            OIDC_URL,
            Holds(
                |c| matches!(&c.policy.auth, AuthConfig::OidcIntrospection(o) if o.client_secret.is_none()),
            ),
        ),
        ("ai.gemini.api_key", GEMINI, Refused("k")),
        ("ai.openrouter.api_key", OPENROUTER, Refused("k")),
        (
            "ai.openrouter.site_url",
            OPENROUTER_KEY,
            Holds(
                |c| matches!(&c.ai.provider, AiProviderConfig::OpenRouter(o) if o.site_url.is_none()),
            ),
        ),
        (
            "ai.openrouter.app_name",
            OPENROUTER_KEY,
            Holds(
                |c| matches!(&c.ai.provider, AiProviderConfig::OpenRouter(o) if o.app_name.is_none()),
            ),
        ),
    ]
}

/// `setting`, which the schema gives no default, has none in the configuration either.
fn check_absence(
    setting: &settings::Setting,
    absences: &[(&'static str, Mode, Absent)],
    wrong: &mut Vec<String>,
) {
    let key = setting.key;
    let Some((_, mode, absent)) = absences.iter().find(|(name, ..)| *name == key) else {
        wrong.push(format!(
            "{key}: no default in the schema, and no absence check says what the \
             configuration holds unset"
        ));
        return;
    };
    if mode.iter().any(|(env, _)| *env == setting.env) {
        wrong.push(format!("{key}: its mode sets it"));
        return;
    }
    let mut source = KeyValueSource::default();
    for &(env, value) in *mode {
        source.insert(env, value);
    }
    match absent {
        Absent::Holds(holds) => match ServerConfig::from_source(&source) {
            Ok(config) if holds(&config) => {}
            Ok(_) => wrong.push(format!(
                "{key}: unset, the configuration holds a value: it has a default"
            )),
            Err(error) => wrong.push(format!("{key}: unset, in its mode: {error:#}")),
        },
        Absent::Refused(value) => {
            if ServerConfig::from_source(&source).is_ok() {
                wrong.push(format!(
                    "{key}: unset, its mode is accepted: it isn't needed"
                ));
            }
            source.insert(setting.env, *value);
            if let Err(error) = ServerConfig::from_source(&source) {
                wrong.push(format!(
                    "{key} = {value}: refused too, so the refusal isn't its absence: {error:#}"
                ));
            }
        }
    }
}

/// The cells of a table row, between its outer `|`.
fn cells(row: &str) -> Vec<&str> {
    row.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

/// The Default cell of `key` in the reference: in the table row with the anchor
/// `<a id="key">`, the column its table's header names Default.
fn default_cell<'a>(reference: &'a str, key: &str) -> Option<&'a str> {
    let anchor = format!("<a id=\"{key}\"></a>");
    let lines: Vec<&str> = reference.lines().map(str::trim).collect();
    let row = lines
        .iter()
        .position(|line| line.starts_with('|') && line.contains(&anchor))?;
    let mut header = row;
    while header > 0 && lines[header - 1].starts_with('|') {
        header -= 1;
    }
    let column = cells(lines[header])
        .iter()
        .position(|name| *name == "Default")?;
    cells(lines[row]).get(column).copied()
}

/// The schema states every default but those of [`UNSTATED_DEFAULTS`], and the reference
/// states each as the schema does: its Default cell starts with the value in code as the
/// schema writes it (`4GiB`, `false`, `1024`); a setting without a default starts with
/// none or required. Every setting is checked; the failures are reported together.
#[test]
fn the_reference_and_the_schema_state_the_same_defaults() {
    let schema = settings::schema();
    let reference = include_str!("../../../../docs/ops/config-reference.md");
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        let listed = UNSTATED_DEFAULTS.iter().any(|(key, _)| *key == setting.key);
        if listed != (setting.unset == Unset::Computed) {
            wrong.push(format!(
                "{}: computed in the registry and listed in UNSTATED_DEFAULTS must go together",
                setting.key
            ));
        }
        if listed {
            continue;
        }
        let Some(cell) = default_cell(reference, setting.key) else {
            wrong.push(format!(
                "{}: no row with a Default column in docs/ops/config-reference.md",
                setting.key
            ));
            continue;
        };
        match schema_default(&schema, setting.key) {
            Some(default) => {
                let stated = match &default {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let code = cell
                    .strip_prefix('`')
                    .and_then(|rest| rest.split('`').next());
                if code != Some(stated.as_str()) {
                    wrong.push(format!(
                        "{}: the schema's default is `{stated}`, the reference's cell \"{cell}\"",
                        setting.key
                    ));
                }
            }
            None if cell.starts_with("none") || cell.starts_with("required") => {}
            None => wrong.push(format!(
                "{}: no default in the schema, but the reference's cell is \"{cell}\"",
                setting.key
            )),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
