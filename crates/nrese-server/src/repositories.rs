//! Several datasets in one server, as RDF4J and GraphDB serve repositories. The configured
//! store is the default repository ([`DEFAULT_REPOSITORY`]), which `/dataset/…` and the
//! operator surfaces serve; the RDF4J protocol (`/repositories/{id}/…`) reaches every
//! repository by its id. Other repositories are created and removed through that protocol
//! (`PUT` and `DELETE /repositories/{id}`), each a store of its own with the default's
//! settings and reasoning: on disk under `repositories/<id>/` of the data directory (opened
//! again at start), or in memory when the server is.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::ReasonerService;
use nrese_store::{MutationPipeline, StoreConfig, StoreMode, StoreService};
use parking_lot::RwLock;

use crate::error::ApiError;
use crate::http::rdf4j::Rdf4jState;

/// The default repository's id.
pub const DEFAULT_REPOSITORY: &str = "nrese";

/// One repository: its write path (and store) and its RDF4J state.
#[derive(Clone)]
pub struct Repository {
    pub pipeline: Arc<MutationPipeline>,
    pub rdf4j: Arc<Rdf4jState>,
}

/// The repositories besides the default one.
pub struct Repositories {
    /// The default store's settings, for the others.
    template: StoreConfig,
    reasoner: ReasonerService,
    /// Where on-disk repositories live; `None` for an in-memory server.
    root: Option<PathBuf>,
    others: RwLock<BTreeMap<String, Repository>>,
}

/// Whether `id` can name a repository: letters, digits, `-`, `_` and `.`, at most 64.
fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && id != "."
        && id != ".."
}

impl Repositories {
    /// The repositories of a server whose default store has `template`'s settings: on disk,
    /// those found under `repositories/` of its data directory (one that fails to open is
    /// logged and left out).
    pub fn open(template: &StoreConfig, reasoner: ReasonerService) -> Self {
        let root = matches!(template.mode, StoreMode::OnDisk)
            .then(|| template.data_dir.join("repositories"));
        let repositories = Self {
            template: template.clone(),
            reasoner,
            root,
            others: RwLock::default(),
        };
        if let Some(root) = &repositories.root
            && let Ok(entries) = std::fs::read_dir(root)
        {
            for entry in entries.flatten() {
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !valid(&id) || !entry.path().is_dir() {
                    continue;
                }
                match repositories.start(&id) {
                    Ok(repository) => {
                        repositories.others.write().insert(id, repository);
                    }
                    Err(error) => tracing::warn!(repository = %id, %error, "repository not opened"),
                }
            }
        }
        repositories
    }

    /// Opens (or creates) repository `id`'s store and brings its inferences in line with
    /// the configured reasoning, as the server does for the default store at start.
    fn start(&self, id: &str) -> Result<Repository, String> {
        let config = StoreConfig {
            data_dir: self
                .root
                .as_ref()
                .map_or_else(|| self.template.data_dir.clone(), |root| root.join(id)),
            ontology_path: None,
            ..self.template.clone()
        };
        let store = StoreService::new(config).map_err(|error| error.to_string())?;
        match self.reasoner.config().materialised_program() {
            Some(program) if !store.reasoning_is_current(&program) => {
                store
                    .rematerialise(&program)
                    .map_err(|error| error.to_string())?;
            }
            Some(_) => {}
            None => {
                store.clear_inferred().map_err(|error| error.to_string())?;
            }
        }
        let rdf4j = Rdf4jState::with_file(
            self.root
                .as_ref()
                .map(|root| root.join(id).join("rdf4j-namespaces.json")),
        );
        Ok(Repository {
            pipeline: Arc::new(MutationPipeline::new(
                Arc::new(store),
                Arc::new(self.reasoner.clone()),
            )),
            rdf4j: Arc::new(rdf4j),
        })
    }

    /// Repository `id`, if there is one besides the default.
    pub fn get(&self, id: &str) -> Option<Repository> {
        self.others.read().get(id).cloned()
    }

    /// The ids of the repositories besides the default, sorted.
    pub fn ids(&self) -> Vec<String> {
        self.others.read().keys().cloned().collect()
    }

    /// Creates repository `id`, empty.
    pub fn create(&self, id: &str) -> Result<(), ApiError> {
        if !valid(id) {
            return Err(ApiError::bad_request(format!(
                "'{id}' can't name a repository (letters, digits, '-', '_', '.')"
            )));
        }
        if id == DEFAULT_REPOSITORY || self.others.read().contains_key(id) {
            return Err(ApiError::bad_request(format!(
                "repository '{id}' exists already"
            )));
        }
        let repository = self.start(id).map_err(ApiError::internal)?;
        self.others.write().insert(id.to_owned(), repository);
        Ok(())
    }

    /// Removes repository `id` and its data. The default repository stays.
    pub fn delete(&self, id: &str) -> Result<(), ApiError> {
        if id == DEFAULT_REPOSITORY {
            return Err(ApiError::bad_request(
                "the default repository can't be removed",
            ));
        }
        let Some(repository) = self.others.write().remove(id) else {
            return Err(ApiError::not_found(format!("no repository '{id}'")));
        };
        // The store goes with the last request that holds it; its files after that.
        drop(repository);
        if let Some(root) = &self.root {
            let dir = root.join(id);
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(dir = %dir.display(), %error, "repository files not removed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::valid;

    #[test]
    fn repository_ids() {
        assert!(valid("bench"));
        assert!(valid("my-repo_2.v1"));
        assert!(!valid(""));
        assert!(!valid(".."));
        assert!(!valid("a/b"));
        assert!(!valid(&"x".repeat(65)));
    }
}
