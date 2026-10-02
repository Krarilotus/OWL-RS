//! Several datasets in one server, as RDF4J and GraphDB serve repositories. The configured
//! store is the default repository ([`DEFAULT_REPOSITORY`]), which `/dataset/…` and the
//! operator surfaces serve; the RDF4J protocol (`/repositories/{id}/…`) reaches every
//! repository by its id. Other repositories are created and removed through that protocol
//! (`PUT` and `DELETE /repositories/{id}`), each a store of its own with the default's
//! settings: on disk under `repositories/<id>/` of the data directory (opened again at
//! start), or in memory when the server is. A repository's title and reasoning come from
//! the configuration it was created with ([`crate::repository_config`]; the server's
//! reasoning when it names none), kept in `repository.json` in its directory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::ReasonerService;
use nrese_store::{MutationPipeline, StoreConfig, StoreMode, StoreService};
use parking_lot::RwLock;

use crate::error::ApiError;
use crate::http::rdf4j::Rdf4jState;
use crate::repository_config::RepositorySettings;

/// A repository's settings, in its directory.
const SETTINGS_FILE: &str = "repository.json";

/// The default repository's id.
pub const DEFAULT_REPOSITORY: &str = "nrese";

/// One repository: its write path (and store) and its RDF4J state.
#[derive(Clone)]
pub struct Repository {
    pub pipeline: Arc<MutationPipeline>,
    pub rdf4j: Arc<Rdf4jState>,
    pub settings: RepositorySettings,
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
                let settings = match std::fs::read(entry.path().join(SETTINGS_FILE)) {
                    Ok(bytes) => match serde_json::from_slice(&bytes) {
                        Ok(settings) => settings,
                        Err(error) => {
                            tracing::warn!(repository = %id, %error, "repository settings not read");
                            continue;
                        }
                    },
                    // Created before settings were kept: the server's.
                    Err(_) => RepositorySettings::default(),
                };
                match repositories.start(&id, settings) {
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
    /// its reasoning, as the server does for the default store at start.
    fn start(&self, id: &str, settings: RepositorySettings) -> Result<Repository, String> {
        let reasoner = match settings.reasoner_config()? {
            Some(config) => ReasonerService::new(config),
            None => self.reasoner.clone(),
        };
        let config = StoreConfig {
            data_dir: self
                .root
                .as_ref()
                .map_or_else(|| self.template.data_dir.clone(), |root| root.join(id)),
            ontology_path: None,
            ..self.template.clone()
        };
        let store = StoreService::new(config).map_err(|error| error.to_string())?;
        match reasoner.config().materialised_program() {
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
            pipeline: Arc::new(MutationPipeline::new(Arc::new(store), Arc::new(reasoner))),
            rdf4j: Arc::new(rdf4j),
            settings,
        })
    }

    /// Repository `id`, if there is one besides the default.
    pub fn get(&self, id: &str) -> Option<Repository> {
        self.others.read().get(id).cloned()
    }

    /// The ids of the repositories besides the default, sorted, with their titles.
    pub fn list(&self) -> Vec<(String, Option<String>)> {
        self.others
            .read()
            .iter()
            .map(|(id, repository)| (id.clone(), repository.settings.title.clone()))
            .collect()
    }

    /// Creates repository `id`, empty, with `settings`.
    pub fn create(&self, id: &str, settings: RepositorySettings) -> Result<(), ApiError> {
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
        let repository = self
            .start(id, settings.clone())
            .map_err(ApiError::internal)?;
        if let Some(root) = &self.root {
            let file = root.join(id).join(SETTINGS_FILE);
            let json = serde_json::to_vec_pretty(&settings)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            std::fs::write(&file, json)
                .map_err(|error| ApiError::internal(format!("{}: {error}", file.display())))?;
        }
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
