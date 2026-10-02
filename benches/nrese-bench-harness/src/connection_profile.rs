use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::interpolation::{expand_env_placeholders, expand_headers_env_placeholders};
use crate::model::{
    BasicAuthConfig, ConnectionProfilesRegistry, LiveConnectionProfile, ReferenceConnection,
    ReferenceKind, ServiceConnectionConfig, ServiceConnectionProfile, ServiceInvocationProfiles,
    ServiceRequestProfile,
};

pub fn read_connection_profiles_registry(path: &Path) -> Result<ConnectionProfilesRegistry> {
    let text = fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    let mut registry: ConnectionProfilesRegistry =
        toml::from_str(&text).with_context(|| format!("failed to parse {:?}", path))?;

    for profile in registry.profiles.values_mut() {
        normalize_live_connection_profile(profile)?;
    }

    Ok(registry)
}

pub fn resolve_live_connection_profile<'a>(
    registry: &'a ConnectionProfilesRegistry,
    profile_name: &str,
) -> Result<&'a LiveConnectionProfile> {
    registry.profiles.get(profile_name).ok_or_else(|| {
        anyhow!(
            "connection profile '{profile_name}' is not defined in the connection profile registry"
        )
    })
}

pub fn resolve_required_service_connection(
    label: &str,
    profile: Option<&ServiceConnectionProfile>,
    cli_base_url: Option<&str>,
    cli_basic_auth: Option<&BasicAuthConfig>,
) -> Result<ServiceConnectionConfig> {
    resolve_optional_service_connection(label, profile, cli_base_url, cli_basic_auth)?.ok_or_else(
        || anyhow!("missing {label} connection; provide --{}-base-url or a selected connection profile", label.to_ascii_lowercase()),
    )
}

pub fn resolve_optional_service_connection(
    label: &str,
    profile: Option<&ServiceConnectionProfile>,
    cli_base_url: Option<&str>,
    cli_basic_auth: Option<&BasicAuthConfig>,
) -> Result<Option<ServiceConnectionConfig>> {
    let Some(base_url) = cli_base_url
        .map(str::to_owned)
        .or_else(|| profile.map(|profile| profile.base_url.clone()))
    else {
        return Ok(None);
    };

    if base_url.trim().is_empty() {
        bail!("{label} connection base_url must not be empty");
    }

    let headers = profile
        .map(|profile| profile.headers.clone())
        .unwrap_or_default();
    let timeout_ms = profile.and_then(|profile| profile.timeout_ms);
    let basic_auth = cli_basic_auth.cloned().or_else(|| {
        profile
            .and_then(|profile| profile.basic_auth.as_ref())
            .map(to_runtime_basic_auth)
    });

    Ok(Some(ServiceConnectionConfig {
        base_url,
        headers,
        timeout_ms,
        basic_auth,
        endpoints: profile
            .map(|profile| profile.endpoints.clone())
            .unwrap_or_default(),
    }))
}

/// Resolves the reference side. The engine kind comes from the CLI or the profile; a
/// reference URL without a kind is an error instead of a guessed layout.
pub fn resolve_reference_connection(
    profile: Option<&ServiceConnectionProfile>,
    cli_kind: Option<ReferenceKind>,
    cli_base_url: Option<&str>,
    cli_basic_auth: Option<&BasicAuthConfig>,
) -> Result<Option<ReferenceConnection>> {
    let Some(connection) =
        resolve_optional_service_connection("Reference", profile, cli_base_url, cli_basic_auth)?
    else {
        return Ok(None);
    };
    let kind = cli_kind
        .or_else(|| profile.and_then(|profile| profile.kind))
        .ok_or_else(|| {
            anyhow!(
                "reference connection needs an engine kind: pass --reference-kind or set `kind` in the connection profile"
            )
        })?;
    Ok(Some(ReferenceConnection { kind, connection }))
}

pub fn merge_invocation_profiles(
    base: &ServiceInvocationProfiles,
    overlay: &ServiceInvocationProfiles,
) -> Result<ServiceInvocationProfiles> {
    Ok(ServiceInvocationProfiles {
        nrese: merge_profile_map("NRESE", &base.nrese, &overlay.nrese)?,
        reference: merge_profile_map("Reference", &base.reference, &overlay.reference)?,
    })
}

