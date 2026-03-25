use std::collections::BTreeSet;

use anyhow::{Context, Result};

use crate::auth::{
    AuthConfig, JwtBearerConfig, MtlsConfig, OidcIntrospectionConfig, StaticBearerConfig,
};

use super::env_names as names;
use super::env_values::parse_u64;
use super::source::ConfigSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfiguredAuthMode {
    None,
    BearerStatic,
    BearerJwt,
    Mtls,
    OidcIntrospection,
}

pub(super) fn parse_auth_config(source: &dyn ConfigSource) -> Result<AuthConfig> {
    match parse_auth_mode(source.get(names::AUTH_MODE).as_deref())? {
        ConfiguredAuthMode::None => Ok(AuthConfig::None),
        ConfiguredAuthMode::BearerStatic => Ok(AuthConfig::BearerStatic(StaticBearerConfig {
            read_token: source.get(names::AUTH_READ_TOKEN),
            admin_token: source
                .get(names::AUTH_ADMIN_TOKEN)
                .context("NRESE_AUTH_ADMIN_TOKEN is required in bearer-static mode")?,
        })),
        ConfiguredAuthMode::BearerJwt => Ok(AuthConfig::BearerJwt(JwtBearerConfig {
            shared_secret: source
                .get(names::AUTH_JWT_SECRET)
                .context("NRESE_AUTH_JWT_SECRET is required in bearer-jwt mode")?,
            issuer: source.get(names::AUTH_JWT_ISSUER),
            audience: source.get(names::AUTH_JWT_AUDIENCE),
            read_role: source
                .get(names::AUTH_JWT_READ_ROLE)
                .unwrap_or_else(|| "nrese.read".to_owned()),
            admin_role: source
                .get(names::AUTH_JWT_ADMIN_ROLE)
                .unwrap_or_else(|| "nrese.admin".to_owned()),
            leeway_seconds: parse_u64(source, names::AUTH_JWT_LEEWAY_SECS, 30)?,
        })),
        ConfiguredAuthMode::Mtls => Ok(AuthConfig::Mtls(MtlsConfig {
            subject_header: source
                .get(names::AUTH_MTLS_SUBJECT_HEADER)
                .unwrap_or_else(|| "x-client-cert-subject".to_owned()),
            read_subjects: parse_semicolon_set(source.get(names::AUTH_MTLS_READ_SUBJECTS)),
            admin_subjects: {
                let admin_subjects =
                    parse_semicolon_set(source.get(names::AUTH_MTLS_ADMIN_SUBJECTS));
                if admin_subjects.is_empty() {
                    anyhow::bail!("NRESE_AUTH_MTLS_ADMIN_SUBJECTS is required in mtls mode");
                }
                admin_subjects
            },
        })),
        ConfiguredAuthMode::OidcIntrospection => Ok(AuthConfig::OidcIntrospection(
            OidcIntrospectionConfig::new(
                source
                    .get(names::AUTH_OIDC_INTROSPECTION_URL)
                    .context(
                        "NRESE_AUTH_OIDC_INTROSPECTION_URL is required in oidc-introspection mode",
                    )?
                    .parse()
                    .context("failed to parse NRESE_AUTH_OIDC_INTROSPECTION_URL")?,
                source.get(names::AUTH_OIDC_CLIENT_ID),
                source.get(names::AUTH_OIDC_CLIENT_SECRET),
                source
                    .get(names::AUTH_OIDC_READ_ROLE)
                    .unwrap_or_else(|| "nrese.read".to_owned()),
                source
                    .get(names::AUTH_OIDC_ADMIN_ROLE)
                    .unwrap_or_else(|| "nrese.admin".to_owned()),
                parse_u64(source, names::AUTH_OIDC_TIMEOUT_MS, 5_000)?,
            )
            .context("failed to construct oidc introspection client")?,
        )),
    }
}

pub(super) fn parse_auth_mode(input: Option<&str>) -> Result<ConfiguredAuthMode> {
    match input.unwrap_or("none").to_ascii_lowercase().as_str() {
        "none" => Ok(ConfiguredAuthMode::None),
        "bearer-static" | "bearer_static" => Ok(ConfiguredAuthMode::BearerStatic),
        "bearer-jwt" | "bearer_jwt" => Ok(ConfiguredAuthMode::BearerJwt),
        "mtls" => Ok(ConfiguredAuthMode::Mtls),
        "oidc-introspection" | "oidc_introspection" => Ok(ConfiguredAuthMode::OidcIntrospection),
        other => anyhow::bail!("unsupported auth mode: {other}"),
    }
}

