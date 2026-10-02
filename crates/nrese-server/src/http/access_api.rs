//! The engine API for users, workspaces and graph policies (ADR-0008): who the requester
//! is and what it may do, the state for administrators, and its changes, each with a
//! reason (`reason` in the JSON body, or as a query parameter of a `DELETE`), kept in the
//! history. Who may change what is the engine's decision ([`nrese_store::access`]):
//! administrators everything; owners their workspace and its members; every user its own
//! password and who sees its personal space.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nrese_store::access::{
    AccessState, Change, ChangeRecord, DefaultGraphRight, Fallback, Inferred, Level, Principal,
    RoleRule,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::http::guard;
use crate::state::AppState;

/// The requester, as the access state's changes see it.
async fn requester(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let identity = guard::enforce_query_read(state, headers).await?;
    Ok(state.access_principal(&identity))
}

/// Fails unless `principal` administers.
fn administrator(access: &AccessState, principal: &Principal) -> Result<(), ApiError> {
    match access.is_admin(principal) {
        true => Ok(()),
        false => Err(ApiError::forbidden(
            "only administrators see the whole access state",
        )),
    }
}

fn body<T: for<'de> Deserialize<'de>>(bytes: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|error| ApiError::bad_request(error.to_string()))
}

/// The `reason` query parameter (for `DELETE`s, which have no body).
fn reason_parameter(raw: &RawQuery) -> Result<String, ApiError> {
    Ok(super::rdf4j::pairs(raw)?
        .into_iter()
        .find(|(key, _)| key == "reason")
        .map(|(_, value)| value)
        .unwrap_or_default())
}

/// Applies `change` by the requester; the change's record.
async fn change(
    state: &AppState,
    headers: &HeaderMap,
    change: Change,
    reason: String,
) -> Result<Response, ApiError> {
    let by = requester(state, headers).await?;
    let access = state.clone();
    let record = tokio::task::spawn_blocking(move || access.access().apply(&by, change, &reason))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(record).into_response())
}

#[derive(Serialize)]
struct WorkspaceView {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Its graphs' IRI prefix.
    prefix: String,
    /// The repository it is in; absent: every repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    /// Graphs it takes in besides its prefix.
    graphs: Vec<String>,
    personal: bool,
    /// The requester's level, where it is a member.
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<Level>,
    members: BTreeMap<String, Level>,
}

fn workspace_view(access: &AccessState, name: &str, level: Option<Level>) -> WorkspaceView {
    let workspace = access.workspaces.get(name).cloned().unwrap_or_default();
    let mut members = workspace.members;
    if let Some(owner) = name.strip_prefix('~') {
        members.insert(owner.to_owned(), Level::Owner);
    }
    WorkspaceView {
        name: name.to_owned(),
        title: workspace.title,
        prefix: access.prefix(name),
        repository: workspace.repository,
        graphs: workspace.graphs,
        personal: name.starts_with('~'),
        level,
        members,
    }
}

#[derive(Serialize)]
struct Me {
    user: Option<String>,
    roles: BTreeSet<String>,
    admin: bool,
    /// Whether access is restricted at all.
    enforced: bool,
    /// Whether the requester reads every graph (in the repository asked about).
    reads_everything: bool,
    /// Its personal space, where it has a user name.
    #[serde(skip_serializing_if = "Option::is_none")]
    personal_space: Option<WorkspaceView>,
    workspaces: Vec<WorkspaceView>,
}

/// Who the requester is: its user name, roles, personal space and workspaces.
pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let identity = guard::enforce_query_read(&state, &headers).await?;
    let principal = crate::access::principal(&identity);
    let access = state.access().state();
    let memberships = principal
        .user
        .as_deref()
        .map(|user| access.memberships(user, None))
        .unwrap_or_default();
    let mut workspaces = memberships
        .iter()
        .map(|(name, level)| workspace_view(&access, name, Some(*level)));
    let personal_space = principal.user.as_ref().and_then(|_| workspaces.next());
    Ok(Json(Me {
        roles: access.roles_of(&principal),
        admin: access.is_admin(&principal),
        enforced: access.settings.enforced,
        reads_everything: state.access_view(&identity).reads_everything(),
        workspaces: workspaces.collect(),
        personal_space,
        user: principal.user,
    })
    .into_response())
}

