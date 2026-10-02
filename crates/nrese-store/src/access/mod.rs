//! Users, workspaces and graph policies, kept by the engine ([ADR-0008]).
//!
//! What a request may read and write is decided here, from who it is (a [`Principal`]:
//! its user name, its roles, whether it administers) and the [`AccessState`]:
//!
//! - **Workspaces** are the unit users see: a named set of graphs (an IRI prefix of its
//!   own, [`AccessState::prefix`], plus graphs an administrator gives it) with members,
//!   each an owner, an editor or a viewer ([`Level`]). Owners manage the members; editors
//!   write the graphs; viewers read them.
//! - **Every user has a personal space**, the workspace `~name`: graphs under its own
//!   prefix that only the user writes, readable by whoever the user shares it with (as
//!   viewers). It exists for every named user, recorded or not.
//! - **Roles** carry organisation-wide rules: graphs (IRIs, or prefixes ending in `*`) a
//!   role reads, writes or is denied, and its right on the default graph. A user's rights
//!   are the union of its roles' and its memberships'; what it may write it may read; an
//!   explicit deny in any of its roles wins.
//! - **Users** recorded in the state add roles or the administrator's right to whoever
//!   logs in under their name, and hold the password of a local login.
//!
//! The state changes only through [`AccessControl::apply`], each change with its author,
//! time and a required reason, kept as RDF with its history ([`control`]). Enforcement is
//! on once the state says so ([`Settings::enforced`]); until then everyone may read and
//! write what their credentials allow, as without a policy.
//!
//! [ADR-0008]: ../../../../docs/adr/0008-users-workspaces-policies.md

mod control;
mod login;
mod queries;
mod rdf;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use nrese_rdf::GraphName;
use nrese_sparql::GraphAccess;
use serde::{Deserialize, Serialize};

pub use control::{AccessControl, ChangeRecord, HISTORY_GRAPH, STATE_GRAPH};
pub use login::{LoginLimits, SESSION_PREFIX, hash_password, verify_password};
pub use queries::{QUERIES_GRAPH, QueryDraft, SavedQuery};

/// What a role grants a user no rule names, when enforcement is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Fallback {
    /// A user no rule names and no workspace has may read and write nothing but its
    /// personal space.
    #[default]
    Deny,
    /// A user no rule names and no shared workspace has is unrestricted.
    Allow,
}

/// Whether users who may not read every graph see the inferred statements.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Inferred {
    #[default]
    Hidden,
    /// Visible to everyone who may read the default graph.
    Visible,
}

/// A role's right on the store's default graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DefaultGraphRight {
    #[default]
    None,
    Read,
    Write,
    /// Neither, whatever other roles grant.
    Deny,
}

/// A role's graphs: entries are IRIs, or IRI prefixes ending in `*`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RoleRule {
    pub name: String,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    /// Graphs the role's users may neither read nor write, whatever other roles grant.
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub default_graph: DefaultGraphRight,
    /// Whether the role's users may call other endpoints with `SERVICE` (a privilege of its
    /// own: it makes the server fetch URLs). Administrators and unrestricted users may.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub service: bool,
}

/// The policy file's form (`auth.access_policy`): role rules and the fallbacks. Imported
/// into the state ([`Change::Import`]) and exported from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicy {
    #[serde(default)]
    pub default: Fallback,
    #[serde(default)]
    pub inferred: Inferred,
    #[serde(default, rename = "role")]
    pub roles: Vec<RoleRule>,
}

/// A member's level in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Level {
    Viewer,
    Editor,
    Owner,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Editor => "editor",
            Self::Owner => "owner",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Viewer, Self::Editor, Self::Owner]
            .into_iter()
            .find(|level| level.name() == name)
    }
}

/// The state's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Settings {
    /// Whether access is restricted at all; off, everyone reads and writes what its
    /// credentials allow.
    pub enforced: bool,
    pub fallback: Fallback,
    pub inferred: Inferred,
    /// Whether users who aren't administrators may create workspaces (they own them).
    pub users_create_workspaces: bool,
    /// The shortest password a local login takes.
    pub min_password_length: u32,
    /// How long a local login's session lasts, in hours.
    pub session_hours: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enforced: false,
            fallback: Fallback::Deny,
            inferred: Inferred::Hidden,
            users_create_workspaces: true,
            min_password_length: 10,
            session_hours: 12,
        }
    }
}

