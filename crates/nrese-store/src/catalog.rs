//! The repository catalogue: several datasets in one server, as RDF4J and GraphDB serve
//! repositories. The configured store is the default repository ([`DEFAULT_REPOSITORY`]);
//! others are created and removed through the engine API or a protocol, each a store of
//! its own with the default's settings: on disk under `repositories/<id>/` of the data
//! directory (opened again at start), or in memory when the server is. A repository's
//! title, reasoning and rules are its [`RepositorySettings`] (the server's reasoning when
//! they name none), kept in `repository.json` in its directory.
//!
//! A repository's lifecycle is serialised: creation and removal reserve its id, and a
//! change or removal holds the repository's lifecycle lock throughout. A change prepares
//! (checks, compiles, recomputes the inferences) with the old write path retired, writes
//! the settings whole ([`crate::metadata`]), then publishes; on failure the old reasoning
//! is published again, so the settings on disk are always the acknowledged ones. A removal
//! writes a tombstone beside the repositories (outside the directory it reclaims) before
//! reclaiming the files, and removes it only once the directory is gone: a repository
//! whose files can't be removed yet is never opened again. A repository whose stored settings or files can't be read is
//! listed as unavailable with the reason ([`Catalog::unavailable`]), not left out.
//!
//! The catalogue is the store's (ADR-0007): the server, the command line and embedders
//! use the same one; protocols only translate their requests and errors.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use parking_lot::{Mutex, RwLock};
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
#[serde(deny_unknown_fields)]
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
            "n3" => UserRules::n3(self.name.clone(), self.text.clone()),
            other => return Err(format!("rules format must be 'n3' or 'pie', not '{other}'")),
        };
        rules.map_err(|error| error.to_string())
    }
}

/// What a repository is created with besides the server's settings. Unknown fields are
/// refused: a misspelt setting would otherwise be silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
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
    /// How long one query on this repository may run, in milliseconds; the server's
    /// query timeout when absent. Lets one server hold datasets with different budgets,
    /// e.g. a small one answering an application in seconds beside a large public one.
    /// Applies to repositories other than the default, which keeps the server's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_timeout_ms: Option<u64>,
    /// When queries get OWL 2 QL answers through existentials (`auto`, `on`, `off`;
    /// docs/design/ql-rewriting.md §4); the server's `reasoner.ql_rewriting` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ql_rewriting: Option<String>,
}

/// The longest per-repository query timeout: one hour.
pub const MAX_QUERY_TIMEOUT_MS: u64 = 3_600_000;

impl RepositorySettings {
    /// The repository's own query timeout, if its settings give one.
    pub fn query_timeout(&self) -> Option<std::time::Duration> {
        self.query_timeout_ms.map(std::time::Duration::from_millis)
    }

    /// The QL rewriting's mode, if the settings choose one.
    pub fn ql_rewriting_mode(&self) -> Option<crate::QlRewritingMode> {
        self.ql_rewriting
            .as_deref()
            .and_then(crate::QlRewritingMode::from_name)
    }

    /// The reasoning mode, if the settings choose one.
    pub fn reasoning_mode(&self) -> Option<ReasoningMode> {
        self.reasoning.as_deref().and_then(ReasoningMode::from_name)
    }