#[derive(Serialize)]
struct UserView {
    name: String,
    admin: bool,
    roles: BTreeSet<String>,
    /// Whether it has a password for local logins.
    local_login: bool,
}

#[derive(Serialize)]
struct Overview {
    settings: nrese_store::access::Settings,
    roles: Vec<RoleRule>,
    users: Vec<UserView>,
    workspaces: Vec<WorkspaceView>,
}

/// The whole state (administrators): settings, roles, users, workspaces.
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let by = requester(&state, &headers).await?;
    let access = state.access().state();
    administrator(&access, &by)?;
    Ok(Json(Overview {
        settings: access.settings.clone(),
        roles: access.roles.values().cloned().collect(),
        users: access
            .users
            .iter()
            .map(|(name, user)| UserView {
                name: name.clone(),
                admin: user.admin,
                roles: user.roles.clone(),
                local_login: user.password_hash.is_some(),
            })
            .collect(),
        workspaces: access
            .workspaces
            .keys()
            .map(|name| workspace_view(&access, name, None))
            .collect(),
    })
    .into_response())
}

/// The workspaces the requester is in (administrators: every one recorded).
pub async fn workspaces(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let by = requester(&state, &headers).await?;
    let access = state.access().state();
    let views: Vec<WorkspaceView> = match (access.is_admin(&by), by.user.as_deref()) {
        (true, _) => access
            .workspaces
            .keys()
            .map(|name| workspace_view(&access, name, None))
            .collect(),
        (false, Some(user)) => access
            .memberships(user, None)
            .iter()
            .map(|(name, level)| workspace_view(&access, name, Some(*level)))
            .collect(),
        (false, None) => Vec::new(),
    };
    Ok(Json(views).into_response())
}

