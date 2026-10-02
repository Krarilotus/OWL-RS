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
use crate::repository_config::RepositorySettings;

/// A repository's settings, in its directory.
const SETTINGS_FILE: &str = "repository.json";

/// The default repository's id.
pub const DEFAULT_REPOSITORY: &str = "nrese";

/// A repository's write path, which a reconfiguration replaces ([`reconfigure`]).
pub type PipelineSlot = Arc<RwLock<Arc<MutationPipeline>>>;

/// One repository: its write path (and store) and its settings.
#[derive(Clone)]
pub struct Repository {
    pub pipeline: PipelineSlot,
    pub settings: Arc<RwLock<RepositorySettings>>,
}

/// The default repository's settings file in the data directory, if the server is on
/// disk and its settings were changed through the engine API.
pub fn default_settings_file(template: &StoreConfig) -> Option<PathBuf> {
    matches!(template.mode, StoreMode::OnDisk).then(|| template.data_dir.join(SETTINGS_FILE))
}

/// The default repository's stored settings (`None` if they were never changed).
pub fn stored_default_settings(
    template: &StoreConfig,
) -> Result<Option<RepositorySettings>, String> {
    let Some(file) = default_settings_file(template) else {
        return Ok(None);
    };
    match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", file.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

/// Gives the repository whose write path is `slot` the reasoning of `settings` (`fallback`
/// where they name none): a new write path over the same store takes over, the old one is
/// retired (its waiting writes are refused, retryable), and the inferences are recomputed
/// under the new rules (or cleared without reasoning). Returns the new reasoning's name.
pub fn reconfigure(
    slot: &PipelineSlot,
    settings: &RepositorySettings,
    fallback: &ReasonerService,
) -> Result<&'static str, ApiError> {
    let reasoner = match settings.reasoner_config().map_err(ApiError::bad_request)? {
        Some(config) => ReasonerService::new(config),
        None => fallback.clone(),
    };
    let store = Arc::clone(slot.read().store());
    let next = Arc::new(MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(reasoner),
    ));
    let previous = std::mem::replace(&mut *slot.write(), Arc::clone(&next));
    previous.retire();
    match next.reasoner().config().materialised_program() {
        Some(program) => {
            store
                .rematerialise(&program)
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        None => {
            store
                .clear_inferred()
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
    }
    Ok(next.reasoner().mode_name())
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
        Ok(Repository {
            pipeline: Arc::new(RwLock::new(Arc::new(MutationPipeline::new(
                Arc::new(store),
                Arc::new(reasoner),
            )))),
            settings: Arc::new(RwLock::new(settings)),
        })
    }

    /// Repository `id`, if there is one besides the default.
    pub fn get(&self, id: &str) -> Option<Repository> {
        self.others.read().get(id).cloned()
    }

    /// Repository `id`'s settings, if there is one besides the default.
    pub fn settings(&self, id: &str) -> Option<RepositorySettings> {
        self.others
            .read()
            .get(id)
            .map(|r| r.settings.read().clone())
    }

    /// Changes repository `id`'s settings (not the default's): its title, and its
    /// reasoning, which takes effect at once ([`reconfigure`]); kept in its directory.
    pub fn change(&self, id: &str, settings: RepositorySettings) -> Result<(), ApiError> {
        let repository = self
            .get(id)
            .ok_or_else(|| ApiError::not_found(format!("no repository '{id}'")))?;
        check(&settings)?;
        let before = repository.settings.read().clone();
        if (&before.reasoning, &before.rules) != (&settings.reasoning, &settings.rules) {
            reconfigure(&repository.pipeline, &settings, &self.reasoner)?;
        }
        if let Some(root) = &self.root {
            write_settings(&root.join(id).join(SETTINGS_FILE), &settings)?;
        }
        *repository.settings.write() = settings;
        Ok(())
    }

    /// The server's reasoning, for repositories whose settings name none.
    pub fn server_reasoner(&self) -> &ReasonerService {
        &self.reasoner
    }
}

/// Refuses settings that name no reasoning mode or whose rules don't compile.
pub fn check(settings: &RepositorySettings) -> Result<(), ApiError> {
    if let Some(name) = &settings.reasoning
        && settings.reasoning_mode().is_none()
    {
        return Err(ApiError::bad_request(format!("no reasoning mode '{name}'")));
    }
    settings.reasoner_config().map_err(ApiError::bad_request)?;
    Ok(())
}

/// Writes `settings` to `file`.
pub fn write_settings(
    file: &std::path::Path,
    settings: &RepositorySettings,
) -> Result<(), ApiError> {
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    std::fs::write(file, json)
        .map_err(|error| ApiError::internal(format!("{}: {error}", file.display())))
}

impl Repositories {
    /// The ids of the repositories besides the default, sorted, with their titles.
    pub fn list(&self) -> Vec<(String, Option<String>)> {
        self.others
            .read()
            .iter()
            .map(|(id, repository)| (id.clone(), repository.settings.read().title.clone()))
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
            return Err(ApiError::conflict(format!(
                "repository '{id}' exists already"
            )));
        }
        check(&settings)?;
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
