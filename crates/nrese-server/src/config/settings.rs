//! Every setting of the server, once: its key in the configuration file, its environment
//! variable, older file keys that still work, what values it takes and what it does.
//!
//! The file, the environment and the command line (`--set key=value`) are read through
//! this table into one key-value source (keyed by the environment names), from which the
//! typed configuration ([`super::ServerConfig`]) is parsed; precedence: command line,
//! environment, file, defaults. The JSON Schema of the configuration file is generated
//! from it ([`schema`], `nrese-server config-schema`), and a test checks that the
//! operator reference documents every setting.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::env_names as names;
use super::source::KeyValueSource;

/// What values a setting takes. The typed configuration parses and checks them; these
/// say what the file may hold and what the schema describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Bool,
    /// A whole number.
    Integer,
    /// A number, or a number with a unit as text.
    Amount(Unit),
    /// Text, or a list of texts (joined with `delimiter` for the environment).
    List {
        delimiter: &'static str,
    },
    /// One of these words (the parser also takes some older spellings).
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Bytes: `"4GiB"`, `"512MiB"`, `"2GB"`.
    Bytes,
    /// Bytes or a share of the machine's memory: `"8GiB"`, `"50%"`.
    BytesOrShare,
    /// Milliseconds: `"30s"`, `"2min"`, `"500ms"`.
    Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// The key in the configuration file (`store.data_dir`).
    pub key: &'static str,
    /// The environment variable.
    pub env: &'static str,
    /// Older file keys that still work; a setting under two of its keys is an error.
    pub older: &'static [&'static str],
    pub kind: Kind,
    /// A credential: never printed, write-only in the schema.
    pub secret: bool,
    pub doc: &'static str,
}

const fn setting(key: &'static str, env: &'static str, kind: Kind, doc: &'static str) -> Setting {
    Setting {
        key,
        env,
        older: &[],
        kind,
        secret: false,
        doc,
    }
}

const fn older(mut setting: Setting, keys: &'static [&'static str]) -> Setting {
    setting.older = keys;
    setting
}

const fn secret(mut setting: Setting) -> Setting {
    setting.secret = true;
    setting
}

use Kind::{Amount, Bool, Choice, Integer, List, Text};
use Unit::{Bytes, BytesOrShare, Duration};