/// One workspace, for its members and administrators.
pub async fn workspace(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let by = requester(&state, &headers).await?;
    let access = state.access().state();
    let level = by
        .user
        .as_deref()
        .and_then(|user| match name.strip_prefix('~') {
            Some(owner) if owner == user => Some(Level::Owner),
            _ => access
                .workspaces
                .get(&name)
                .and_then(|w| w.members.get(user).copied()),
        });
    let known = name.starts_with('~') || access.workspaces.contains_key(&name);
    if !known || (level.is_none() && !access.is_admin(&by)) {
        return Err(ApiError::not_found(format!("no workspace '{name}'")));
    }
    Ok(Json(workspace_view(&access, &name, level)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsBody {
    enforced: Option<bool>,
    fallback: Option<Fallback>,
    inferred: Option<Inferred>,
    users_create_workspaces: Option<bool>,
    #[serde(default)]
    reason: String,
}

/// Changes the settings given (administrators).
pub async fn settings_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: SettingsBody = body(&bytes)?;
    let mut settings = state.access().state().settings.clone();
    if let Some(enforced) = request.enforced {
        settings.enforced = enforced;
    }
    if let Some(fallback) = request.fallback {
        settings.fallback = fallback;
    }
    if let Some(inferred) = request.inferred {
        settings.inferred = inferred;
    }
    if let Some(create) = request.users_create_workspaces {
        settings.users_create_workspaces = create;
    }
    change(&state, &headers, Change::Settings(settings), request.reason).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleBody {
    #[serde(default)]
    read: Vec<String>,
    #[serde(default)]
    write: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    default_graph: DefaultGraphRight,
    #[serde(default)]
    reason: String,
}

/// Sets role `name`'s rule (administrators).
pub async fn role_put(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: RoleBody = body(&bytes)?;
    let rule = RoleRule {
        name,
        read: request.read,
        write: request.write,
        deny: request.deny,
        default_graph: request.default_graph,
    };
    change(&state, &headers, Change::PutRole(rule), request.reason).await
}

/// Removes role `name`'s rule (administrators).
pub async fn role_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let reason = reason_parameter(&raw)?;
    change(&state, &headers, Change::RemoveRole(name), reason).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserBody {
    admin: Option<bool>,
    roles: Option<BTreeSet<String>>,
    #[serde(default)]
    reason: String,
}

/// Creates or changes user `name` (administrators).
pub async fn user_put(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: UserBody = body(&bytes)?;
    let change_user = Change::PutUser {
        name,
        admin: request.admin,
        roles: request.roles,
        password_hash: None,
    };
    change(&state, &headers, change_user, request.reason).await
}

/// Removes user `name` with its memberships (administrators).
pub async fn user_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let reason = reason_parameter(&raw)?;
    change(&state, &headers, Change::RemoveUser(name), reason).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceBody {
    /// Empty: no title.
    title: Option<String>,
    /// Empty or `*`: every repository.
    repository: Option<String>,
    /// Graphs it takes in besides its prefix (administrators only).
    graphs: Option<Vec<String>>,
    #[serde(default)]
    reason: String,
}

/// Creates workspace `name` (the requester owns it) or changes it (its owners).
pub async fn workspace_put(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: WorkspaceBody = body(&bytes)?;
    let none_if = |value: String, empty: &[&str]| -> Option<String> {
        (!empty.contains(&value.trim())).then_some(value)
    };
    let change_workspace = Change::PutWorkspace {
        name,
        title: request.title.map(|title| none_if(title, &[""])),
        repository: request
            .repository
            .map(|repository| none_if(repository, &["", "*"])),
        graphs: request.graphs,
    };
    change(&state, &headers, change_workspace, request.reason).await
}

/// Removes workspace `name` (its owners; for a personal space: who sees it).
pub async fn workspace_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let reason = reason_parameter(&raw)?;
    change(&state, &headers, Change::RemoveWorkspace(name), reason).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberBody {
    level: Level,
    #[serde(default)]
    reason: String,
}

/// Makes `user` a member of workspace `name` at a level (its owners).
pub async fn member_put(
    State(state): State<AppState>,
    Path((name, user)): Path<(String, String)>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: MemberBody = body(&bytes)?;
    let set = Change::SetMember {
        workspace: name,
        user,
        level: Some(request.level),
    };
    change(&state, &headers, set, request.reason).await
}

/// Removes `user` from workspace `name` (its owners).
pub async fn member_delete(
    State(state): State<AppState>,
    Path((name, user)): Path<(String, String)>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let reason = reason_parameter(&raw)?;
    let set = Change::SetMember {
        workspace: name,
        user,
        level: None,
    };
    change(&state, &headers, set, reason).await
}

/// The latest changes (`limit`, 100 by default), the latest first (administrators).
pub async fn history(
    State(state): State<AppState>,
    raw: RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let by = requester(&state, &headers).await?;
    administrator(&state.access().state(), &by)?;
    let limit = super::rdf4j::pairs(&raw)?
        .into_iter()
        .find(|(key, _)| key == "limit")
        .map(|(_, value)| {
            value
                .parse::<usize>()
                .map_err(|_| ApiError::bad_request("limit must be a number"))
        })
        .transpose()?
        .unwrap_or(100);
    let access = state.clone();
    let records: Vec<ChangeRecord> =
        tokio::task::spawn_blocking(move || access.access().history(limit))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(records).into_response())
}

/// Replaces the role rules and fallbacks with the policy file in the body (TOML) and
/// turns enforcement on (administrators); `reason` as a query parameter.
pub async fn import(
    State(state): State<AppState>,
    raw: RawQuery,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let reason = reason_parameter(&raw)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ApiError::bad_request("the policy must be UTF-8"))?;
    let policy: crate::access::AccessPolicy =
        toml::from_str(text).map_err(|error| ApiError::bad_request(error.to_string()))?;
    change(&state, &headers, Change::Import(policy), reason).await
}

/// The role rules and fallbacks as a policy file (TOML; administrators).
pub async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let by = requester(&state, &headers).await?;
    let access = state.access().state();
    administrator(&access, &by)?;
    let text =
        toml::to_string(&access.export()).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/toml")],
        text,
    )
        .into_response())
}
