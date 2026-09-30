use std::path::PathBuf;

use crate::error::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreMode {
    #[default]
    InMemory,
    OnDisk,
}

/// Typed store configuration. Parsing from env/files lives in `nrese-server/src/config/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub mode: StoreMode,
    pub data_dir: PathBuf,
    /// Ontology file loaded at startup. `None` means no preload; there are no implicit
    /// fallback locations.
    pub ontology_path: Option<PathBuf>,
    /// Bytes of serialised query results kept for repeated queries on an unchanged store
    /// (see `query_cache`); 0 disables the cache.
    pub query_cache_bytes: usize,
    /// The graph that holds the repository's SHACL shapes.
    pub shapes_graph: String,
}

/// Default query result cache: 64 MiB.
pub const DEFAULT_QUERY_CACHE_BYTES: usize = 64 << 20;

/// The default shapes graph: the one RDF4J and GraphDB use, so their clients and
/// documentation apply unchanged.
pub const DEFAULT_SHAPES_GRAPH: &str = "http://rdf4j.org/schema/rdf4j#SHACLShapeGraph";

impl Default for StoreConfig {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl StoreConfig {
    pub fn in_memory() -> Self {
        Self {
            mode: StoreMode::InMemory,
            data_dir: PathBuf::from("./data"),
            ontology_path: None,
            query_cache_bytes: DEFAULT_QUERY_CACHE_BYTES,
            shapes_graph: DEFAULT_SHAPES_GRAPH.to_owned(),
        }
    }

    pub fn on_disk(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            mode: StoreMode::OnDisk,
            data_dir: data_dir.into(),
            ontology_path: None,
            query_cache_bytes: DEFAULT_QUERY_CACHE_BYTES,
            shapes_graph: DEFAULT_SHAPES_GRAPH.to_owned(),
        }
    }

    #[must_use]
    pub fn with_ontology(mut self, path: impl Into<PathBuf>) -> Self {
        self.ontology_path = Some(path.into());
        self
    }

    pub fn validate(&self) -> StoreResult<()> {
        if matches!(self.mode, StoreMode::OnDisk) && self.data_dir.as_os_str().is_empty() {
            return Err(StoreError::Configuration(
                "data_dir must not be empty in on-disk mode".to_owned(),
            ));
        }
        if oxrdf::NamedNode::new(self.shapes_graph.as_str()).is_err() {
            return Err(StoreError::Configuration(format!(
                "shapes_graph must be an IRI, not '{}'",
                self.shapes_graph
            )));
        }
        if matches!(self.ontology_path.as_ref(), Some(path) if path.as_os_str().is_empty()) {
            return Err(StoreError::Configuration(
                "ontology_path must not be empty if set".to_owned(),
            ));
        }
        Ok(())
    }
}
