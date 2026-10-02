//! The repository catalogue: several datasets in one server, as RDF4J and GraphDB serve
//! repositories. The configured store is the default repository ([`DEFAULT_REPOSITORY`]);
//! others are created and removed through the engine API or a protocol, each a store of
//! its own with the default's settings: on disk under `repositories/<id>/` of the data
//! directory (opened again at start), or in memory when the server is. A repository's
//! title, reasoning and rules are its [`RepositorySettings`] (the server's reasoning when
//! they name none), kept in `repository.json` in its directory.
//!
//! The catalogue is the store's (ADR-0007): the server, the command line and embedders
//! use the same one; protocols only translate their requests and errors.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{MutationPipeline, StoreConfig, StoreMode, StoreService};

/// Why the catalogue refused or failed a change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// The request is wrong: an id that can't name a repository, settings that don't
    /// compile.
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    /// The store or its files failed.
    #[error("{0}")]
    Store(String),
}

/// A repository's user rules: their text, kept with the settings so that a restart needs
/// no file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RepositoryRules {
    /// Shown in errors and diagnostics: the file name, or `configuration`.
    pub name: String,
    /// `n3` or `pie`.
    pub format: String,
    pub text: String,
}

impl RepositoryRules {
    /// The rules, compiled to check them.
    fn compile(&self) -> Result<UserRules, String> {
        let rules = match self.format.as_str() {
            "pie" => UserRules::pie(self.name.clone(), self.text.clone()),
            _ => UserRules::n3(self.name.clone(), self.text.clone()),
        };
        rules.map_err(|error| error.to_string())
    }
}

/// What a repository is created with besides the server's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RepositorySettings {
    /// Shown in the repository list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The repository's reasoning, by mode name; the server's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// The repository's user rules, if it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<RepositoryRules>,
}

impl RepositorySettings {
    /// The reasoning mode, if the settings choose one.
    pub fn reasoning_mode(&self) -> Option<ReasoningMode> {
        self.reasoning.as_deref().and_then(ReasoningMode::from_name)
    }

    /// The repository's reasoner configuration, if the settings choose one (else the
    /// server's applies).
    pub fn reasoner_config(&self) -> Result<Option<ReasonerConfig>, String> {
        let Some(mode) = self.reasoning_mode() else {
            return Ok(None);
        };
        let rules = self
            .rules
            .as_ref()
            .map(|rules| rules.compile().map(Arc::new))
            .transpose()?;
        ReasonerConfig::for_mode(mode)
            .with_rules(rules)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

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
) -> Result<&'static str, CatalogError> {
    let reasoner = match settings.reasoner_config().map_err(CatalogError::Invalid)? {
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
                .map_err(|error| CatalogError::Store(error.to_string()))?;
        }
        None => {
            store
                .clear_inferred()
                .map_err(|error| CatalogError::Store(error.to_string()))?;
        }
    }
    Ok(next.reasoner().mode_name())
}

/// The repositories: the default one and the others, with their stores, write paths and
/// settings.
pub struct Catalog {
    /// The default repository: the configured store.
    default: Repository,
    /// The default store's settings, for the others.
    template: StoreConfig,
    reasoner: ReasonerService,
    /// Where on-disk repositories live; `None` for an in-memory server.
    root: Option<PathBuf>,
    others: RwLock<BTreeMap<String, Repository>>,
}

/// Whether `id` can name a repository: letters, digits, `-`, `_` and `.`, at most 64, not
/// starting with `.` (the catalogue's own directories do) nor ending with one (Windows
/// drops it: `a.` is `a`), and no Windows device name (`CON`, `NUL`, `COM1`, ... with or
/// without an extension), on every system, so that a data directory moves between them.
fn valid(id: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let stem = id.split('.').next().unwrap_or(id).to_ascii_uppercase();
    let device = DEVICES.contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !device
}

/// What a repository's directory is renamed to while it is removed.
const TRASH: &str = ".trash-";

impl Catalog {
    /// The repositories of a server whose default store is `store`, reasoning with
    /// `reasoner` where a repository's settings name none: the default with its stored
    /// settings (those changed through the engine API), and on disk the others found under
    /// `repositories/` of its data directory (one that fails to open is logged and left
    /// out).
    pub fn open(store: StoreService, reasoner: ReasonerService) -> Result<Self, CatalogError> {
        let template = store.config().clone();
        let default_settings = stored_default_settings(&template)
            .map_err(CatalogError::Store)?
            .unwrap_or_default();
        let default = Repository {
            pipeline: Arc::new(RwLock::new(Arc::new(MutationPipeline::new(
                Arc::new(store),
                Arc::new(reasoner.clone()),
            )))),
            settings: Arc::new(RwLock::new(default_settings)),
        };
        let root = matches!(template.mode, StoreMode::OnDisk)
            .then(|| template.data_dir.join("repositories"));
        let repositories = Self {
            default,
            template,
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
                // A removal that didn't finish (files still held when it ran): finished now.
                if id.starts_with(TRASH) {
                    if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                        tracing::warn!(dir = %entry.path().display(), %error, "removed repository's files not deleted");
                    }
                    continue;
                }
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
        Ok(repositories)
    }

