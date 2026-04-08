use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Cli {
    pub command: Command,
}

#[derive(Debug, Clone)]
pub enum Command {
    Bench(BenchConfig),
    CatalogSync(CatalogSyncConfig),
    Compat(CompatConfig),
    Pack(PackConfig),
    ValidatePack(ValidatePackConfig),
    PackMatrix(PackMatrixConfig),
    Seed(SeedConfig),
}

#[derive(Debug, Clone)]
pub struct CatalogSyncConfig {
    pub catalog_path: PathBuf,
    pub output_dir: PathBuf,
    pub tier: Option<String>,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub nrese: ServiceConnectionConfig,
    pub fuseki: Option<ServiceConnectionConfig>,
    pub iterations: usize,
    pub query_workload_path: PathBuf,
    pub update_workload_path: PathBuf,
    pub report_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CompatConfig {
    pub nrese: ServiceConnectionConfig,
    pub fuseki: ServiceConnectionConfig,
    pub nrese_profiles: BTreeMap<String, ServiceRequestProfile>,
    pub fuseki_profiles: BTreeMap<String, ServiceRequestProfile>,
    pub cases_path: PathBuf,
    pub report_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SeedConfig {
    pub nrese: ServiceConnectionConfig,
    pub fuseki: Option<ServiceConnectionConfig>,
    pub dataset_path: PathBuf,
    pub dataset_base_iri: Option<String>,
    pub content_type: Option<String>,
    pub replace: bool,
}

#[derive(Debug, Clone)]
pub struct PackConfig {
    pub nrese_base_url: Option<String>,
    pub fuseki_base_url: Option<String>,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub connection_profiles_path: Option<PathBuf>,
    pub connection_profile_name: Option<String>,
    pub workload_pack_path: PathBuf,
    pub execution_mode: PackExecutionMode,
    pub iterations: usize,
    pub report_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ValidatePackConfig {
    pub nrese_base_url: Option<String>,
    pub fuseki_base_url: Option<String>,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub connection_profiles_path: Option<PathBuf>,
    pub connection_profile_name: Option<String>,
    pub workload_pack_path: PathBuf,
    pub report_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackExecutionMode {
    Full,
    CompatOnly,
}

#[derive(Debug, Clone)]
pub struct PackMatrixConfig {
    pub nrese_base_url: Option<String>,
    pub fuseki_base_url: Option<String>,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub connection_profiles_path: Option<PathBuf>,
    pub connection_profile_name: Option<String>,
    pub catalog_path: PathBuf,
    pub packs_dir: PathBuf,
    pub ontology_name: Option<String>,
    pub execution_mode: PackExecutionMode,
    pub tier: Option<String>,
    pub semantic_dialect: Option<OntologySemanticDialect>,
    pub reasoning_feature: Option<OntologyReasoningFeature>,
    pub service_coverage: Option<OntologyServiceSurface>,
    pub iterations: usize,
    pub report_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkloadPackManifest {
    pub name: String,
    pub dataset: PathBuf,
    #[serde(default)]
    pub dataset_base_iri: Option<String>,
    pub query_workload: PathBuf,
    pub update_workload: PathBuf,
    #[serde(default)]
    pub compat_suites: Vec<PathBuf>,
    #[serde(default)]
    pub nrese: ServiceRequestProfile,
    #[serde(default)]
    pub fuseki: ServiceRequestProfile,
    #[serde(default)]
    pub invocation_profiles: ServiceInvocationProfiles,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OntologyCatalog {
    pub ontologies: Vec<OntologyFixture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OntologyFixture {
    pub name: String,
    pub title: String,
    pub url: String,
    pub media_type: String,
    pub serialization: OntologySerialization,
    pub filename: String,
    pub tier: String,
    #[serde(default)]
    pub focus_terms: Vec<String>,
    #[serde(default)]
    pub semantic_dialects: Vec<OntologySemanticDialect>,
    #[serde(default)]
    pub reasoning_features: Vec<OntologyReasoningFeature>,
    #[serde(default)]
    pub service_coverage: Vec<OntologyServiceSurface>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OntologySerialization {
    Turtle,
    RdfXml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OntologySemanticDialect {
    Rdfs,
    Owl,
    Foaf,
    Org,
    Time,
    ProvO,
    Skos,
    Sosa,
    Ssn,
    Dcat,
    Vcard,
    Odrl,
    DcmiTerms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OntologyReasoningFeature {
    SubclassClosure,
    SubpropertyClosure,
    DomainRangeTyping,
    InverseProperty,
    TransitiveProperty,
    SymmetricProperty,
    Disjointness,
    Identity,
    Restrictions,
    ListAxioms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OntologyServiceSurface {
    CatalogSync,
    Compat,
    Tell,
    GraphStore,
    Query,
    Reasoner,
    Benchmark,
}

impl OntologyCatalog {
    pub fn validate(&self) -> Result<(), String> {
        for fixture in &self.ontologies {
            fixture.validate()?;
        }
        Ok(())
    }
}

impl OntologyFixture {
    pub fn validate(&self) -> Result<(), String> {
        if self.focus_terms.is_empty() {
            return Err(format!(
                "ontology fixture '{}' must declare at least one focus_terms entry",
                self.name
            ));
        }
        if self.semantic_dialects.is_empty() {
            return Err(format!(
                "ontology fixture '{}' must declare semantic_dialects",
                self.name
            ));
        }
        if self.reasoning_features.is_empty() {
            return Err(format!(
                "ontology fixture '{}' must declare reasoning_features",
                self.name
            ));
        }
        if self.service_coverage.is_empty() {
            return Err(format!(
                "ontology fixture '{}' must declare service_coverage",
                self.name
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ServiceRequestProfile {
    #[serde(default)]
    pub headers: CompatHeaders,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ServiceInvocationProfiles {
    #[serde(default)]
    pub nrese: BTreeMap<String, ServiceRequestProfile>,
    #[serde(default)]
    pub fuseki: BTreeMap<String, ServiceRequestProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ServiceConnectionProfile {
    pub base_url: String,
    #[serde(default)]
    pub headers: CompatHeaders,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub basic_auth: Option<BasicAuthFile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BasicAuthFile {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfilesRegistry {
    pub profiles: BTreeMap<String, LiveConnectionProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LiveConnectionProfile {
    pub nrese: ServiceConnectionProfile,
    #[serde(default)]
    pub fuseki: Option<ServiceConnectionProfile>,
    #[serde(default)]
    pub invocation_profiles: ServiceInvocationProfiles,
}

#[derive(Debug, Clone)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct ServiceConnectionConfig {
    pub base_url: String,
    pub headers: CompatHeaders,
    pub timeout_ms: Option<u64>,
    pub basic_auth: Option<BasicAuthConfig>,
}

pub type CompatHeaders = BTreeMap<String, String>;

#[derive(Debug, Clone, Deserialize)]
pub struct QueryWorkloadCase {
    pub name: String,
    pub query: String,
    #[serde(default = "default_query_accept")]
    pub accept: String,
    #[serde(default = "default_weight")]
    pub weight: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateWorkloadCase {
    pub name: String,
    pub update: String,
    #[serde(default = "default_weight")]
    pub weight: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompatCase {
    pub name: String,
    #[serde(default)]
    pub operation: CompatOperation,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_query_accept")]
    pub accept: String,
    #[serde(default)]
    pub update: Option<String>,
    #[serde(default)]
    pub verify_query: Option<String>,
    #[serde(default)]
    pub graph_target: Option<CompatGraphTarget>,
    #[serde(default)]
    pub graph_payload: Option<String>,
    #[serde(default)]
    pub graph_content_type: Option<String>,
    #[serde(default = "default_true")]
    pub graph_replace: bool,
    #[serde(default)]
    pub generated_payload: Option<GeneratedPayloadSpec>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub request_headers: CompatHeaders,
    #[serde(default)]
    pub nrese_profile: Option<String>,
    #[serde(default)]
    pub fuseki_profile: Option<String>,
    pub kind: CompatKind,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratedPayloadSpec {
    pub kind: GeneratedPayloadKind,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeneratedPayloadKind {
    SparqlQuery,
    SparqlUpdate,
    RdfTurtle,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatOperation {
    #[default]
    Query,
    GraphRead,
    GraphHead,
    GraphDeleteEffect,
    GraphPutEffect,
    GraphPostEffect,
    UpdateEffect,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CompatGraphTarget {
    DefaultGraph,
    NamedGraph { iri: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatKind {
    AskBoolean,
    SolutionsCount,
    SolutionsBindingsSet,
    ConstructTriplesSet,
    GraphTriplesSet,
    StatusAndContentType,
    StatusContentTypeBodyClass,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencySummary {
    pub samples: usize,
    pub success: usize,
    pub failure: usize,
    pub min_ms: u128,
    pub p50_ms: u128,
    pub p95_ms: u128,
    pub p99_ms: u128,
    pub max_ms: u128,
    pub total_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct BenchReport {
    pub mode: &'static str,
    pub iterations: usize,
    pub services: Vec<ServiceBenchReport>,
    pub comparison: Option<BenchComparison>,
}

#[derive(Debug, Serialize)]
pub struct ServiceBenchReport {
    pub label: &'static str,
    pub base_url: String,
    pub query: LatencySummary,
    pub update: LatencySummary,
}

#[derive(Debug, Serialize)]
pub struct BenchComparison {
    pub left_label: &'static str,
    pub right_label: &'static str,
    pub query_p95_delta_ms: i128,
    pub update_p95_delta_ms: i128,
}

#[derive(Debug, Serialize)]
pub struct CompatReport {
    pub mode: &'static str,
    pub nrese_base_url: String,
    pub fuseki_base_url: String,
    pub total_cases: usize,
    pub matched_cases: usize,
    pub mismatched_cases: usize,
    pub status: &'static str,
    pub cases: Vec<CompatCaseReport>,
}

#[derive(Debug, Serialize)]
pub struct PackReport {
    pub mode: &'static str,
    pub pack_name: String,
    pub manifest_path: String,
    pub connection_profiles_path: Option<String>,
    pub connection_profile_name: Option<String>,
    pub execution_mode: PackExecutionMode,
    pub dataset_path: String,
    pub dataset_base_iri: Option<String>,
    pub nrese_base_url: String,
    pub fuseki_base_url: Option<String>,
    pub iterations: usize,
    pub status: &'static str,
    pub error: Option<String>,
    pub compat_suites: Vec<PackCompatSuiteReport>,
    pub bench_report: Option<PackArtifactReport>,
}

#[derive(Debug, Serialize)]
pub struct PackValidationReport {
    pub mode: &'static str,
    pub pack_name: String,
    pub manifest_path: String,
    pub connection_profiles_path: Option<String>,
    pub connection_profile_name: Option<String>,
    pub dataset_path: String,
    pub dataset_base_iri: Option<String>,
    pub nrese_base_url: String,
    pub fuseki_base_url: Option<String>,
    pub compat_suites: Vec<String>,
    pub nrese_invocation_profiles: Vec<String>,
    pub fuseki_invocation_profiles: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PackMatrixReport {
    pub mode: &'static str,
    pub catalog_path: String,
    pub packs_dir: String,
    pub connection_profiles_path: Option<String>,
    pub connection_profile_name: Option<String>,
    pub ontology_name: Option<String>,
    pub execution_mode: PackExecutionMode,
    pub tier: Option<String>,
    pub semantic_dialect: Option<OntologySemanticDialect>,
    pub reasoning_feature: Option<OntologyReasoningFeature>,
    pub service_coverage: Option<OntologyServiceSurface>,
    pub pack_runs: Vec<PackMatrixEntryReport>,
}

#[derive(Debug, Serialize)]
pub struct PackMatrixEntryReport {
    pub ontology_name: String,
    pub ontology_title: String,
    pub ontology_tier: String,
    pub manifest_path: String,
    pub semantic_dialects: Vec<OntologySemanticDialect>,
    pub reasoning_features: Vec<OntologyReasoningFeature>,
    pub service_coverage: Vec<OntologyServiceSurface>,
    pub status: &'static str,
    pub error: Option<String>,
    pub pack_report: Option<PackArtifactReport>,
}

#[derive(Debug, Serialize)]
pub struct PackCompatSuiteReport {
    pub suite_name: String,
    pub suite_path: String,
    pub report: Option<PackArtifactReport>,
    pub total_cases: usize,
    pub matched_cases: usize,
    pub mismatched_cases: usize,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PackArtifactReport {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct CompatCaseReport {
    pub name: String,
    pub operation: &'static str,
    pub kind: &'static str,
    pub left_status: u16,
    pub right_status: u16,
    pub left_content_type: Option<String>,
    pub right_content_type: Option<String>,
    pub left_body_class: Option<String>,
    pub right_body_class: Option<String>,
    pub matched: bool,
    pub left_summary: String,
    pub right_summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_result_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_result_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_only_sample: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub right_only_sample: Vec<String>,
}

pub fn default_query_accept() -> String {
    "application/sparql-results+json".to_owned()
}

pub fn default_weight() -> usize {
    1
}

pub fn default_true() -> bool {
    true
}

pub fn compat_kind_label(kind: CompatKind) -> &'static str {
    match kind {
        CompatKind::AskBoolean => "ask-boolean",
        CompatKind::SolutionsCount => "solutions-count",
        CompatKind::SolutionsBindingsSet => "solutions-bindings-set",
        CompatKind::ConstructTriplesSet => "construct-triples-set",
        CompatKind::GraphTriplesSet => "graph-triples-set",
        CompatKind::StatusAndContentType => "status-and-content-type",
        CompatKind::StatusContentTypeBodyClass => "status-content-type-body-class",
    }
}

pub fn compat_operation_label(operation: CompatOperation) -> &'static str {
    match operation {
        CompatOperation::Query => "query",
        CompatOperation::GraphRead => "graph-read",
        CompatOperation::GraphHead => "graph-head",
        CompatOperation::GraphDeleteEffect => "graph-delete-effect",
        CompatOperation::GraphPutEffect => "graph-put-effect",
        CompatOperation::GraphPostEffect => "graph-post-effect",
        CompatOperation::UpdateEffect => "update-effect",
    }
}
