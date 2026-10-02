//! Endpoint layouts of the engines the harness talks to.
//!
//! NRESE is always the system under test. The *reference* side is any engine we compare
//! against (ADR-0004: QLever and GraphDB are the parity targets; Fuseki remains a
//! correctness reference). Each engine exposes query, update and Graph Store endpoints at
//! different paths; this module is the single place that knows them. Explicit URLs in the
//! connection config override the layout for engine versions that deviate.

use serde::Deserialize;

use crate::model::{BasicAuthConfig, CompatHeaders, ServiceConnectionConfig};

/// Which engine a reference endpoint runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceKind {
    /// Apache Jena Fuseki dataset URL, e.g. `http://host:3030/ds`.
    Fuseki,
    /// GraphDB / RDF4J repository URL, e.g. `http://host:7200/repositories/repo`.
    Graphdb,
    /// QLever server URL, e.g. `http://host:7001`.
    Qlever,
}

impl ReferenceKind {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "fuseki" => Ok(Self::Fuseki),
            "graphdb" | "rdf4j" => Ok(Self::Graphdb),
            "qlever" => Ok(Self::Qlever),
            other => Err(format!(
                "unsupported reference kind '{other}' (expected fuseki, graphdb or qlever)"
            )),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fuseki => "Fuseki",
            Self::Graphdb => "GraphDB",
            Self::Qlever => "QLever",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointLayout {
    Nrese,
    Reference(ReferenceKind),
}

/// Optional explicit endpoint URLs; each one replaces the layout-derived URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EndpointOverrides {
    #[serde(default)]
    pub query_url: Option<String>,
    #[serde(default)]
    pub update_url: Option<String>,
    /// Graph Store endpoint without graph selector; `?default` / `?graph=` are appended.
    #[serde(default)]
    pub data_url: Option<String>,
}

pub struct ServiceTarget {
    pub label: &'static str,
    pub base_url: String,
    pub layout: EndpointLayout,
    pub overrides: EndpointOverrides,
    pub basic_auth: Option<BasicAuthConfig>,
    pub default_headers: CompatHeaders,
    pub default_timeout_ms: Option<u64>,
}

impl ServiceTarget {
    pub fn nrese(config: ServiceConnectionConfig) -> Self {
        Self::with_layout("NRESE", EndpointLayout::Nrese, config)
    }

    pub fn reference(kind: ReferenceKind, config: ServiceConnectionConfig) -> Self {
        Self::with_layout(kind.label(), EndpointLayout::Reference(kind), config)
    }

    fn with_layout(
        label: &'static str,
        layout: EndpointLayout,
        config: ServiceConnectionConfig,
    ) -> Self {
        Self {
            label,
            base_url: config.base_url,
            layout,
            overrides: config.endpoints,
            basic_auth: config.basic_auth,
            default_headers: config.headers,
            default_timeout_ms: config.timeout_ms,
        }
    }

    pub fn query_endpoint(&self) -> String {
        self.overrides
            .query_url
            .clone()
            .unwrap_or_else(|| self.layout.query_endpoint(&self.base_url))
    }

    pub fn update_endpoint(&self) -> String {
        self.overrides
            .update_url
            .clone()
            .unwrap_or_else(|| self.layout.update_endpoint(&self.base_url))
    }

    /// Graph Store endpoint addressing the default graph.
    pub fn data_endpoint(&self) -> String {
        format!("{}?default", self.data_endpoint_base())
    }

    /// Graph Store endpoint without a graph selector.
    pub fn data_endpoint_base(&self) -> String {
        self.overrides
            .data_url
            .clone()
            .unwrap_or_else(|| self.layout.data_endpoint_base(&self.base_url))
    }
}

impl EndpointLayout {
    pub fn query_endpoint(self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/query"),
            Self::Reference(ReferenceKind::Fuseki) => join_url(base_url, "/query"),
            Self::Reference(ReferenceKind::Graphdb | ReferenceKind::Qlever) => {
                base_url.trim_end_matches('/').to_owned()
            }
        }
    }

    pub fn update_endpoint(self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/update"),
            Self::Reference(ReferenceKind::Fuseki) => join_url(base_url, "/update"),
            Self::Reference(ReferenceKind::Graphdb) => join_url(base_url, "/statements"),
            Self::Reference(ReferenceKind::Qlever) => base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// QLever serves the Graph Store Protocol on its root path in recent versions; override
    /// `data_url` if the deployed version differs.
    pub fn data_endpoint_base(self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/data"),
            Self::Reference(ReferenceKind::Fuseki) => join_url(base_url, "/data"),
            Self::Reference(ReferenceKind::Graphdb) => join_url(base_url, "/rdf-graphs/service"),
            Self::Reference(ReferenceKind::Qlever) => join_url(base_url, "/"),
        }
    }
}

fn join_url(base_url: &str, suffix: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), suffix)
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatHeaders, ServiceConnectionConfig};

    use super::{EndpointOverrides, ReferenceKind, ServiceTarget};

    fn config(base_url: &str) -> ServiceConnectionConfig {
        ServiceConnectionConfig {
            base_url: base_url.to_owned(),
            headers: CompatHeaders::new(),
            timeout_ms: Some(25),
            basic_auth: None,
            endpoints: EndpointOverrides::default(),
        }
    }

    #[test]
    fn nrese_layout_builds_dataset_endpoints() {
        let target = ServiceTarget::nrese(config("http://127.0.0.1:8080/"));
        assert_eq!(
            target.query_endpoint(),
            "http://127.0.0.1:8080/dataset/query"
        );
        assert_eq!(
            target.update_endpoint(),
            "http://127.0.0.1:8080/dataset/update"
        );
        assert_eq!(
            target.data_endpoint(),
            "http://127.0.0.1:8080/dataset/data?default"
        );
        assert_eq!(target.default_timeout_ms, Some(25));
    }

    #[test]
    fn reference_layouts_follow_each_engine() {
        let fuseki = ServiceTarget::reference(ReferenceKind::Fuseki, config("http://h:3030/ds"));
        assert_eq!(fuseki.query_endpoint(), "http://h:3030/ds/query");
        assert_eq!(fuseki.data_endpoint(), "http://h:3030/ds/data?default");

        let graphdb = ServiceTarget::reference(
            ReferenceKind::Graphdb,
            config("http://h:7200/repositories/r"),
        );
        assert_eq!(graphdb.query_endpoint(), "http://h:7200/repositories/r");
        assert_eq!(
            graphdb.update_endpoint(),
            "http://h:7200/repositories/r/statements"
        );
        assert_eq!(
            graphdb.data_endpoint(),
            "http://h:7200/repositories/r/rdf-graphs/service?default"
        );
        assert_eq!(graphdb.label, "GraphDB");

        let qlever = ServiceTarget::reference(ReferenceKind::Qlever, config("http://h:7001/"));
        assert_eq!(qlever.query_endpoint(), "http://h:7001");
        assert_eq!(qlever.update_endpoint(), "http://h:7001");
    }

    #[test]
    fn explicit_endpoints_override_the_layout() {
        let mut overridden = config("http://h:7001");
        overridden.endpoints.data_url = Some("http://h:7001/gsp".to_owned());
        let qlever = ServiceTarget::reference(ReferenceKind::Qlever, overridden);
        assert_eq!(qlever.data_endpoint(), "http://h:7001/gsp?default");
    }

    #[test]
    fn reference_kind_parsing_is_strict() {
        assert_eq!(
            ReferenceKind::parse("GraphDB").unwrap(),
            ReferenceKind::Graphdb
        );
        assert!(ReferenceKind::parse("blazegraph").is_err());
    }
}
