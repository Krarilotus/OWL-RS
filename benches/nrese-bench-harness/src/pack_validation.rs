use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::io::read_json;
use crate::model::{
    CompatCase, OntologyFixture, OntologySerialization, ServiceInvocationProfiles,
    WorkloadPackManifest,
};

pub fn validate_catalog_baseline_pack(
    ontology: &OntologyFixture,
    manifest_path: &Path,
    manifest: &WorkloadPackManifest,
) -> Result<()> {
    let expected_pack_name = format!("{}-baseline", ontology.name);
    if manifest.name != expected_pack_name {
        bail!(
            "catalog baseline pack {} must use name '{}' but found '{}'",
            manifest_path.display(),
            expected_pack_name,
            manifest.name
        );
    }

    let dataset_filename = manifest
        .dataset
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "catalog baseline pack {} has dataset path without filename",
                manifest_path.display()
            )
        })?;
    if dataset_filename != ontology.filename {
        bail!(
            "catalog baseline pack {} must target dataset '{}' but found '{}'",
            manifest_path.display(),
            ontology.filename,
            dataset_filename
        );
    }

    let suite_filenames = manifest
        .compat_suites
        .iter()
        .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
        .collect::<Vec<_>>();

    require_suite(
        &suite_filenames,
        "baseline_cases.json",
        manifest_path,
        ontology,
    )?;
    require_suite(
        &suite_filenames,
        &format!("{}_cases.json", ontology.name),
        manifest_path,
        ontology,
    )?;

    if matches!(ontology.serialization, OntologySerialization::RdfXml) {
        require_suite(
            &suite_filenames,
            "rdf_xml_cases.json",
            manifest_path,
            ontology,
        )?;
    }

    Ok(())
}

pub fn validate_pack_invocation_profiles(
    suite_paths: &[PathBuf],
    invocation_profiles: &ServiceInvocationProfiles,
) -> Result<()> {
    for suite_path in suite_paths {
        let cases: Vec<CompatCase> = read_json(suite_path.clone())?;
        for case in &cases {
            require_profile(
                &invocation_profiles.nrese,
                case.nrese_profile.as_deref(),
                "NRESE",
                &case.name,
                suite_path,
            )?;
            require_profile(
                &invocation_profiles.fuseki,
                case.fuseki_profile.as_deref(),
                "Fuseki",
                &case.name,
                suite_path,
            )?;
        }
    }

    Ok(())
}

fn require_suite(
    suite_filenames: &[&str],
    required_suite: &str,
    manifest_path: &Path,
    ontology: &OntologyFixture,
) -> Result<()> {
    if suite_filenames.contains(&required_suite) {
        return Ok(());
    }

    bail!(
        "catalog baseline pack {} for ontology '{}' must include compat suite '{}'",
        manifest_path.display(),
        ontology.name,
        required_suite
    )
}