/// Every setting, in the order of the operator reference.
pub const SETTINGS: &[Setting] = &[
    // [server]
    older(
        setting(
            "server.bind_address",
            names::BIND_ADDR,
            Text,
            "The address and port the server listens on (default 127.0.0.1:8080).",
        ),
        &["server.bind_addr"],
    ),
    setting(
        "server.deployment_posture",
        names::DEPLOYMENT_POSTURE,
        Choice(&[
            "open-workbench",
            "read-only-demo",
            "internal-authenticated",
            "replacement-grade",
        ]),
        "What the deployment is for; read-only-demo disables writes, internal-authenticated \
         needs authentication, replacement-grade also on-disk storage and problem+json parse \
         errors (default open-workbench).",
    ),
    // [store]
    setting(
        "store.mode",
        names::STORE_MODE,
        Choice(&["in-memory", "on-disk"]),
        "Where the store keeps its data.",
    ),
    setting(
        "store.data_dir",
        names::DATA_DIR,
        Text,
        "The data directory of an on-disk store (default ./data).",
    ),
    setting(
        "store.default_graph",
        names::DEFAULT_GRAPH,
        Choice(&["default", "union"]),
        "What a query that names no dataset reads: the default graph, or the union of all \
         graphs (default default).",
    ),
    setting(
        "store.import_directory",
        names::IMPORT_DIR,
        Text,
        "The directory administrators import files from by name; without it there are no \
         server-side imports.",
    ),
    setting(
        "store.verify_on_open",
        names::VERIFY_ON_OPEN,
        Bool,
        "Check the whole newest checkpoint when opening (default false).",
    ),
    setting(
        "store.map_checkpoints",
        names::MAP_CHECKPOINTS,
        Bool,
        "Serve checkpointed data from the mapped file and free its copies in memory \
         (default true).",
    ),
    setting(
        "store.wal_archive",
        names::WAL_ARCHIVE,
        Bool,
        "Keep the WAL segments checkpoints cover, for point-in-time restores (default false).",
    ),
    setting(
        "store.index_encoding",
        names::INDEX_ENCODING,
        Choice(&["fast", "compact"]),
        "How index blocks are encoded when they are built (default fast).",
    ),
    setting(
        "store.vocabulary",
        names::VOCABULARY,
        Choice(&["plain", "fsst"]),
        "How checkpoints store the dictionary's keys (default plain).",
    ),
    setting(
        "store.ontology_path",
        names::ONTOLOGY_PATH,
        Text,
        "A file loaded at startup; a missing file is a startup error.",
    ),
    // [shacl]
    setting(
        "shacl.shapes_graph",
        names::SHACL_SHAPES_GRAPH,
        Text,
        "The graph of the shapes validation uses (an IRI; default RDF4J's \
         http://rdf4j.org/schema/rdf4j#SHACLShapeGraph).",
    ),
    setting(
        "shacl.gate",
        names::SHACL_GATE,
        Choice(&["off", "report", "enforce"]),
        "SHACL as a commit gate: report what commits introduce, or reject them (default off).",
    ),
    setting(
        "shacl.gate_severity",
        names::SHACL_GATE_SEVERITY,
        Choice(&["violation", "warning", "info"]),
        "The least severity an enforcing gate rejects (default violation).",
    ),
    // [reasoner]
    setting(
        "reasoner.mode",
        names::REASONING_MODE,
        Choice(&[
            "disabled",
            "rdfs",
            "rdfs-full",
            "rdfs-plus",
            "owl-horst",
            "owl2-ql",
            "owl2-rl",
            "custom",
        ]),
        "The ruleset materialised into the inferred stack (default disabled).",
    ),
    setting(
        "reasoner.rules",
        names::REASONING_RULES,
        Text,
        "A Notation3 (.n3) or GraphDB (.pie) file of user rules: the whole program in the \
         custom mode, added to the ruleset otherwise.",
    ),
    setting(
        "reasoner.equality",
        names::REASONING_EQUALITY,
        Choice(&["representatives", "compact", "replicate"]),
        "How owl:sameAs is reasoned with and stored (default representatives).",
    ),
    setting(
        "reasoner.equality_answers",
        names::REASONING_EQUALITY_ANSWERS,
        Choice(&["strict", "canonical"]),
        "With equality = compact: answer with every identity of an owl:sameAs class \
         (strict, the default), or with its representative only (canonical, for analytics).",
    ),
    setting(
        "reasoner.equality_expansion",
        names::REASONING_EQUALITY_EXPANSION,
        Choice(&["late", "early"]),
        "With equality = compact: expand owl:sameAs classes after the joins where the \
         answer can't tell (late, the default) or at every read (early). The same answers.",
    ),
    setting(
        "reasoner.unnamed_classes",
        names::REASONING_UNNAMED_CLASSES,
        Choice(&["derive", "skip"]),
        "Whether memberships in unnamed union classes that nothing consumes are derived \
         (default derive).",
    ),
    setting(
        "reasoner.support_sets",
        names::REASONING_SUPPORT_SETS,
        Integer,
        "Support graph sets kept per inferred statement for inferred = \"supported\" in the \
         access policy (default 16, at least 1); fewer can only hide statements.",
    ),
    // [federation]
    setting(
        "federation.allow",
        names::FEDERATION_ALLOW,
        List { delimiter: "," },
        "The endpoints SERVICE may call (IRIs or prefixes, * for any); none by default.",
    ),
    setting(
        "federation.timeout",
        names::FEDERATION_TIMEOUT_MS,
        Amount(Duration),
        "Per request to an endpoint (default 30s).",
    ),
    setting(
        "federation.max_rows",
        names::FEDERATION_MAX_ROWS,
        Integer,
        "Rows one request may return (default 1000000).",
    ),
    // [budgets]
    older(
        setting(
            "budgets.query_memory",
            names::MAX_QUERY_MEMORY_BYTES,
            Amount(BytesOrShare),
            "Intermediate results of one query; 0 is unlimited (default 4GiB).",
        ),
        &["policy.limits.max_query_memory_bytes"],
    ),
    setting(
        "budgets.total_query_memory",
        names::MAX_TOTAL_QUERY_MEMORY_BYTES,
        Amount(BytesOrShare),
        "Intermediate results of all running queries together; 0 is unlimited (default 50%).",
    ),
    setting(
        "budgets.bulk_load_memory",
        names::BULK_LOAD_MEMORY,
        Amount(BytesOrShare),
        "The quads of a bulk load before they are sorted in chunks and spilled; 0 is \
         unlimited (default 25%).",
    ),
    older(
        setting(
            "budgets.query_timeout",
            names::QUERY_TIMEOUT_MS,
            Amount(Duration),
            "A query, until its last result is sent (default 30s).",
        ),
        &["policy.timeouts.query_ms"],
    ),
    older(
        setting(
            "budgets.update_timeout",
            names::UPDATE_TIMEOUT_MS,
            Amount(Duration),
            "A SPARQL update, reasoning included (default 60s).",
        ),
        &["policy.timeouts.update_ms"],
    ),
    older(
        setting(
            "budgets.graph_read_timeout",
            names::GRAPH_READ_TIMEOUT_MS,
            Amount(Duration),
            "A Graph Store or statements read (default 30s).",
        ),
        &["policy.timeouts.graph_read_ms"],
    ),
    older(
        setting(
            "budgets.graph_write_timeout",
            names::GRAPH_WRITE_TIMEOUT_MS,
            Amount(Duration),
            "A Graph Store write (default 60s).",
        ),
        &["policy.timeouts.graph_write_ms"],
    ),
    older(
        setting(
            "budgets.query_text",
            names::MAX_QUERY_BYTES,
            Amount(Bytes),
            "The text of a query (default 1MiB).",
        ),
        &["policy.limits.max_query_bytes"],
    ),
    older(
        setting(
            "budgets.update_size",
            names::MAX_UPDATE_BYTES,
            Amount(Bytes),
            "A SPARQL update request (default 16MiB).",
        ),
        &["policy.limits.max_update_bytes"],
    ),
    older(
        setting(
            "budgets.upload_size",
            names::MAX_RDF_UPLOAD_BYTES,
            Amount(Bytes),
            "An RDF payload (default 128MiB).",
        ),
        &["policy.limits.max_rdf_upload_bytes"],
    ),
    older(
        setting(
            "budgets.result_cache",
            names::QUERY_CACHE_BYTES,
            Amount(BytesOrShare),
            "Serialised results kept for repeated queries; 0 switches the cache off \
             (default 2% of memory, 64MiB to 8GiB).",
        ),
        &["store.query_cache_bytes"],
    ),
    // [policy]
    setting(
        "policy.rate_limits.window_secs",
        names::RATE_LIMIT_WINDOW_SECS,
        Integer,
        "The rate limits' window in seconds.",
    ),
    setting(
        "policy.rate_limits.read_requests_per_window",
        names::READ_REQUESTS_PER_WINDOW,
        Integer,
        "Read requests per client and window.",
    ),
    setting(
        "policy.rate_limits.write_requests_per_window",
        names::WRITE_REQUESTS_PER_WINDOW,
        Integer,
        "Write requests per client and window.",
    ),
    setting(
        "policy.rate_limits.admin_requests_per_window",
        names::ADMIN_REQUESTS_PER_WINDOW,
        Integer,
        "Administrative requests per client and window.",
    ),
    setting(
        "policy.sparql_parse_error_profile",
        names::SPARQL_PARSE_ERROR_PROFILE,
        Choice(&["problem-json", "plain-text", "fuseki-plain-text"]),
        "How SPARQL syntax errors are answered (default problem-json).",
    ),
    setting(
        "policy.exposure.operator_ui",
        names::ENABLE_OPERATOR_UI,
        Bool,
        "Whether the operator console is served.",
    ),
    setting(
        "policy.exposure.metrics",
        names::ENABLE_METRICS,
        Bool,
        "Whether the Prometheus metrics are served.",
    ),
    // [auth]
    setting(
        "auth.mode",
        names::AUTH_MODE,
        Choice(&[
            "none",
            "bearer-static",
            "bearer-jwt",
            "mtls",
            "oidc-introspection",
        ]),
        "How requests are authenticated (default none).",
    ),
    setting(
        "auth.access_policy",
        names::ACCESS_POLICY,
        Text,
        "An access policy file, imported into the access state at the first start.",
    ),
    setting(
        "auth.workspace_base",
        names::WORKSPACE_BASE,
        Text,
        "What personal space and workspace graph prefixes start with (default urn:nrese:).",
    ),
    setting(
        "auth.local_logins",
        names::LOCAL_LOGINS,
        Bool,
        "Whether users of the access state log in with passwords (default true).",
    ),
    setting(
        "auth.trusted_proxies",
        names::AUTH_TRUSTED_PROXIES,
        List { delimiter: "," },
        "The proxies (addresses or ranges) whose X-Forwarded-For names the client, for login throttling (default loopback).",
    ),
    secret(setting(
        "auth.bearer_static.read_token",
        names::AUTH_READ_TOKEN,
        Text,
        "The static bearer token that may read.",
    )),
    secret(setting(
        "auth.bearer_static.admin_token",
        names::AUTH_ADMIN_TOKEN,
        Text,
        "The static bearer token that may administer.",
    )),
    secret(setting(
        "auth.bearer_jwt.shared_secret",
        names::AUTH_JWT_SECRET,
        Text,
        "The HMAC secret JWTs are signed with.",
    )),
    setting(
        "auth.bearer_jwt.issuer",
        names::AUTH_JWT_ISSUER,
        Text,
        "The issuer JWTs must name.",
    ),
    setting(
        "auth.bearer_jwt.audience",
        names::AUTH_JWT_AUDIENCE,
        Text,
        "The audience JWTs must name.",
    ),
    setting(
        "auth.bearer_jwt.read_role",
        names::AUTH_JWT_READ_ROLE,
        Text,
        "The role that may read.",
    ),
    setting(
        "auth.bearer_jwt.admin_role",
        names::AUTH_JWT_ADMIN_ROLE,
        Text,
        "The role that may administer.",
    ),
    setting(
        "auth.bearer_jwt.leeway_seconds",
        names::AUTH_JWT_LEEWAY_SECS,
        Integer,
        "Clock leeway for expiry checks, in seconds.",
    ),
    setting(
        "auth.mtls.subject_header",
        names::AUTH_MTLS_SUBJECT_HEADER,
        Text,
        "The header the TLS-terminating proxy passes the client certificate's subject in.",
    ),
    setting(
        "auth.mtls.read_subjects",
        names::AUTH_MTLS_READ_SUBJECTS,
        List { delimiter: ";" },
        "The subjects that may read.",
    ),
    setting(
        "auth.mtls.admin_subjects",
        names::AUTH_MTLS_ADMIN_SUBJECTS,
        List { delimiter: ";" },
        "The subjects that may administer.",
    ),
    setting(
        "auth.mtls.trusted_proxies",
        names::AUTH_MTLS_TRUSTED_PROXIES,
        List { delimiter: "," },
        "The peers (addresses or ranges) that may send the subject header (default loopback).",
    ),
    setting(
        "auth.oidc_introspection.introspection_url",
        names::AUTH_OIDC_INTROSPECTION_URL,
        Text,
        "The OAuth 2.0 token introspection endpoint.",
    ),
    setting(
        "auth.oidc_introspection.client_id",
        names::AUTH_OIDC_CLIENT_ID,
        Text,
        "The client id the server introspects as.",
    ),
    secret(setting(
        "auth.oidc_introspection.client_secret",
        names::AUTH_OIDC_CLIENT_SECRET,
        Text,
        "The client secret the server introspects with.",
    )),
    setting(
        "auth.oidc_introspection.read_role",
        names::AUTH_OIDC_READ_ROLE,
        Text,
        "The role that may read.",
    ),
    setting(
        "auth.oidc_introspection.admin_role",
        names::AUTH_OIDC_ADMIN_ROLE,
        Text,
        "The role that may administer.",
    ),
    setting(
        "auth.oidc_introspection.timeout_ms",
        names::AUTH_OIDC_TIMEOUT_MS,
        Integer,
        "Per introspection request, in milliseconds.",
    ),
    // [ai]
    setting(
        "ai.enabled",
        names::AI_ENABLED,
        Bool,
        "Whether AI query suggestions are offered (default false).",
    ),
    setting(
        "ai.provider",
        names::AI_PROVIDER,
        Choice(&["disabled", "gemini", "openrouter"]),
        "Who makes the suggestions.",
    ),
    setting("ai.model", names::AI_MODEL, Text, "The provider's model."),
    setting(
        "ai.timeout_ms",
        names::AI_TIMEOUT_MS,
        Integer,
        "Per suggestion request, in milliseconds.",
    ),
    setting(
        "ai.max_suggestions",
        names::AI_MAX_SUGGESTIONS,
        Integer,
        "Suggestions per request.",
    ),
    setting(
        "ai.system_prompt",
        names::AI_SYSTEM_PROMPT,
        Text,
        "The system prompt of suggestion requests.",
    ),
    secret(setting(
        "ai.gemini.api_key",
        names::AI_GOOGLE_API_KEY,
        Text,
        "The Gemini API key.",
    )),
    setting(
        "ai.gemini.api_base",
        names::AI_GOOGLE_API_BASE,
        Text,
        "The Gemini API's base URL.",
    ),
    secret(setting(
        "ai.openrouter.api_key",
        names::AI_OPENROUTER_API_KEY,
        Text,
        "The OpenRouter API key.",
    )),
    setting(
        "ai.openrouter.api_base",
        names::AI_OPENROUTER_API_BASE,
        Text,
        "The OpenRouter API's base URL.",
    ),
    setting(
        "ai.openrouter.site_url",
        names::AI_OPENROUTER_SITE_URL,
        Text,
        "The site URL sent to OpenRouter.",
    ),
    setting(
        "ai.openrouter.app_name",
        names::AI_OPENROUTER_APP_NAME,
        Text,
        "The application name sent to OpenRouter.",
    ),
];

