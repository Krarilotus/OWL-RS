use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::interpolation::expand_headers_env_placeholders;
use crate::model::{OntologyCatalog, WorkloadPackManifest};

pub fn read_json<T: DeserializeOwned>(path: PathBuf) -> Result<T> {
    let text = fs::read_to_string(&path).with_context(|| format!("failed to read {:?}", path))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {:?}", path))
}

pub fn write_json_report<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("failed to serialize report json")?;
    fs::write(&path, json).with_context(|| format!("failed to write {:?}", path))
}

pub fn read_dataset_payload(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {:?}", path))
}

pub fn infer_rdf_content_type(path: &Path) -> Result<&'static str> {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        bail!("cannot infer RDF content type for {:?}", path);
    };

    match extension.to_ascii_lowercase().as_str() {
        "ttl" => Ok("text/turtle"),
        "nt" => Ok("application/n-triples"),
        "nq" => Ok("application/n-quads"),
        "trig" => Ok("application/trig"),
        "rdf" | "xml" => Ok("application/rdf+xml"),
        _ => bail!("unsupported RDF file extension: {extension}"),
    }
}

pub fn read_workload_pack(path: &Path) -> Result<WorkloadPackManifest> {
    let text = fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    let mut manifest: WorkloadPackManifest =
        toml::from_str(&text).with_context(|| format!("failed to parse {:?}", path))?;
    let base_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    manifest.dataset = resolve_pack_path(&base_dir, &manifest.dataset);
    manifest.query_workload = resolve_pack_path(&base_dir, &manifest.query_workload);
    manifest.update_workload = resolve_pack_path(&base_dir, &manifest.update_workload);
    if let Some(path) = manifest.compat_cases.take() {
        manifest.compat_suites.push(path);
    }
    for path in &mut manifest.compat_suites {
        *path = resolve_pack_path(&base_dir, path);
    }
    expand_headers_env_placeholders(&mut manifest.nrese.headers)?;
    expand_headers_env_placeholders(&mut manifest.fuseki.headers)?;

    Ok(manifest)
}

pub fn read_ontology_catalog(path: &Path) -> Result<OntologyCatalog> {
    let text = fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {:?}", path))
}

fn resolve_pack_path(base_dir: &Path, candidate: &Path) -> PathBuf {
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base_dir.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{infer_rdf_content_type, read_ontology_catalog, read_workload_pack};

    #[test]
    fn infers_turtle_content_type() {
        assert_eq!(
            infer_rdf_content_type(Path::new("dataset.ttl")).expect("content type"),
            "text/turtle"
        );
    }

    #[test]
    fn rejects_unknown_extension() {
        assert!(infer_rdf_content_type(Path::new("dataset.unknown")).is_err());
    }

    #[test]
    fn workload_pack_resolves_relative_fixture_paths() {
        let temp_dir = tempdir().expect("tempdir");
        let pack_dir = temp_dir.path().join("pack");
        fs::create_dir_all(&pack_dir).expect("pack dir");
        fs::write(
            pack_dir.join("pack.toml"),
            r#"
name = "example"
dataset = "../fixtures/seed.ttl"
query_workload = "../fixtures/query.json"
update_workload = "../fixtures/update.json"
compat_suites = ["../fixtures/compat.json"]

[nrese]
timeout_ms = 15000

[nrese.headers]
authorization = "Bearer local-token"

[fuseki.headers]
x-forwarded-proto = "https"
"#,
        )
        .expect("pack manifest");

        let manifest = read_workload_pack(&pack_dir.join("pack.toml")).expect("pack");
        assert!(
            manifest.dataset.ends_with("fixtures\\seed.ttl")
                || manifest.dataset.ends_with("fixtures/seed.ttl")
        );
        assert_eq!(manifest.name, "example");
        assert_eq!(manifest.compat_suites.len(), 1);
        assert_eq!(
            manifest
                .nrese
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("Bearer local-token")
        );
        assert_eq!(manifest.nrese.timeout_ms, Some(15000));
        assert_eq!(
            manifest
                .fuseki
                .headers
                .get("x-forwarded-proto")
                .map(String::as_str),
            Some("https")
        );
    }

    #[test]
    fn secured_timeout_pack_template_parses_with_multiple_compat_suites() {
        let previous_nrese = std::env::var("NRESE_COMPARE_READ_TOKEN").ok();
        let previous_fuseki = std::env::var("FUSEKI_COMPARE_READ_TOKEN").ok();
        unsafe {
            std::env::set_var("NRESE_COMPARE_READ_TOKEN", "nrese-token");
            std::env::set_var("FUSEKI_COMPARE_READ_TOKEN", "fuseki-token");
        }
        let manifest = read_workload_pack(Path::new(
            "fixtures/packs/secured-live-auth-timeout-template/pack.toml",
        ))
        .expect("pack");
        match previous_nrese {
            Some(value) => unsafe {
                std::env::set_var("NRESE_COMPARE_READ_TOKEN", value);
            },
            None => unsafe {
                std::env::remove_var("NRESE_COMPARE_READ_TOKEN");
            },
        }
        match previous_fuseki {
            Some(value) => unsafe {
                std::env::set_var("FUSEKI_COMPARE_READ_TOKEN", value);
            },
            None => unsafe {
                std::env::remove_var("FUSEKI_COMPARE_READ_TOKEN");
            },
        }

        assert_eq!(manifest.name, "secured-live-auth-timeout-template");
        assert_eq!(manifest.compat_suites.len(), 3);
        assert!(
            manifest.compat_suites.iter().any(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value == "policy_failure_cases.json")
            })
        );
        assert!(
            manifest.compat_suites.iter().any(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value == "timeout_failure_cases.json")
            })
        );
        assert_eq!(
            manifest
                .nrese
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("Bearer nrese-token")
        );
    }

    #[test]
    fn ontology_catalog_parses_entries() {
        let temp_dir = tempdir().expect("tempdir");
        let path = temp_dir.path().join("catalog.toml");
        fs::write(
            &path,
            r#"
[[ontologies]]
name = "prov"
title = "PROV-O"
url = "https://www.w3.org/ns/prov.ttl"
media_type = "text/turtle"
filename = "prov.ttl"
tier = "broad"
focus_terms = ["http://www.w3.org/ns/prov#Entity"]
"#,
        )
        .expect("catalog");

        let catalog = read_ontology_catalog(&path).expect("catalog");
        assert_eq!(catalog.ontologies.len(), 1);
        assert_eq!(catalog.ontologies[0].name, "prov");
    }
}
