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
    pub nrese_base_url: String,
    pub nrese_headers: CompatHeaders,
    pub fuseki_base_url: Option<String>,
    pub fuseki_headers: CompatHeaders,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub iterations: usize,
    pub query_workload_path: PathBuf,
    pub update_workload_path: PathBuf,
    pub report_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CompatConfig {
    pub nrese_base_url: String,
    pub nrese_headers: CompatHeaders,
    pub fuseki_base_url: String,
    pub fuseki_headers: CompatHeaders,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub cases_path: PathBuf,
    pub report_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SeedConfig {
    pub nrese_base_url: String,
    pub nrese_headers: CompatHeaders,
    pub fuseki_base_url: Option<String>,
    pub fuseki_headers: CompatHeaders,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub dataset_path: PathBuf,
    pub content_type: Option<String>,
    pub replace: bool,
}

#[derive(Debug, Clone)]
pub struct PackConfig {
    pub nrese_base_url: String,
    pub fuseki_base_url: Option<String>,
    pub fuseki_basic_auth: Option<BasicAuthConfig>,
    pub workload_pack_path: PathBuf,
    pub iterations: usize,
    pub report_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkloadPackManifest {
    pub name: String,
    pub dataset: PathBuf,
    pub query_workload: PathBuf,
    pub update_workload: PathBuf,
    #[serde(default)]
    pub compat_cases: Option<PathBuf>,
    #[serde(default)]
    pub compat_suites: Vec<PathBuf>,
    #[serde(default)]
    pub nrese: ServiceRequestProfile,
    #[serde(default)]
    pub fuseki: ServiceRequestProfile,
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
    pub filename: String,
    pub tier: String,
    #[serde(default)]
    pub focus_terms: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServiceRequestProfile {
    #[serde(default)]
    pub headers: CompatHeaders,
}

#[derive(Debug, Clone)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
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
    pub request_headers: CompatHeaders,
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