/// The setting with file key `key` (or an older one), and whether `key` is an older one.
pub fn by_key(key: &str) -> Option<&'static Setting> {
    SETTINGS
        .iter()
        .find(|setting| setting.key == key || setting.older.contains(&key))
}

/// Whether `key` is a table of the file (a prefix of setting keys).
fn is_table(key: &str) -> bool {
    SETTINGS.iter().any(|setting| {
        std::iter::once(setting.key)
            .chain(setting.older.iter().copied())
            .any(|name| {
                name.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
    })
}

/// The settings of a parsed configuration file, by environment name. Unknown keys, values
/// of the wrong kind, and a setting under two of its keys are errors that name the key.
pub(super) fn from_file(document: &toml::Table) -> Result<KeyValueSource> {
    let mut values: BTreeMap<&'static str, (String, String)> = BTreeMap::new();
    walk(document, "", &mut values)?;
    let mut source = KeyValueSource::default();
    for (env, (_, value)) in values {
        source.insert(env, value);
    }
    Ok(source)
}

fn walk(
    table: &toml::Table,
    prefix: &str,
    values: &mut BTreeMap<&'static str, (String, String)>,
) -> Result<()> {
    for (name, value) in table {
        let key = match prefix {
            "" => name.clone(),
            prefix => format!("{prefix}.{name}"),
        };
        if let (toml::Value::Table(inner), true) = (value, is_table(&key)) {
            walk(inner, &key, values)?;
            continue;
        }
        let Some(setting) = by_key(&key) else {
            bail!("unknown key '{key}'");
        };
        let text = file_value(setting, &key, value)?;
        if let Some((first, _)) = values.get(setting.env) {
            bail!("{} is set twice: as '{first}' and as '{key}'", setting.key);
        }
        values.insert(setting.env, (key, text));
    }
    Ok(())
}

/// A file value as the environment would hold it.
fn file_value(setting: &Setting, key: &str, value: &toml::Value) -> Result<String> {
    use toml::Value as V;
    let wrong = |expected: &str| anyhow::anyhow!("'{key}' must be {expected}, not {value}");
    Ok(match (setting.kind, value) {
        (Text | Choice(_), V::String(text)) => text.clone(),
        (Text | Choice(_), _) => return Err(wrong("text")),
        (Bool, V::Boolean(flag)) => flag.to_string(),
        (Bool, _) => return Err(wrong("true or false")),
        (Integer, V::Integer(number)) if *number >= 0 => number.to_string(),
        (Integer, _) => return Err(wrong("a whole number, 0 or more")),
        (Amount(_), V::Integer(number)) if *number >= 0 => number.to_string(),
        (Amount(_), V::String(text)) => text.clone(),
        (Amount(_), _) => return Err(wrong("a number or a number with a unit")),
        (List { .. }, V::String(text)) => text.clone(),
        (List { delimiter }, V::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| wrong("text or a list of texts"))
            })
            .collect::<Result<Vec<_>>>()?
            .join(delimiter),
        (List { .. }, _) => return Err(wrong("text or a list of texts")),
    })
}