fn parse_semicolon_set(value: Option<String>) -> BTreeSet<String> {
    match value {
        Some(value) => value
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        None => BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::auth::{AuthConfig, MtlsConfig, OidcIntrospectionConfig, StaticBearerConfig};
    use crate::config::source::ProcessEnv;
    use crate::config::test_support::{EnvGuard, env_lock};

    use super::{ConfiguredAuthMode, parse_auth_config, parse_auth_mode};

    #[test]
    fn auth_mode_parser_accepts_static_bearer() {
        assert_eq!(
            parse_auth_mode(Some("bearer-static")).expect("auth mode should parse"),
            ConfiguredAuthMode::BearerStatic
        );
    }

    #[test]
    fn bearer_jwt_mode_parser_accepts_kebab_case() {
        assert_eq!(
            parse_auth_mode(Some("bearer-jwt")).expect("auth mode should parse"),
            ConfiguredAuthMode::BearerJwt
        );
    }

    #[test]
    fn oidc_introspection_mode_parser_accepts_kebab_case() {
        assert_eq!(
            parse_auth_mode(Some("oidc-introspection")).expect("auth mode should parse"),
            ConfiguredAuthMode::OidcIntrospection
        );
    }

    #[test]
    fn bearer_static_config_requires_admin_token() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("bearer-static")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert!(parse_auth_config(&ProcessEnv).is_err());
    }

    #[test]
    fn bearer_static_config_parses_explicit_tokens() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("bearer-static")),
            ("NRESE_AUTH_READ_TOKEN", Some("reader")),
            ("NRESE_AUTH_ADMIN_TOKEN", Some("admin")),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert_eq!(
            parse_auth_config(&ProcessEnv).expect("auth config"),
            AuthConfig::BearerStatic(StaticBearerConfig {
                read_token: Some("reader".to_owned()),
                admin_token: "admin".to_owned(),
            })
        );
    }

    #[test]
    fn bearer_jwt_config_requires_shared_secret() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("bearer-jwt")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert!(parse_auth_config(&ProcessEnv).is_err());
    }

    #[test]
    fn mtls_config_requires_admin_subjects() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("mtls")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert!(parse_auth_config(&ProcessEnv).is_err());
    }

    #[test]
    fn mtls_config_parses_subject_mapping() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("mtls")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", Some("x-ssl-client-s-dn")),
            (
                "NRESE_AUTH_MTLS_READ_SUBJECTS",
                Some("CN=reader-1,O=Test;CN=reader-2,O=Test"),
            ),
            (
                "NRESE_AUTH_MTLS_ADMIN_SUBJECTS",
                Some("CN=admin-1,O=Test;CN=admin-2,O=Test"),
            ),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert_eq!(
            parse_auth_config(&ProcessEnv).expect("auth config"),
            AuthConfig::Mtls(MtlsConfig {
                subject_header: "x-ssl-client-s-dn".to_owned(),
                read_subjects: BTreeSet::from([
                    "CN=reader-1,O=Test".to_owned(),
                    "CN=reader-2,O=Test".to_owned()
                ]),
                admin_subjects: BTreeSet::from([
                    "CN=admin-1,O=Test".to_owned(),
                    "CN=admin-2,O=Test".to_owned()
                ]),
            })
        );
    }

    #[test]
    fn oidc_introspection_requires_introspection_url() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("oidc-introspection")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            ("NRESE_AUTH_OIDC_INTROSPECTION_URL", None),
            ("NRESE_AUTH_OIDC_CLIENT_ID", None),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", None),
            ("NRESE_AUTH_OIDC_READ_ROLE", None),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", None),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", None),
        ]);

        assert!(parse_auth_config(&ProcessEnv).is_err());
    }

    #[test]
    fn oidc_introspection_parses_explicit_config() {
        let _lock = env_lock().lock().expect("env test lock");
        let _guard = EnvGuard::set(&[
            ("NRESE_AUTH_MODE", Some("oidc-introspection")),
            ("NRESE_AUTH_READ_TOKEN", None),
            ("NRESE_AUTH_ADMIN_TOKEN", None),
            ("NRESE_AUTH_JWT_SECRET", None),
            ("NRESE_AUTH_JWT_ISSUER", None),
            ("NRESE_AUTH_JWT_AUDIENCE", None),
            ("NRESE_AUTH_JWT_READ_ROLE", None),
            ("NRESE_AUTH_JWT_ADMIN_ROLE", None),
            ("NRESE_AUTH_JWT_LEEWAY_SECS", None),
            ("NRESE_AUTH_MTLS_SUBJECT_HEADER", None),
            ("NRESE_AUTH_MTLS_READ_SUBJECTS", None),
            ("NRESE_AUTH_MTLS_ADMIN_SUBJECTS", None),
            (
                "NRESE_AUTH_OIDC_INTROSPECTION_URL",
                Some("http://127.0.0.1:43123/introspect"),
            ),
            ("NRESE_AUTH_OIDC_CLIENT_ID", Some("nrese-client")),
            ("NRESE_AUTH_OIDC_CLIENT_SECRET", Some("secret")),
            ("NRESE_AUTH_OIDC_READ_ROLE", Some("nrese.read")),
            ("NRESE_AUTH_OIDC_ADMIN_ROLE", Some("nrese.admin")),
            ("NRESE_AUTH_OIDC_TIMEOUT_MS", Some("7000")),
        ]);

        assert_eq!(
            parse_auth_config(&ProcessEnv).expect("auth config"),
            AuthConfig::OidcIntrospection(
                OidcIntrospectionConfig::new(
                    "http://127.0.0.1:43123/introspect".parse().expect("url"),
                    Some("nrese-client".to_owned()),
                    Some("secret".to_owned()),
                    "nrese.read".to_owned(),
                    "nrese.admin".to_owned(),
                    7000,
                )
                .expect("oidc config"),
            )
        );
    }
}