    /// The default repository.
    pub fn default_repository(&self) -> &Repository {
        &self.default
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

    /// Repository `id`, the default one included.
    pub fn get(&self, id: &str) -> Option<Repository> {
        match id {
            DEFAULT_REPOSITORY => Some(self.default.clone()),
            id => self.others.read().get(id).cloned(),
        }
    }

    /// Repository `id`'s settings: those it was created with, or for the default
    /// repository those changed through the engine API (empty: the server's).
    pub fn settings(&self, id: &str) -> Option<RepositorySettings> {
        self.get(id).map(|r| r.settings.read().clone())
    }

    /// Changes repository `id`'s settings: its title, and its reasoning, which takes effect
    /// at once ([`reconfigure`]). Kept in its directory on disk; the default repository's
    /// override the configuration's reasoning at the next start.
    pub fn change(&self, id: &str, settings: RepositorySettings) -> Result<(), CatalogError> {
        let repository = self
            .get(id)
            .ok_or_else(|| CatalogError::NotFound(format!("no repository '{id}'")))?;
        check(&settings)?;
        let before = repository.settings.read().clone();
        if (&before.reasoning, &before.rules) != (&settings.reasoning, &settings.rules) {
            reconfigure(&repository.pipeline, &settings, &self.reasoner)?;
        }
        let file = match id {
            DEFAULT_REPOSITORY => default_settings_file(&self.template),
            id => self
                .root
                .as_ref()
                .map(|root| root.join(id).join(SETTINGS_FILE)),
        };
        if let Some(file) = file {
            write_settings(&file, &settings)?;
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
pub fn check(settings: &RepositorySettings) -> Result<(), CatalogError> {
    if let Some(name) = &settings.reasoning
        && settings.reasoning_mode().is_none()
    {
        return Err(CatalogError::Invalid(format!("no reasoning mode '{name}'")));
    }
    settings.reasoner_config().map_err(CatalogError::Invalid)?;
    Ok(())
}

/// Writes `settings` to `file`.
pub fn write_settings(
    file: &std::path::Path,
    settings: &RepositorySettings,
) -> Result<(), CatalogError> {
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|error| CatalogError::Store(error.to_string()))?;
    std::fs::write(file, json)
        .map_err(|error| CatalogError::Store(format!("{}: {error}", file.display())))
}

impl Catalog {
    /// The ids of the repositories besides the default, sorted, with their titles.
    pub fn list(&self) -> Vec<(String, Option<String>)> {
        self.others
            .read()
            .iter()
            .map(|(id, repository)| (id.clone(), repository.settings.read().title.clone()))
            .collect()
    }

    /// Creates repository `id`, empty, with `settings`.
    pub fn create(&self, id: &str, settings: RepositorySettings) -> Result<(), CatalogError> {
        if !valid(id) {
            return Err(CatalogError::Invalid(format!(
                "'{id}' can't name a repository (letters, digits, '-', '_', '.', not starting or ending with '.', no device name such as 'CON')"
            )));
        }
        // Ids differing only in case are one directory on Windows and macOS.
        let taken = |other: &str| other.eq_ignore_ascii_case(id);
        if let Some(other) = std::iter::once(DEFAULT_REPOSITORY)
            .chain(self.others.read().keys().map(String::as_str))
            .find(|other| taken(other))
            .map(str::to_owned)
        {
            return Err(CatalogError::Conflict(format!(
                "repository '{other}' exists already"
            )));
        }
        check(&settings)?;
        let repository = self
            .start(id, settings.clone())
            .map_err(CatalogError::Store)?;
        if let Some(root) = &self.root {
            let file = root.join(id).join(SETTINGS_FILE);
            let json = serde_json::to_vec_pretty(&settings)
                .map_err(|error| CatalogError::Store(error.to_string()))?;
            std::fs::write(&file, json)
                .map_err(|error| CatalogError::Store(format!("{}: {error}", file.display())))?;
        }
        self.others.write().insert(id.to_owned(), repository);
        Ok(())
    }

    /// Removes repository `id` and its data. The default repository stays.
    pub fn delete(&self, id: &str) -> Result<(), CatalogError> {
        if id == DEFAULT_REPOSITORY {
            return Err(CatalogError::Invalid(
                "the default repository can't be removed".to_owned(),
            ));
        }
        let Some(repository) = self.others.write().remove(id) else {
            return Err(CatalogError::NotFound(format!("no repository '{id}'")));
        };
        // The store goes with the last request that holds it; its files after that. The
        // directory is renamed first (at once), so that files still held (mapped
        // checkpoints of a request in flight, on Windows) leave a `.trash-` directory the
        // next start deletes, not a broken repository.
        drop(repository);
        if let Some(root) = &self.root {
            let dir = root.join(id);
            let trash = (0..)
                .map(|n| root.join(format!("{TRASH}{id}-{n}")))
                .find(|path| !path.exists())
                .expect("a free name");
            let doomed = match std::fs::rename(&dir, &trash) {
                Ok(()) => trash,
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "repository directory not renamed");
                    dir
                }
            };
            if let Err(error) = std::fs::remove_dir_all(&doomed) {
                tracing::warn!(dir = %doomed.display(), %error, "repository files not removed yet; deleted at the next start");
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
        // Hidden, trailing dots, device names (any case, with an extension).
        assert!(!valid(".trash-a-0"));
        assert!(!valid("repo."));
        for device in ["CON", "nul", "Aux.db", "com1", "LPT9.x"] {
            assert!(!valid(device), "{device}");
        }
        assert!(valid("console") && valid("com10") && valid("nullable"));
    }
}