fn merge_profile_map(
    label: &str,
    base: &BTreeMap<String, ServiceRequestProfile>,
    overlay: &BTreeMap<String, ServiceRequestProfile>,
) -> Result<BTreeMap<String, ServiceRequestProfile>> {
    let collisions = overlay
        .keys()
        .filter(|key| base.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        bail!(
            "{label} invocation profiles collide between selected connection profile and workload pack: {}",
            collisions.join(", ")
        );
    }

    let mut merged = base.clone();
    merged.extend(overlay.clone());
    Ok(merged)
}

fn normalize_live_connection_profile(profile: &mut LiveConnectionProfile) -> Result<()> {
    normalize_service_profile(&mut profile.nrese)?;
    if let Some(reference) = profile.reference.as_mut() {
        normalize_service_profile(reference)?;
    }
    for request_profile in profile.invocation_profiles.nrese.values_mut() {
        expand_headers_env_placeholders(&mut request_profile.headers)?;
    }
    for request_profile in profile.invocation_profiles.reference.values_mut() {
        expand_headers_env_placeholders(&mut request_profile.headers)?;
    }
    Ok(())
}

fn normalize_service_profile(profile: &mut ServiceConnectionProfile) -> Result<()> {
    profile.base_url = expand_env_placeholders(&profile.base_url)?;
    expand_headers_env_placeholders(&mut profile.headers)?;
    if let Some(basic_auth) = profile.basic_auth.as_mut() {
        basic_auth.username = expand_env_placeholders(&basic_auth.username)?;
        basic_auth.password = expand_env_placeholders(&basic_auth.password)?;
    }
    Ok(())
}