/// Command-line overrides (`--set key=value`) by environment name. Secrets are refused:
/// a command line is visible to every user of the machine (the process list).
pub(super) fn from_overrides(overrides: &[(String, String)]) -> Result<KeyValueSource> {
    let mut source = KeyValueSource::default();
    for (key, value) in overrides {
        let Some(setting) = by_key(key) else {
            bail!("--set: unknown key '{key}'");
        };
        if setting.secret {
            bail!(
                "--set: '{key}' is a secret, and a command line is visible in the process list; set it in the configuration file or as {}",
                setting.env
            );
        }
        source.insert(setting.env, value.clone());
    }
    Ok(source)
}

/// The JSON Schema (2020-12) of the configuration file.
pub fn schema() -> Value {
    let mut root = Map::new();
    // Each setting under its key, and under its older keys, deprecated (they still work).
    let entries = SETTINGS.iter().flat_map(|setting| {
        std::iter::once((setting.key, property(setting))).chain(setting.older.iter().map(|older| {
            let mut property = property(setting);
            property["deprecated"] = json!(true);
            property["description"] = json!(format!("Older name of `{}`.", setting.key));
            (*older, property)
        }))
    });
    for (key, property) in entries {
        let mut node = &mut root;
        let mut parts = key.split('.').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.insert(part.to_owned(), property);
                break;
            }
            let table = node.entry(part.to_owned()).or_insert_with(
                || json!({ "type": "object", "additionalProperties": false, "properties": {} }),
            );
            node = table["properties"]
                .as_object_mut()
                .expect("a table's properties are an object");
        }
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://nrese.dev/schema/config.json",
        "title": "NRESE server configuration (config.toml)",
        "type": "object",
        "additionalProperties": false,
        "properties": root,
    })
}

