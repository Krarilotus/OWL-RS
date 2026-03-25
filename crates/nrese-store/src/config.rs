use std::path::PathBuf;

use crate::error::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreMode {
    #[default]
    InMemory,
    OnDisk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub mode: StoreMode,
    pub data_dir: PathBuf,
    pub preload_ontology: bool,
    pub ontology_path: Option<PathBuf>,
    pub ontology_fallbacks: Vec<PathBuf>,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            mode: StoreMode::InMemory,
            data_dir: PathBuf::from("./data"),
            preload_ontology: false,
            ontology_path: None,
            ontology_fallbacks: vec![
                PathBuf::from("../Ontology-Development/files/processed/rg_ontology.ttl"),
                PathBuf::from("../MEPHISTO/Ontology-Development/files/processed/rg_ontology.ttl"),
            ],
        }
    }
}

impl StoreConfig {
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

    pub fn ontology_candidates(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(path) = &self.ontology_path {
            candidates.push(path.clone());
        }

        candidates.extend(self.ontology_fallbacks.iter().cloned());
        candidates
    }
}