/// A user the state records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    pub admin: bool,
    /// Roles besides those its credentials carry.
    pub roles: BTreeSet<String>,
    /// The PHC string of its local login's password, if it has one.
    pub password_hash: Option<String>,
}

/// A workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    pub title: Option<String>,
    /// The repository it is in; `None`: every repository.
    pub repository: Option<String>,
    /// Graphs it takes in besides its own prefix (IRIs, or prefixes ending in `*`).
    pub graphs: Vec<String>,
    pub members: BTreeMap<String, Level>,
}

/// Who a request is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Principal {
    /// Its user name, if its credentials name one.
    pub user: Option<String>,
    pub roles: BTreeSet<String>,
    /// Administrators are unrestricted and manage the state.
    pub admin: bool,
}

impl Principal {
    /// How the history names it.
    pub fn display(&self) -> String {
        match &self.user {
            Some(user) => user.clone(),
            None if self.admin => "administrator".to_owned(),
            None => format!(
                "roles: {}",
                self.roles.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// What one request may read and write: `None` is unrestricted.
#[derive(Debug, Clone, Default)]
pub struct AccessView {
    pub read: Option<Arc<GraphAccess>>,
    pub write: Option<Arc<GraphAccess>>,
    /// Who the view is for (a user name, else roles), for logs and running queries.
    pub origin: Option<String>,
}

impl AccessView {
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// Whether `graph` may be read.
    pub fn can_read(&self, graph: &GraphName) -> bool {
        self.read.as_ref().is_none_or(|set| set.allows_graph(graph))
    }

    /// Whether `graph` may be written.
    pub fn can_write(&self, graph: &GraphName) -> bool {
        self.write
            .as_ref()
            .is_none_or(|set| set.allows_graph(graph))
    }

    /// Whether the requester sees the inferred statements.
    pub fn sees_inferred(&self) -> bool {
        self.read.as_ref().is_none_or(|set| set.inferred)
    }

    /// Whether the user may read every graph (and the inferred statements).
    pub fn reads_everything(&self) -> bool {
        self.read.is_none()
    }

    /// The scope of the user's reads, for the store's read methods.
    pub fn read_scope(&self) -> crate::ReadScope {
        crate::ReadScope::of(self.read.clone())
    }

    /// The user as a requester of writes, for the store's write methods.
    pub fn requester(&self) -> crate::Requester {
        crate::Requester::new(self.read_scope(), crate::WriteScope::of(self.write.clone()))
    }
}

/// A change of the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Settings(Settings),
    PutRole(RoleRule),
    RemoveRole(String),
    /// Creates or updates a user; fields left `None` keep their value. `password_hash`:
    /// `Some(None)` removes the password.
    PutUser {
        name: String,
        admin: Option<bool>,
        roles: Option<BTreeSet<String>>,
        password_hash: Option<Option<String>>,
    },
    RemoveUser(String),
    /// Creates or updates a workspace; fields left `None` keep their value. Whoever
    /// creates one owns it.
    PutWorkspace {
        name: String,
        title: Option<Option<String>>,
        repository: Option<Option<String>>,
        graphs: Option<Vec<String>>,
    },
    RemoveWorkspace(String),
    /// Sets (or with `None` removes) a member's level.
    SetMember {
        workspace: String,
        user: String,
        level: Option<Level>,
    },
    /// Replaces the role rules and fallbacks with a policy file's, and turns enforcement
    /// on.
    Import(AccessPolicy),
}

/// Why a change is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessError {
    Forbidden(String),
    Invalid(String),
    NotFound(String),
    Conflict(String),
    /// Too many failed logins.
    Throttled(String),
    Store(String),
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden(m)
            | Self::Invalid(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Throttled(m)
            | Self::Store(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AccessError {}

/// The personal space's workspace name of `user`.
pub fn personal_space(user: &str) -> String {
    format!("~{user}")
}

/// Whether `name` can name a user: letters, digits and `. _ @ + | : -`, at most 128.
pub fn valid_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '+' | '|' | ':' | '-')
        })
}

/// Whether `name` can name a workspace other than a personal space.
fn valid_workspace_name(name: &str) -> bool {
    !name.starts_with('~') && valid_user_name(name)
}

/// Whether `name` can name a role (token claims name them freely): no control
/// characters, at most 256.
fn valid_role_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

/// Checks a graph entry: an IRI, or an IRI prefix ending in `*`.
fn check_entry(entry: &str) -> Result<(), AccessError> {
    let iri = entry.strip_suffix('*').unwrap_or(entry);
    let bad = iri.is_empty()
        || iri
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '{' | '}' | '\\'))
        || !iri.contains(':');
    match bad {
        true => Err(AccessError::Invalid(format!(
            "'{entry}' is neither a graph IRI nor a prefix ending in '*'"
        ))),
        false => Ok(()),
    }
}