    /// The repository's reasoner configuration, if the settings choose one (else the
    /// server's applies). Rules need a mode: the server's reasoning would leave them out.
    pub fn reasoner_config(&self) -> Result<Option<ReasonerConfig>, String> {
        let Some(mode) = self.reasoning_mode() else {
            return match self.rules {
                Some(_) => Err("rules need a reasoning mode ('custom' runs them alone, a \
                     ruleset's name adds them to it)"
                    .to_owned()),
                None => Ok(None),
            };
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

/// A repository's write path, which a change of its reasoning replaces ([`Catalog::change`]).
pub type PipelineSlot = Arc<RwLock<Arc<MutationPipeline>>>;

/// One repository: its write path (and store) and its settings.
#[derive(Clone)]
pub struct Repository {
    pub pipeline: PipelineSlot,
    pub settings: Arc<RwLock<RepositorySettings>>,
    /// Held through a change or removal of this repository.
    lifecycle: Arc<Mutex<()>>,
}

impl Repository {
    fn new(pipeline: MutationPipeline, settings: RepositorySettings) -> Self {
        Self {
            pipeline: Arc::new(RwLock::new(Arc::new(pipeline))),
            settings: Arc::new(RwLock::new(settings)),
            lifecycle: Arc::default(),
        }
    }
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

/// A removal's tombstone, beside the repositories: `~removed-<id>`, a name no repository can
/// have ([`existing`]: no `~`). Written (durably) before the files are reclaimed, outside the
/// directory a reclamation removes, and removed only once that directory is gone: a
/// repository it names is never opened again, only reclaimed.
const REMOVED: &str = "~removed-";

fn tombstone(root: &std::path::Path, id: &str) -> PathBuf {
    root.join(format!("{REMOVED}{id}"))
}

/// Brings `store`'s inferences in line with `reasoner`: recomputed under its rules, or
/// cleared without reasoning.
fn settle(store: &StoreService, reasoner: &ReasonerService) -> Result<(), CatalogError> {
    match reasoner.config().materialised_program() {
        Some(program) => store.rematerialise(&program).map(drop),
        None => store.clear_inferred().map(drop),
    }
    .map_err(|error| CatalogError::Store(error.to_string()))
}

/// How a removal ended ([`Catalog::delete`]): the repository is gone either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Its files are deleted.
    Reclaimed,
    /// Its files are still held (or couldn't be deleted): its tombstone (or its directory's
    /// trash name) keeps it removed, and they are deleted at the next start.
    Pending,
}

/// The repositories: the default one and the others, with their stores, write paths and
/// settings.
pub struct Catalog {
    /// The default repository: the configured store.
    default: Repository,
    /// The default store's settings, for the others.
    template: StoreConfig,
    runtime: Arc<crate::Runtime>,
    reasoner: ReasonerService,
    /// Where on-disk repositories live; `None` for an in-memory server.
    root: Option<PathBuf>,
    others: RwLock<BTreeMap<String, Repository>>,
    /// Repositories on disk that didn't open, with why and how to repair them.
    unavailable: RwLock<BTreeMap<String, String>>,
    /// Ids (ASCII lowercase) being created or removed.
    reserved: Mutex<BTreeSet<String>>,
}

/// An id reserved by a creation or removal, until it is dropped.
struct Reservation<'a> {
    reserved: &'a Mutex<BTreeSet<String>>,
    key: String,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.reserved.lock().remove(&self.key);
    }
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

/// Whether a directory of `repositories/` named `id` is a repository an earlier version
/// created: letters, digits, `-`, `_` and `.`, at most 64, not `.` or `..` (the rule
/// before 3 October 2026). It is opened as it is, though [`valid`] refuses new ones like it
/// (`.staging`, `con` on Linux): a store written by `main` keeps all its repositories.
fn existing(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && id != "."
        && id != ".."
}

/// What a repository's directory is renamed to while it is removed: a name no repository
/// can have, now or before ([`existing`]: no `~`), so that the start never deletes one.
const TRASH: &str = "~trash-";

impl Catalog {
    /// The repositories of a server whose default store is `store`, reasoning with
    /// `reasoner` where a repository's settings name none: the default with its stored
    /// settings (those changed through the engine API), and on disk the others found under
    /// `repositories/` of its data directory (one that fails to open is logged and left
    /// out).
    pub fn open(store: StoreService, reasoner: ReasonerService) -> Result<Self, CatalogError> {
        let template = store.config().clone();
        let runtime = Arc::clone(store.runtime());
        let default_settings = stored_default_settings(&template)
            .map_err(CatalogError::Store)?
            .unwrap_or_default();
        if let Err(error) = check(&default_settings) {
            let file = default_settings_file(&template).unwrap_or_default();
            return Err(CatalogError::Store(repair(&file, &error.to_string())));
        }
        let default = Repository::new(
            MutationPipeline::new(Arc::new(store), Arc::new(reasoner.clone())),
            default_settings,
        );
        let root = matches!(template.mode, StoreMode::OnDisk)
            .then(|| template.data_dir.join("repositories"));
        let repositories = Self {
            default,
            template,
            runtime,
            reasoner,
            root,
            others: RwLock::default(),
            unavailable: RwLock::default(),
            reserved: Mutex::default(),
        };
        if let Some(root) = &repositories.root
            && let Ok(entries) = std::fs::read_dir(root)
        {
            let entries: Vec<_> = entries.flatten().collect();
            // Removals: their repositories reclaimed (again) and never opened.
            let removed: BTreeSet<String> = entries
                .iter()
                .filter_map(|entry| {
                    let name = entry.file_name();
                    name.to_str()?.strip_prefix(REMOVED).map(str::to_owned)
                })
                .collect();
            for id in &removed {
                if repositories.reclaim(root, id) == Removal::Pending {
                    tracing::warn!(repository = %id, "removed repository's files not deleted yet");
                }
            }
            for entry in entries {
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
                if !existing(&id) || !entry.path().is_dir() || removed.contains(&id) {
                    continue;
                }
                if !valid(&id) {
                    tracing::warn!(repository = %id, "a repository id that can't be created any more (it starts or ends with '.' or is a device name): opened as it is; copy it into a repository with another id to move the data directory between systems");
                }
                let file = entry.path().join(SETTINGS_FILE);
                let settings = match std::fs::read(&file) {
                    // Settings written by hand meet the same checks as those sent to the API.
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|error| error.to_string())
                        .and_then(|settings| {
                            check(&settings)
                                .map(|()| settings)
                                .map_err(|error| error.to_string())
                        })
                        .map_err(|error| repair(&file, &error)),
                    // Created before settings were kept: the server's.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Ok(RepositorySettings::default())
                    }
                    Err(error) => Err(repair(&file, &error.to_string())),
                };
                match settings.and_then(|settings| repositories.start(&id, settings)) {
                    Ok(repository) => {
                        repositories.others.write().insert(id, repository);
                    }
                    Err(error) => {
                        tracing::error!(repository = %id, %error, "repository unavailable");
                        repositories.unavailable.write().insert(id, error);
                    }
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
            ql_rewriting: settings
                .ql_rewriting_mode()
                .unwrap_or(self.template.ql_rewriting),
            ..self.template.clone()
        };
        let store = StoreService::with_runtime(config, Arc::clone(&self.runtime))
            .map_err(|error| error.to_string())?;
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
        Ok(Repository::new(
            MutationPipeline::new(Arc::new(store), Arc::new(reasoner)),
            settings,
        ))
    }

    /// The reasoning of `settings`: theirs, else the server's.
    fn reasoner_for(&self, settings: &RepositorySettings) -> Result<ReasonerService, CatalogError> {
        Ok(
            match settings.reasoner_config().map_err(CatalogError::Invalid)? {
                Some(config) => ReasonerService::new(config),
                None => self.reasoner.clone(),
            },
        )
    }

    /// Where repository `id`'s settings are kept (`None` in memory).
    fn settings_file(&self, id: &str) -> Option<PathBuf> {
        match id {
            DEFAULT_REPOSITORY => default_settings_file(&self.template),
            id => self
                .root
                .as_ref()
                .map(|root| root.join(id).join(SETTINGS_FILE)),
        }
    }

    /// Why repository `id` didn't open at start, and how to repair it.
    pub fn unavailable(&self, id: &str) -> Option<String> {
        self.unavailable.read().get(id).cloned()
    }

    /// The repositories that didn't open at start, sorted, each with why.
    pub fn unavailable_list(&self) -> Vec<(String, String)> {
        self.unavailable
            .read()
            .iter()
            .map(|(id, why)| (id.clone(), why.clone()))
            .collect()
    }

    /// Reserves `id` for a creation or removal: refused while another holds it.
    fn reserve(&self, id: &str) -> Result<Reservation<'_>, CatalogError> {
        let key = id.to_ascii_lowercase();
        if !self.reserved.lock().insert(key.clone()) {
            return Err(CatalogError::Conflict(format!(
                "repository '{id}' is being created or removed"
            )));
        }
        Ok(Reservation {
            reserved: &self.reserved,
            key,
        })
    }

    /// Deletes removed repository `id`'s directory (its tombstone written): renamed out of
    /// the way first, at once, so that files still held (mapped checkpoints of a request
    /// in flight, on Windows) leave a `~trash-` directory the next start deletes; then the
    /// tombstone goes. A directory that can't be renamed keeps its name and its tombstone,
    /// whole (nothing is deleted under a name a start could open), for the next start.
    fn reclaim(&self, root: &std::path::Path, id: &str) -> Removal {
        let dir = root.join(id);
        let trash = (0..)
            .map(|n| root.join(format!("{TRASH}{id}-{n}")))
            .find(|path| !path.exists())
            .expect("a free name");
        match std::fs::rename(&dir, &trash) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.unmark(root, id);
                return Removal::Reclaimed;
            }
            Err(error) => {
                tracing::warn!(dir = %dir.display(), %error, "removed repository's directory not renamed; reclaimed at the next start");
                return Removal::Pending;
            }
        }
        self.unmark(root, id);
        match std::fs::remove_dir_all(&trash) {
            Ok(()) => Removal::Reclaimed,
            Err(error) => {
                tracing::warn!(dir = %trash.display(), %error, "removed repository's files not deleted yet; deleted at the next start");
                Removal::Pending
            }
        }
    }

    /// Removes repository `id`'s tombstone, its directory gone. One that stays (the
    /// deletion failed) names nothing: the next start removes it.
    fn unmark(&self, root: &std::path::Path, id: &str) {
        if let Err(error) = crate::metadata::remove(&tombstone(root, id)) {
            tracing::warn!(repository = %id, %error, "removed repository's tombstone not deleted");
        }
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
    /// at once. Kept in its directory on disk; the default repository's override the
    /// configuration's reasoning at the next start.
    ///
    /// A new reasoning is prepared with the old write path retired (its writes are refused,
    /// retryable, until the new one is published): compiled, the inferences recomputed,
    /// the settings written, then a write path over the store with it published. A failure
    /// publishes the old reasoning again, its inferences recomputed if they had changed.
    pub fn change(&self, id: &str, settings: RepositorySettings) -> Result<(), CatalogError> {
        let missing = || CatalogError::NotFound(format!("no repository '{id}'"));
        let repository = self.get(id).ok_or_else(missing)?;
        let _lifecycle = repository.lifecycle.lock();
        // Removed while this waited.
        if id != DEFAULT_REPOSITORY
            && !self
                .others
                .read()
                .get(id)
                .is_some_and(|r| Arc::ptr_eq(&r.lifecycle, &repository.lifecycle))
        {
            return Err(missing());
        }
        check(&settings)?;
        let before = repository.settings.read().clone();
        let persist = || match self.settings_file(id) {
            Some(file) => write_settings(&file, &settings),
            None => Ok(()),
        };
        if (&before.reasoning, &before.rules) != (&settings.reasoning, &settings.rules) {
            let reasoner = self.reasoner_for(&settings)?;
            let previous = Arc::clone(&*repository.pipeline.read());
            let store = Arc::clone(previous.store());
            // First: the old write path would otherwise recompute its own inferences over
            // the new ones.
            previous.retire();
            let restore = || {
                *repository.pipeline.write() = Arc::new(MutationPipeline::new(
                    Arc::clone(&store),
                    Arc::clone(previous.reasoner()),
                ));
            };
            if let Err(error) = settle(&store, &reasoner) {
                // One commit that didn't happen: the inferences are the old ones.
                restore();
                return Err(error);
            }
            if let Err(error) = persist() {
                if let Err(back) = settle(&store, previous.reasoner()) {
                    // The old write path recomputes them at its first write, a start too.
                    tracing::error!(repository = %id, error = %back, "inferences not recomputed under the old reasoning");
                }
                restore();
                return Err(error);
            }
            *repository.pipeline.write() =
                Arc::new(MutationPipeline::new(store, Arc::new(reasoner)));
        } else {
            persist()?;
        }
        if before.ql_rewriting != settings.ql_rewriting {
            let mode = settings
                .ql_rewriting_mode()
                .unwrap_or(self.template.ql_rewriting);
            repository.pipeline.read().store().set_ql_rewriting(mode);
        }
        *repository.settings.write() = settings;
        Ok(())
    }

    /// The server's reasoning, for repositories whose settings name none.
    pub fn server_reasoner(&self) -> &ReasonerService {
        &self.reasoner
    }
}