fn property(setting: &Setting) -> Value {
    let mut property = match setting.kind {
        Text => json!({ "type": "string" }),
        Bool => json!({ "type": "boolean" }),
        Integer => json!({ "type": "integer", "minimum": 0 }),
        Amount(unit) => json!({
            "type": ["integer", "string"],
            "minimum": 0,
            "x-unit": match unit {
                Bytes => "bytes",
                BytesOrShare => "bytes-or-share",
                Duration => "milliseconds",
            },
        }),
        List { .. } => json!({
            "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
            ],
        }),
        Choice(values) => json!({ "type": "string", "enum": values }),
    };
    let object = property.as_object_mut().expect("an object");
    object.insert("description".to_owned(), json!(setting.doc));
    object.insert("x-env".to_owned(), json!(setting.env));
    if setting.secret {
        object.insert("writeOnly".to_owned(), json!(true));
    }
    if !setting.older.is_empty() {
        object.insert("x-older-keys".to_owned(), json!(setting.older));
    }
    property
}

#[cfg(test)]
mod tests {
    #[test]
    fn secrets_are_refused_on_the_command_line() {
        let set = |key: &str| super::from_overrides(&[(key.to_owned(), "x".to_owned())]);
        let error = set("auth.bearer_jwt.shared_secret")
            .unwrap_err()
            .to_string();
        assert!(error.contains("secret"), "{error}");
        assert!(set("auth.bearer_jwt.issuer").is_ok());
    }