/// Graph entries sorted, each once (as their statements are a set).
fn normalise(entries: &mut Vec<String>) {
    entries.sort();
    entries.dedup();
}

/// Adds a graph entry to `set`.
fn add(set: &mut GraphAccess, entry: &str) {
    match entry.strip_suffix('*') {
        Some(prefix) => set.prefixes.push(prefix.to_owned()),
        None => set.graphs.push(entry.to_owned()),
    }
}

/// Excludes a graph entry from `set`, whatever else it holds.
fn exclude(set: &mut GraphAccess, entry: &str) {
    match entry.strip_suffix('*') {
        Some(prefix) => set.excluded_prefixes.push(prefix.to_owned()),
        None => set.excluded.push(entry.to_owned()),
    }
}

/// `text` with every byte outside the unreserved characters percent-encoded (for names in
/// IRIs).
pub(crate) fn encode_name(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The users, workspaces, roles and settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessState {
    pub settings: Settings,
    pub roles: BTreeMap<String, RoleRule>,
    pub users: BTreeMap<String, User>,
    pub workspaces: BTreeMap<String, Workspace>,
    /// What workspace prefixes start with (configuration, not state).
    pub base: String,
}

impl Default for AccessState {
    fn default() -> Self {
        Self::new("urn:nrese:")
    }
}

impl AccessState {
    pub fn new(base: &str) -> Self {
        Self {
            settings: Settings::default(),
            roles: BTreeMap::new(),
            users: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            base: base.to_owned(),
        }
    }

    /// The graph IRI prefix of workspace `name`: `{base}space/{user}/` for a personal
    /// space, `{base}workspace/{name}/` for the others.
    pub fn prefix(&self, name: &str) -> String {
        match name.strip_prefix('~') {
            Some(user) => format!("{}space/{}/", self.base, encode_name(user)),
            None => format!("{}workspace/{}/", self.base, encode_name(name)),
        }
    }

    /// Whether `principal` administers (by its credentials or its user record).
    pub fn is_admin(&self, principal: &Principal) -> bool {
        principal.admin
            || principal
                .user
                .as_ref()
                .and_then(|user| self.users.get(user))
                .is_some_and(|user| user.admin)
    }

    /// `principal`'s roles: its credentials' and its user record's.
    pub fn roles_of(&self, principal: &Principal) -> BTreeSet<String> {
        let mut roles = principal.roles.clone();
        if let Some(user) = principal.user.as_ref().and_then(|u| self.users.get(u)) {
            roles.extend(user.roles.iter().cloned());
        }
        roles
    }

    /// The workspaces `user` is a member of in `repository` (every repository with
    /// `None`), with its level: its personal space first, as owner.
    pub fn memberships(&self, user: &str, repository: Option<&str>) -> Vec<(String, Level)> {
        let personal = personal_space(user);
        let mut found = vec![(personal.clone(), Level::Owner)];
        for (name, workspace) in &self.workspaces {
            if *name == personal {
                continue;
            }
            let here = match (&workspace.repository, repository) {
                (Some(own), Some(asked)) => own == asked,
                _ => true,
            };
            if let Some(&level) = workspace.members.get(user)
                && here
            {
                found.push((name.clone(), level));
            }
        }
        found
    }

    fn rules_for(&self, roles: &BTreeSet<String>) -> Vec<&RoleRule> {
        self.roles
            .values()
            .filter(|rule| roles.contains(&rule.name))
            .collect()
    }

    /// What `principal` may read and write in `repository`.
    pub fn view(&self, principal: &Principal, repository: &str) -> AccessView {
        if !self.settings.enforced || self.is_admin(principal) {
            return AccessView::unrestricted();
        }
        let roles = self.roles_of(principal);
        let rules = self.rules_for(&roles);
        let memberships = principal
            .user
            .as_deref()
            .map(|user| self.memberships(user, Some(repository)))
            .unwrap_or_default();
        let shared = memberships.len() > 1;
        if rules.is_empty() && !shared && self.settings.fallback == Fallback::Allow {
            return AccessView::unrestricted();
        }
        let (mut read, mut write) = (GraphAccess::default(), GraphAccess::default());
        for rule in &rules {
            for entry in rule.read.iter().chain(&rule.write) {
                add(&mut read, entry);
            }
            for entry in &rule.write {
                add(&mut write, entry);
            }
            for entry in &rule.deny {
                exclude(&mut read, entry);
                exclude(&mut write, entry);
            }
            match rule.default_graph {
                DefaultGraphRight::None | DefaultGraphRight::Deny => {}
                DefaultGraphRight::Read => read.default_graph = true,
                DefaultGraphRight::Write => {
                    read.default_graph = true;
                    write.default_graph = true;
                }
            }
        }
        for (name, level) in &memberships {
            let prefix = format!("{}*", self.prefix(name));
            let taken_in = self
                .workspaces
                .get(name)
                .map(|workspace| workspace.graphs.as_slice())
                .unwrap_or_default();
            for entry in std::iter::once(&prefix).chain(taken_in) {
                add(&mut read, entry);
                if *level >= Level::Editor {
                    add(&mut write, entry);
                }
            }
        }
        if rules
            .iter()
            .any(|rule| rule.default_graph == DefaultGraphRight::Deny)
        {
            read.default_graph = false;
            write.default_graph = false;
        }
        read.inferred = read.default_graph && self.settings.inferred == Inferred::Visible;
        read.service = rules.iter().any(|rule| rule.service);
        AccessView {
            read: Some(Arc::new(read)),
            write: Some(Arc::new(write)),
            origin: None,
        }
    }

    /// Whether the state lets `principal` read beyond its credentials' grants: with a rule
    /// for one of its roles, or a user name (its personal space at least).
    pub fn grants_read(&self, principal: &Principal) -> bool {
        self.settings.enforced
            && (self.is_admin(principal)
                || principal.user.is_some()
                || !self.rules_for(&self.roles_of(principal)).is_empty())
    }

    /// Whether the state lets `principal` write data beyond its credentials' grants: with
    /// a rule that writes, or a user name (its personal space).
    pub fn grants_write(&self, principal: &Principal) -> bool {
        self.settings.enforced
            && (self.is_admin(principal)
                || principal.user.is_some()
                || self
                    .rules_for(&self.roles_of(principal))
                    .iter()
                    .any(|rule| {
                        !rule.write.is_empty() || rule.default_graph == DefaultGraphRight::Write
                    }))
    }

    /// The policy-file form of the role rules and fallbacks.
    pub fn export(&self) -> AccessPolicy {
        AccessPolicy {
            default: self.settings.fallback,
            inferred: self.settings.inferred,
            roles: self.roles.values().cloned().collect(),
        }
    }

    /// Applies `change` by `by`, if `by` may make it; returns what it did, for the
    /// history.
    pub fn apply(&mut self, by: &Principal, change: Change) -> Result<String, AccessError> {
        let admin = self.is_admin(by);
        let only_admins = |what: &str| -> Result<(), AccessError> {
            match admin {
                true => Ok(()),
                false => Err(AccessError::Forbidden(format!(
                    "only administrators {what}"
                ))),
            }
        };
        match change {
            Change::Settings(settings) => {
                only_admins("change the access settings")?;
                if settings.session_hours == 0 {
                    return Err(AccessError::Invalid(
                        "sessions must last an hour at least".to_owned(),
                    ));
                }
                let summary = format!(
                    "settings: enforced {}, fallback {:?}, inferred {:?}, users create workspaces {}, \
                     passwords of {} characters at least, sessions of {} hours",
                    settings.enforced,
                    settings.fallback,
                    settings.inferred,
                    settings.users_create_workspaces,
                    settings.min_password_length,
                    settings.session_hours
                );
                self.settings = settings;
                Ok(summary)
            }
            Change::PutRole(mut rule) => {
                only_admins("change roles")?;
                normalise(&mut rule.read);
                normalise(&mut rule.write);
                normalise(&mut rule.deny);
                if !valid_role_name(&rule.name) {
                    return Err(AccessError::Invalid(format!(
                        "'{}' can't name a role",
                        rule.name
                    )));
                }
                for entry in rule.read.iter().chain(&rule.write).chain(&rule.deny) {
                    check_entry(entry)?;
                }
                let summary = format!("role {} set", rule.name);
                self.roles.insert(rule.name.clone(), rule);
                Ok(summary)
            }
            Change::RemoveRole(name) => {
                only_admins("change roles")?;
                match self.roles.remove(&name) {
                    Some(_) => Ok(format!("role {name} removed")),
                    None => Err(AccessError::NotFound(format!("no role '{name}'"))),
                }
            }
            Change::PutUser {
                name,
                admin: make_admin,
                roles,
                password_hash,
            } => {
                let own_password = by.user.as_deref() == Some(name.as_str())
                    && make_admin.is_none()
                    && roles.is_none();
                if !own_password {
                    only_admins("change users other than their own password")?;
                }
                if !valid_user_name(&name) {
                    return Err(AccessError::Invalid(format!(
                        "'{name}' can't name a user (letters, digits, '. _ @ + | : -')"
                    )));
                }
                if let Some(roles) = &roles
                    && let Some(bad) = roles.iter().find(|role| !valid_role_name(role))
                {
                    return Err(AccessError::Invalid(format!("'{bad}' can't name a role")));
                }
                let created = !self.users.contains_key(&name);
                let user = self.users.entry(name.clone()).or_default();
                let mut parts = Vec::new();
                if let Some(make_admin) = make_admin {
                    user.admin = make_admin;
                    parts.push(format!("admin {make_admin}"));
                }
                if let Some(roles) = roles {
                    parts.push(format!(
                        "roles [{}]",
                        roles.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                    user.roles = roles;
                }
                if let Some(hash) = password_hash {
                    parts.push(
                        match hash.is_some() {
                            true => "password set",
                            false => "password removed",
                        }
                        .to_owned(),
                    );
                    user.password_hash = hash;
                }
                Ok(format!(
                    "user {name} {}{}",
                    if created { "created" } else { "changed" },
                    match parts.is_empty() {
                        true => String::new(),
                        false => format!(": {}", parts.join(", ")),
                    }
                ))
            }
            Change::RemoveUser(name) => {
                only_admins("remove users")?;
                if self.users.remove(&name).is_none() {
                    return Err(AccessError::NotFound(format!("no user '{name}'")));
                }
                // Its memberships go with it; its personal space's sharing too.
                self.workspaces.remove(&personal_space(&name));
                for workspace in self.workspaces.values_mut() {
                    workspace.members.remove(&name);
                }
                Ok(format!("user {name} removed"))
            }
            Change::PutWorkspace {
                name,
                title,
                repository,
                graphs,
            } => {
                let personal = name.strip_prefix('~');
                match personal {
                    Some(user) if !valid_user_name(user) => {
                        return Err(AccessError::Invalid(format!(
                            "'{name}' can't name a personal space"
                        )));
                    }
                    None if !valid_workspace_name(&name) => {
                        return Err(AccessError::Invalid(format!(
                            "'{name}' can't name a workspace (letters, digits, '. _ @ + | : -')"
                        )));
                    }
                    _ => {}
                }
                if graphs.is_some() {
                    only_admins("give workspaces graphs outside their own prefix")?;
                }
                for entry in graphs.iter().flatten() {
                    check_entry(entry)?;
                }
                let exists = self.workspaces.contains_key(&name);
                let owns = match personal {
                    Some(user) => by.user.as_deref() == Some(user),
                    None => by.user.as_deref().is_some_and(|user| {
                        self.workspaces.get(&name).and_then(|w| w.members.get(user))
                            == Some(&Level::Owner)
                    }),
                };
                if !(admin || owns) {
                    if exists || personal.is_some() {
                        return Err(AccessError::Forbidden(format!(
                            "only the owners of '{name}' and administrators change it"
                        )));
                    }
                    if by.user.is_none() || !self.settings.users_create_workspaces {
                        return Err(AccessError::Forbidden(
                            "only administrators create workspaces here".to_owned(),
                        ));
                    }
                }
                let workspace = self.workspaces.entry(name.clone()).or_default();
                if !exists
                    && personal.is_none()
                    && let Some(user) = &by.user
                {
                    workspace.members.insert(user.clone(), Level::Owner);
                }
                if let Some(title) = title {
                    workspace.title = title;
                }
                if let Some(repository) = repository {
                    workspace.repository = repository;
                }
                if let Some(mut graphs) = graphs {
                    normalise(&mut graphs);
                    workspace.graphs = graphs;
                }
                Ok(format!(
                    "workspace {name} {}",
                    if exists { "changed" } else { "created" }
                ))
            }
            Change::RemoveWorkspace(name) => {
                let owns = match name.strip_prefix('~') {
                    Some(user) => by.user.as_deref() == Some(user),
                    None => by.user.as_deref().is_some_and(|user| {
                        self.workspaces.get(&name).and_then(|w| w.members.get(user))
                            == Some(&Level::Owner)
                    }),
                };
                if !(admin || owns) {
                    return Err(AccessError::Forbidden(format!(
                        "only the owners of '{name}' and administrators remove it"
                    )));
                }
                match self.workspaces.remove(&name) {
                    Some(_) => Ok(format!("workspace {name} removed")),
                    None => Err(AccessError::NotFound(format!("no workspace '{name}'"))),
                }
            }
            Change::SetMember {
                workspace: name,
                user,
                level,
            } => {
                if !valid_user_name(&user) {
                    return Err(AccessError::Invalid(format!("'{user}' can't name a user")));
                }
                let personal = name.strip_prefix('~');
                let owns = match personal {
                    Some(owner) => by.user.as_deref() == Some(owner),
                    None => by.user.as_deref().is_some_and(|me| {
                        self.workspaces.get(&name).and_then(|w| w.members.get(me))
                            == Some(&Level::Owner)
                    }),
                };
                if !(admin || owns) {
                    return Err(AccessError::Forbidden(format!(
                        "only the owners of '{name}' and administrators change its members"
                    )));
                }
                if let Some(owner) = personal {
                    if owner == user {
                        return Err(AccessError::Invalid(
                            "a personal space's owner is its user".to_owned(),
                        ));
                    }
                    if level.is_some_and(|level| level != Level::Viewer) {
                        return Err(AccessError::Invalid(
                            "a personal space is shared with viewers only: only its user writes it"
                                .to_owned(),
                        ));
                    }
                } else if !self.workspaces.contains_key(&name) {
                    return Err(AccessError::NotFound(format!("no workspace '{name}'")));
                }
                let workspace = self.workspaces.entry(name.clone()).or_default();
                let before = workspace.members.get(&user).copied();
                match level {
                    Some(level) => {
                        workspace.members.insert(user.clone(), level);
                    }
                    None => {
                        workspace.members.remove(&user);
                    }
                }
                if personal.is_none()
                    && before == Some(Level::Owner)
                    && !workspace.members.values().any(|&l| l == Level::Owner)
                {
                    workspace.members.insert(user.clone(), Level::Owner);
                    return Err(AccessError::Conflict(format!(
                        "'{name}' would have no owner left"
                    )));
                }
                Ok(match level {
                    Some(level) => format!("{user} is {} of {name}", level.name()),
                    None => format!("{user} left {name}"),
                })
            }
            Change::Import(policy) => {
                only_admins("import policies")?;
                let mut names = BTreeSet::new();
                for rule in &policy.roles {
                    if !valid_role_name(&rule.name) || !names.insert(rule.name.as_str()) {
                        return Err(AccessError::Invalid(format!(
                            "role names must be present and unique ('{}')",
                            rule.name
                        )));
                    }
                    for entry in rule.read.iter().chain(&rule.write).chain(&rule.deny) {
                        check_entry(entry)?;
                    }
                }
                let summary = format!(
                    "policy imported: {} roles, fallback {:?}, inferred {:?}",
                    policy.roles.len(),
                    policy.default,
                    policy.inferred
                );
                self.settings.enforced = true;
                self.settings.fallback = policy.default;
                self.settings.inferred = policy.inferred;
                self.roles = policy
                    .roles
                    .into_iter()
                    .map(|mut rule| {
                        normalise(&mut rule.read);
                        normalise(&mut rule.write);
                        normalise(&mut rule.deny);
                        (rule.name.clone(), rule)
                    })
                    .collect();
                Ok(summary)
            }
        }
    }
}

#[cfg(test)]
mod tests;