/// Refuses settings that name an unknown reasoning mode, rules without a mode, or rules
/// that don't compile.
pub fn check(settings: &RepositorySettings) -> Result<(), CatalogError> {
    if let Some(name) = &settings.reasoning
        && settings.reasoning_mode().is_none()
    {
        return Err(CatalogError::Invalid(format!("no reasoning mode '{name}'")));
    }
    if let Some(name) = &settings.ql_rewriting
        && settings.ql_rewriting_mode().is_none()
    {
        return Err(CatalogError::Invalid(format!(
            "ql_rewriting must be 'auto', 'on' or 'off', not '{name}'"
        )));
    }
    if let Some(ms) = settings.query_timeout_ms
        && !(1..=MAX_QUERY_TIMEOUT_MS).contains(&ms)
    {
        return Err(CatalogError::Invalid(format!(
            "query_timeout_ms must be between 1 and {MAX_QUERY_TIMEOUT_MS}, not {ms}"
        )));
    }
    settings.reasoner_config().map_err(CatalogError::Invalid)?;
    Ok(())
}

/// Writes `settings` to `file`, whole or not at all ([`crate::metadata`]).
pub fn write_settings(
    file: &std::path::Path,
    settings: &RepositorySettings,
) -> Result<(), CatalogError> {
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|error| CatalogError::Store(error.to_string()))?;
    crate::metadata::write(file, &json)
        .map_err(|error| CatalogError::Store(format!("{}: {error}", file.display())))
}