fn to_runtime_basic_auth(input: &crate::model::BasicAuthFile) -> BasicAuthConfig {
    BasicAuthConfig {
        username: input.username.clone(),
        password: input.password.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::model::{BasicAuthConfig, ServiceInvocationProfiles, ServiceRequestProfile};

    use super::{
        merge_invocation_profiles, read_connection_profiles_registry,
        resolve_live_connection_profile, resolve_optional_service_connection,
        resolve_reference_connection, resolve_required_service_connection,
    };

    #[test]
    fn connection_profiles_registry_expands_env_placeholders() {
        let temp_dir = tempdir().expect("tempdir");
        let path = temp_dir.path().join("connection-profiles.toml");
        fs::write(
            &path,
            r#"
[profiles.secured-live.nrese]
base_url = "${NRESE_BASE_URL}"
timeout_ms = 15000

[profiles.secured-live.nrese.headers]
authorization = "Bearer ${NRESE_TOKEN}"

[profiles.secured-live.reference]
base_url = "${REFERENCE_BASE_URL}"

[profiles.secured-live.reference.basic_auth]
username = "${REFERENCE_USER}"
password = "${REFERENCE_PASS}"

[profiles.secured-live.invocation_profiles.nrese.invalid.headers]
authorization = "Bearer invalid-token"
"#,
        )
        .expect("registry");

        let previous = [
            ("NRESE_BASE_URL", std::env::var("NRESE_BASE_URL").ok()),
            ("NRESE_TOKEN", std::env::var("NRESE_TOKEN").ok()),
            (
                "REFERENCE_BASE_URL",
                std::env::var("REFERENCE_BASE_URL").ok(),
            ),
            ("REFERENCE_USER", std::env::var("REFERENCE_USER").ok()),
            ("REFERENCE_PASS", std::env::var("REFERENCE_PASS").ok()),
        ];
        unsafe {
            std::env::set_var("NRESE_BASE_URL", "http://127.0.0.1:8080");
            std::env::set_var("NRESE_TOKEN", "secret");
            std::env::set_var("REFERENCE_BASE_URL", "http://127.0.0.1:3030/ds");
            std::env::set_var("REFERENCE_USER", "admin");
            std::env::set_var("REFERENCE_PASS", "reference-admin");
        }
        let registry = read_connection_profiles_registry(&path).expect("registry");
        for (name, value) in previous {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }

        let profile =
            resolve_live_connection_profile(&registry, "secured-live").expect("selected profile");
        assert_eq!(profile.nrese.base_url, "http://127.0.0.1:8080");
        assert_eq!(
            profile
                .nrese
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("Bearer secret")
        );
        assert_eq!(
            profile
                .invocation_profiles
                .nrese
                .get("invalid")
                .and_then(|profile| profile.headers.get("authorization"))
                .map(String::as_str),
            Some("Bearer invalid-token")
        );
        assert_eq!(
            profile
                .reference
                .as_ref()
                .and_then(|profile| profile.basic_auth.as_ref())
                .map(|auth| auth.username.as_str()),
            Some("admin")
        );
    }

    #[test]
    fn cli_basic_auth_overrides_profile_basic_auth() {
        let temp_dir = tempdir().expect("tempdir");
        let path = temp_dir.path().join("connection-profiles.toml");
        fs::write(
            &path,
            r#"
[profiles.secured-live.nrese]
base_url = "http://127.0.0.1:8080"

[profiles.secured-live.reference]
base_url = "http://127.0.0.1:3030/ds"

[profiles.secured-live.reference.basic_auth]
username = "profile-user"
password = "profile-pass"
"#,
        )
        .expect("registry");

        let registry = read_connection_profiles_registry(&path).expect("registry");
        let profile =
            resolve_live_connection_profile(&registry, "secured-live").expect("selected profile");
        let config = resolve_optional_service_connection(
            "Reference",
            profile.reference.as_ref(),
            Some("http://127.0.0.1:3030/ds"),
            Some(&BasicAuthConfig {
                username: "override".to_owned(),
                password: "override-pass".to_owned(),
            }),
        )
        .expect("connection")
        .expect("reference");

        assert_eq!(
            config
                .basic_auth
                .as_ref()
                .map(|auth| auth.username.as_str()),
            Some("override")
        );
    }

    #[test]
    fn required_connection_reports_missing_config() {
        let error = resolve_required_service_connection("NRESE", None, None, None)
            .expect_err("missing connection");

        assert!(error.to_string().contains("--nrese-base-url"));
    }

    #[test]
    fn merged_invocation_profiles_reject_collisions() {
        let mut base = ServiceInvocationProfiles::default();
        base.nrese.insert(
            "read".to_owned(),
            ServiceRequestProfile {
                headers: [("authorization".to_owned(), "Bearer a".to_owned())]
                    .into_iter()
                    .collect(),
                timeout_ms: None,
            },
        );
        let mut overlay = ServiceInvocationProfiles::default();
        overlay.nrese.insert(
            "read".to_owned(),
            ServiceRequestProfile {
                headers: [("authorization".to_owned(), "Bearer b".to_owned())]
                    .into_iter()
                    .collect(),
                timeout_ms: None,
            },
        );

        let error = merge_invocation_profiles(&base, &overlay).expect_err("collision");
        assert!(error.to_string().contains("collide"));
    }

    #[test]
    fn reference_kind_comes_from_cli_or_profile_and_is_required() {
        let profile: crate::model::ServiceConnectionProfile = toml::from_str(
            r#"
kind = "graphdb"
base_url = "http://h:7200/repositories/r"
update_url = "http://h:7200/custom"
"#,
        )
        .expect("profile");

        let from_profile = resolve_reference_connection(Some(&profile), None, None, None)
            .expect("resolve")
            .expect("reference");
        assert_eq!(from_profile.kind, crate::model::ReferenceKind::Graphdb);
        assert_eq!(
            from_profile.connection.endpoints.update_url.as_deref(),
            Some("http://h:7200/custom")
        );

        let from_cli = resolve_reference_connection(
            Some(&profile),
            Some(crate::model::ReferenceKind::Qlever),
            None,
            None,
        )
        .expect("resolve")
        .expect("reference");
        assert_eq!(from_cli.kind, crate::model::ReferenceKind::Qlever);

        let error = resolve_reference_connection(None, None, Some("http://h:3030/ds"), None)
            .expect_err("kind required");
        assert!(error.to_string().contains("engine kind"));
    }
}
