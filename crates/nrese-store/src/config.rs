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
}

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
        }
    }

    pub fn on_disk(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            mode: StoreMode::OnDisk,
            data_dir: data_dir.into(),
            ontology_path: None,
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
        if matches!(self.ontology_path.as_ref(), Some(path) if path.as_os_str().is_empty()) {
            return Err(StoreError::Configuration(
                "ontology_path must not be empty if set".to_owned(),
            ));
        }
        Ok(())
    }
}