fn require_profile(
    profiles: &std::collections::BTreeMap<String, crate::model::ServiceRequestProfile>,
    profile_name: Option<&str>,
    target_label: &str,
    case_name: &str,
    suite_path: &Path,
) -> Result<()> {
    let Some(profile_name) = profile_name else {
        return Ok(());
    };

    if profiles.contains_key(profile_name) {
        return Ok(());
    }

    bail!(
        "{target_label} invocation profile '{profile_name}' referenced by case '{case_name}' in suite {} is not defined by the selected live connection profile or workload pack",
        suite_path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use crate::model::{
        CompatHeaders, OntologyFixture, OntologyReasoningFeature, OntologySemanticDialect,
        OntologySerialization, OntologyServiceSurface, ServiceInvocationProfiles,
        ServiceRequestProfile, WorkloadPackManifest,
    };
    use crate::io::{read_ontology_catalog, read_workload_pack};

    use super::{validate_catalog_baseline_pack, validate_pack_invocation_profiles};

    fn ontology_fixture(name: &str, serialization: OntologySerialization, filename: &str) -> OntologyFixture {
        OntologyFixture {
            name: name.to_owned(),
            title: name.to_owned(),
            url: format!("https://example.com/{filename}"),
            media_type: "text/turtle".to_owned(),
            serialization,
            filename: filename.to_owned(),
            tier: "medium".to_owned(),
            focus_terms: vec![format!("http://example.com/{name}#Thing")],
            semantic_dialects: vec![OntologySemanticDialect::Owl],
            reasoning_features: vec![OntologyReasoningFeature::SubclassClosure],
            service_coverage: vec![OntologyServiceSurface::Benchmark],
        }
    }

    #[test]
    fn accepts_expected_catalog_baseline_pack_shape() {
        let ontology = ontology_fixture("foaf", OntologySerialization::RdfXml, "foaf.rdf");
        let manifest = WorkloadPackManifest {
            name: "foaf-baseline".to_owned(),
            dataset: "fixtures/catalog-cache/foaf.rdf".into(),
            dataset_base_iri: None,
            query_workload: "fixtures/workloads/query.json".into(),
            update_workload: "fixtures/workloads/update.json".into(),
            compat_suites: vec![
                "fixtures/compat/ontologies/baseline_cases.json".into(),
                "fixtures/compat/ontologies/foaf_cases.json".into(),
                "fixtures/compat/ontologies/rdf_xml_cases.json".into(),
            ],
            nrese: Default::default(),
            fuseki: Default::default(),
            invocation_profiles: Default::default(),
        };

        validate_catalog_baseline_pack(&ontology, Path::new("fixtures/packs/foaf-baseline/pack.toml"), &manifest)
            .expect("valid pack");
    }

    #[test]
    fn rejects_missing_ontology_specific_suite() {
        let ontology = ontology_fixture("sosa", OntologySerialization::Turtle, "sosa.ttl");
        let manifest = WorkloadPackManifest {
            name: "sosa-baseline".to_owned(),
            dataset: "fixtures/catalog-cache/sosa.ttl".into(),
            dataset_base_iri: None,
            query_workload: "fixtures/workloads/query.json".into(),
            update_workload: "fixtures/workloads/update.json".into(),
            compat_suites: vec!["fixtures/compat/ontologies/baseline_cases.json".into()],
            nrese: Default::default(),
            fuseki: Default::default(),
            invocation_profiles: Default::default(),
        };

        let error = validate_catalog_baseline_pack(
            &ontology,
            Path::new("fixtures/packs/sosa-baseline/pack.toml"),
            &manifest,
        )
        .expect_err("missing ontology suite");

        assert!(error.to_string().contains("sosa_cases.json"));
    }

    #[test]
    fn rejects_rdf_xml_pack_without_rdf_xml_suite() {
        let ontology = ontology_fixture("skos", OntologySerialization::RdfXml, "skos.rdf");
        let manifest = WorkloadPackManifest {
            name: "skos-baseline".to_owned(),
            dataset: "fixtures/catalog-cache/skos.rdf".into(),
            dataset_base_iri: None,
            query_workload: "fixtures/workloads/query.json".into(),
            update_workload: "fixtures/workloads/update.json".into(),
            compat_suites: vec![
                "fixtures/compat/ontologies/baseline_cases.json".into(),
                "fixtures/compat/ontologies/skos_cases.json".into(),
            ],
            nrese: Default::default(),
            fuseki: Default::default(),
            invocation_profiles: Default::default(),
        };

        let error = validate_catalog_baseline_pack(
            &ontology,
            Path::new("fixtures/packs/skos-baseline/pack.toml"),
            &manifest,
        )
        .expect_err("missing rdf xml suite");

        assert!(error.to_string().contains("rdf_xml_cases.json"));
    }

    #[test]
    fn checked_in_catalog_baseline_packs_validate_against_catalog_metadata() {
        let catalog = read_ontology_catalog(Path::new("fixtures/catalog/ontologies.toml"))
        .expect("catalog");

        for ontology in &catalog.ontologies {
            let manifest_path = Path::new("fixtures/packs")
                .join(format!("{}-baseline", ontology.name))
                .join("pack.toml");
            let manifest = read_workload_pack(&manifest_path).expect("pack");
            validate_catalog_baseline_pack(ontology, &manifest_path, &manifest)
                .expect("catalog baseline pack");
        }
    }

    #[test]
    fn pack_invocation_profiles_accept_defined_profile_references() {
        let temp_dir = tempdir().expect("tempdir");
        let suite_path = temp_dir.path().join("secured_auth_failure_cases.json");
        std::fs::write(
            &suite_path,
            r#"[
  {
    "name": "query-invalid-auth-profile",
    "operation": "query",
    "query": "SELECT * WHERE { ?s ?p ?o } LIMIT 1",
    "nrese_profile": "invalid",
    "fuseki_profile": "invalid",
    "kind": "status-content-type-body-class"
  }
]"#,
        )
        .expect("suite");

        let mut profiles = ServiceInvocationProfiles::default();
        let invalid = ServiceRequestProfile {
            headers: CompatHeaders::from([("authorization".to_owned(), "Bearer invalid".to_owned())]),
            timeout_ms: None,
        };
        profiles.nrese.insert("invalid".to_owned(), invalid.clone());
        profiles.fuseki.insert("invalid".to_owned(), invalid);

        validate_pack_invocation_profiles(&[suite_path], &profiles).expect("valid profiles");
    }

    #[test]
    fn pack_invocation_profiles_reject_missing_profile_reference() {
        let temp_dir = tempdir().expect("tempdir");
        let suite_path = temp_dir.path().join("secured_auth_failure_cases.json");
        std::fs::write(
            &suite_path,
            r#"[
  {
    "name": "query-invalid-auth-profile",
    "operation": "query",
    "query": "SELECT * WHERE { ?s ?p ?o } LIMIT 1",
    "nrese_profile": "invalid",
    "kind": "status-content-type-body-class"
  }
]"#,
        )
        .expect("suite");

        let error = validate_pack_invocation_profiles(std::slice::from_ref(&suite_path), &ServiceInvocationProfiles::default())
            .expect_err("missing profile");

        assert!(error.to_string().contains("selected live connection profile or workload pack"));
        assert!(error.to_string().contains("invalid"));
        assert!(error.to_string().contains("query-invalid-auth-profile"));
        assert!(error.to_string().contains(&suite_path.display().to_string()));
    }
}