/// Why the stored settings in `file` are refused, and how to repair them.
fn repair(file: &std::path::Path, error: &str) -> String {
    format!(
        "{}: {error}. Repair or remove the file (the repository then takes the server's \
         settings) and start again, or remove the repository",
        file.display()
    )
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

    /// Creates repository `id`, empty, with `settings`: its id reserved, its settings
    /// written, then its store opened. A failed start removes what was made.
    pub fn create(&self, id: &str, settings: RepositorySettings) -> Result<(), CatalogError> {
        if !valid(id) {
            return Err(CatalogError::Invalid(format!(
                "'{id}' can't name a repository (letters, digits, '-', '_', '.', not starting or ending with '.', no device name such as 'CON')"
            )));
        }
        check(&settings)?;
        let _reservation = self.reserve(id)?;
        // Ids differing only in case are one directory on Windows and macOS.
        let taken = |other: &str| other.eq_ignore_ascii_case(id);
        let existing = std::iter::once(DEFAULT_REPOSITORY.to_owned())
            .chain(self.others.read().keys().cloned())
            .chain(self.unavailable.read().keys().cloned())
            .find(|other| taken(other));
        if let Some(other) = existing {
            return Err(CatalogError::Conflict(format!(
                "repository '{other}' exists already"
            )));
        }
        if let Some(root) = &self.root {
            let dir = root.join(id);
            // A removal whose files weren't deleted: done now, or the id stays taken.
            if tombstone(root, id).exists() && self.reclaim(root, id) == Removal::Pending {
                return Err(CatalogError::Conflict(format!(
                    "repository '{id}' is still being removed (its files are held); try again later"
                )));
            }
            if dir.exists() {
                return Err(CatalogError::Conflict(format!(
                    "{} exists already and is no repository of the catalogue",
                    dir.display()
                )));
            }
            std::fs::create_dir_all(&dir)
                .map_err(|error| CatalogError::Store(format!("{}: {error}", dir.display())))?;
            write_settings(&dir.join(SETTINGS_FILE), &settings)?;
        }
        let repository = match self.start(id, settings) {
            Ok(repository) => repository,
            Err(error) => {
                if let Some(root) = &self.root {
                    self.remove_files(root, id);
                }
                return Err(CatalogError::Store(error));
            }
        };
        self.others.write().insert(id.to_owned(), repository);
        Ok(())
    }

    /// Removes repository `id` and its data (one that didn't open too); the default
    /// repository stays. Its tombstone is written first; then it leaves the catalogue, its
    /// write path is retired, and its files are deleted, at once or, while requests in
    /// flight hold them, at the next start ([`Removal::Pending`]).
    pub fn delete(&self, id: &str) -> Result<Removal, CatalogError> {
        if id == DEFAULT_REPOSITORY {
            return Err(CatalogError::Invalid(
                "the default repository can't be removed".to_owned(),
            ));
        }
        let _reservation = self.reserve(id)?;
        let repository = self.others.read().get(id).cloned();
        if repository.is_none() && !self.unavailable.read().contains_key(id) {
            return Err(CatalogError::NotFound(format!("no repository '{id}'")));
        }
        let _lifecycle = repository.as_ref().map(|r| r.lifecycle.lock());
        if let Some(root) = &self.root {
            let tombstone = tombstone(root, id);
            crate::metadata::write(&tombstone, b"").map_err(|error| {
                CatalogError::Store(format!("{}: {error}", tombstone.display()))
            })?;
        }
        self.others.write().remove(id);
        self.unavailable.write().remove(id);
        // The store goes with the last request that holds it, and takes no more writes.
        if let Some(repository) = &repository {
            repository.pipeline.read().retire();
        }
        Ok(match &self.root {
            Some(root) => self.reclaim(root, id),
            None => Removal::Reclaimed,
        })
    }

    /// Removes the files of repository `id`, which isn't in the catalogue: tombstoned,
    /// then reclaimed.
    fn remove_files(&self, root: &std::path::Path, id: &str) {
        let tombstone = tombstone(root, id);
        if let Err(error) = crate::metadata::write(&tombstone, b"") {
            tracing::warn!(file = %tombstone.display(), %error, "tombstone not written");
        }
        self.reclaim(root, id);
    }
}

#[cfg(test)]
mod tests {
    use super::{TRASH, existing, valid};

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

    /// Repositories created before the rule above are opened (the main-store fixture
    /// has `.staging` and `.trash-kept`), and no repository's directory looks like trash.
    #[test]
    fn earlier_repository_ids_are_kept() {
        for id in [".staging", ".trash-kept", "repo.", "con", "my-repo_2.v1"] {
            assert!(existing(id), "{id}");
        }
        for id in ["", ".", "..", "a/b", "~x"] {
            assert!(!existing(id), "{id}");
        }
        assert!(!existing(&format!("{TRASH}a-0")));
    }
}