    use super::*;

    #[test]
    fn every_environment_name_is_one_setting() {
        let mut envs: Vec<&str> = SETTINGS.iter().map(|setting| setting.env).collect();
        let count = envs.len();
        envs.sort_unstable();
        envs.dedup();
        assert_eq!(envs.len(), count, "an environment name twice");
        let mut keys: Vec<&str> = SETTINGS
            .iter()
            .flat_map(|setting| std::iter::once(setting.key).chain(setting.older.iter().copied()))
            .collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "a file key twice");
        // Every name the configuration reads is a setting (but the config file's path).
        let source = include_str!("env_names.rs");
        for line in source.lines().filter(|line| line.starts_with("pub const ")) {
            let env = line.split('"').nth(1).expect("a quoted name");
            assert!(
                env == names::CONFIG_PATH || SETTINGS.iter().any(|setting| setting.env == env),
                "{env} is no setting"
            );
        }
    }

    /// The operator reference documents every setting: its key and its environment name.
    #[test]
    fn the_reference_documents_every_setting() {
        let reference = include_str!("../../../../docs/ops/config-reference.md");
        for setting in SETTINGS {
            assert!(
                reference.contains(setting.key) && reference.contains(setting.env),
                "docs/ops/config-reference.md doesn't document {} ({})",
                setting.key,
                setting.env
            );
        }
    }

    /// Every listed choice is a value the configuration takes.
    #[test]
    fn every_choice_is_accepted() {
        for setting in SETTINGS {
            let Choice(values) = setting.kind else {
                continue;
            };
            for value in values {
                let mut source = KeyValueSource::default();
                source.insert(setting.env, *value);
                // What some choices need besides.
                match (setting.key, *value) {
                    ("reasoner.mode", "custom") => {
                        let dir = tempfile::tempdir().expect("temp dir");
                        let rules = dir.path().join("rules.n3");
                        std::fs::write(&rules, "{ ?x <urn:p> ?y } => { ?y <urn:p> ?x } .")
                            .expect("rules");
                        source.insert(names::REASONING_RULES, rules.display().to_string());
                        super::super::ServerConfig::from_source(&source)
                            .unwrap_or_else(|error| panic!("{}={value}: {error:#}", setting.key));
                        continue;
                    }
                    ("server.deployment_posture", "internal-authenticated") => {
                        source.insert(names::AUTH_MODE, "bearer-static");
                        source.insert(names::AUTH_READ_TOKEN, "r");
                        source.insert(names::AUTH_ADMIN_TOKEN, "a");
                    }
                    ("server.deployment_posture", "replacement-grade") => {
                        source.insert(names::AUTH_MODE, "bearer-static");
                        source.insert(names::AUTH_READ_TOKEN, "r");
                        source.insert(names::AUTH_ADMIN_TOKEN, "a");
                        source.insert(names::STORE_MODE, "on-disk");
                    }
                    ("auth.mode", "bearer-static") => {
                        source.insert(names::AUTH_READ_TOKEN, "r");
                        source.insert(names::AUTH_ADMIN_TOKEN, "a");
                    }
                    ("auth.mode", "bearer-jwt") => {
                        source.insert(names::AUTH_JWT_SECRET, "s");
                    }
                    ("auth.mode", "mtls") => {
                        source.insert(names::AUTH_MTLS_ADMIN_SUBJECTS, "CN=a");
                    }
                    ("auth.mode", "oidc-introspection") => {
                        source.insert(names::AUTH_OIDC_INTROSPECTION_URL, "https://idp/introspect");
                        source.insert(names::AUTH_OIDC_CLIENT_ID, "c");
                        source.insert(names::AUTH_OIDC_CLIENT_SECRET, "s");
                    }
                    ("ai.provider", "gemini") => {
                        source.insert(names::AI_GOOGLE_API_KEY, "k");
                    }
                    ("ai.provider", "openrouter") => {
                        source.insert(names::AI_OPENROUTER_API_KEY, "k");
                    }
                    _ => {}
                }
                super::super::ServerConfig::from_source(&source)
                    .unwrap_or_else(|error| panic!("{}={value}: {error:#}", setting.key));
            }
        }
    }

    #[test]
    fn the_schema_nests_the_tables_and_marks_secrets() {
        let schema = schema();
        let budgets = &schema["properties"]["budgets"]["properties"];
        assert_eq!(budgets["query_timeout"]["x-env"], names::QUERY_TIMEOUT_MS);
        assert_eq!(
            budgets["query_timeout"]["x-older-keys"][0],
            "policy.timeouts.query_ms"
        );
        let jwt = &schema["properties"]["auth"]["properties"]["bearer_jwt"]["properties"];
        assert_eq!(jwt["shared_secret"]["writeOnly"], true);
        let timeouts = &schema["properties"]["policy"]["properties"]["timeouts"]["properties"];
        assert_eq!(timeouts["query_ms"]["deprecated"], true);
        assert_eq!(timeouts["query_ms"]["x-env"], names::QUERY_TIMEOUT_MS);
        assert_eq!(
            schema["properties"]["store"]["properties"]["mode"]["enum"][1],
            "on-disk"
        );
    }
}
