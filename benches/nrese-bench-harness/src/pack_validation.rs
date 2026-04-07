use std::path::Path;

use anyhow::{Result, bail};

use crate::model::{OntologyFixture, OntologySerialization, WorkloadPackManifest};

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::io::{read_ontology_catalog, read_workload_pack};
    use crate::model::{
        OntologyFixture, OntologyReasoningFeature, OntologySemanticDialect,
        OntologySerialization, OntologyServiceSurface, WorkloadPackManifest,
    };

    use super::validate_catalog_baseline_pack;

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
        let catalog = read_ontology_catalog(Path::new(
            "benches/nrese-bench-harness/fixtures/catalog/ontologies.toml",
        ))
        .expect("catalog");

        for ontology in &catalog.ontologies {
            let manifest_path = Path::new("benches/nrese-bench-harness/fixtures/packs")
                .join(format!("{}-baseline", ontology.name))
                .join("pack.toml");
            let manifest = read_workload_pack(&manifest_path).expect("pack");
            validate_catalog_baseline_pack(ontology, &manifest_path, &manifest)
                .expect("catalog baseline pack");
        }
    }
}
