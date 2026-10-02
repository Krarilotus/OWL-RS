//! Graph-level access control: which graphs a request may read and write, from who it is.
//!
//! The rules live in the engine ([`nrese_store::access`], ADR-0008): role rules for
//! organisation-wide policy, users, workspaces with owners, editors and viewers, and every
//! user's personal space. The server maps a request's credentials to a [`Principal`]
//! ([`principal`]: its user name, its roles, whether it administers) and asks the state.
//! The state changes only through the engine API (`/api/v1/access/…`), each change with a
//! reason, kept with its history.
//!
//! A policy file (`auth.access_policy`, `NRESE_ACCESS_POLICY`) is the state's import
//! form: imported at the first start (it turns enforcement on), afterwards through
//! `POST /api/v1/access/import`; `GET /api/v1/access/export` writes it.
//!
//! ```toml
//! default = "deny"        # roles no rule names: nothing ("deny") or everything ("allow")
//! inferred = "hidden"     # inferred statements for users who may not read every graph
//!
//! [[role]]
//! name = "analyst"
//! read = ["https://kg.example/graphs/public/*", "https://kg.example/graphs/sales"]
//! write = ["https://kg.example/graphs/analyst/*"]
//! deny = ["https://kg.example/graphs/public/hr/*"]
//! default_graph = "read"  # "none" (the default), "read", "write" or "deny"
//! ```
//!
//! A user's rights are the union of its roles' rules and its workspaces'; what it may
//! write it may read. An explicit deny wins. Administrators are unrestricted, and so is
//! everyone while enforcement is off. Without authentication every request has the role
//! `anonymous`.
//!
//! The state grants as well as restricts: an authenticated user whose roles have rules,
//! or who has a user name (and so a personal space), may query and read graphs (in the
//! graphs it may read), and one who may write a graph may update, even without the
//! authentication's read or admin role.
//!
//! Reads see a dataset restricted to the readable graphs: the others are absent, not
//! forbidden ([`nrese_sparql::GraphAccess`]). Inferred statements live in the default
//! graph and are derived from statements in any graph: a user who may not read every
//! graph sees them only with `inferred = "visible"` (until the reasoner records which
//! graphs an inference came from). A write that changes a graph its user may not write is
//! refused as a whole.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub use nrese_store::access::{
    AccessError, AccessPolicy, AccessView, DefaultGraphRight, Fallback, Inferred, Principal,
    RoleRule,
};

use crate::auth::Identity;
use crate::error::ApiError;

/// Reads and checks a policy file.
pub fn load(path: &Path) -> Result<AccessPolicy> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("access policy {}", path.display()))?;
    let policy: AccessPolicy =
        toml::from_str(&text).with_context(|| format!("access policy {}", path.display()))?;
    let mut names = BTreeSet::new();
    for rule in &policy.roles {
        if rule.name.is_empty() || !names.insert(rule.name.as_str()) {
            bail!(
                "access policy {}: role names must be present and unique ('{}')",
                path.display(),
                rule.name
            );
        }
    }
    Ok(policy)
}

/// The principal a request's identity is.
pub fn principal(identity: &Identity) -> Principal {
    Principal {
        user: identity.user.clone(),
        roles: identity.roles.clone(),
        admin: identity.admin,
    }
}

impl From<AccessError> for ApiError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Forbidden(message) => ApiError::forbidden(message),
            AccessError::Invalid(message) => ApiError::bad_request(message),
            AccessError::NotFound(message) => ApiError::not_found(message),
            AccessError::Conflict(message) => ApiError::conflict(message),
            AccessError::Throttled(message) => ApiError::too_many_requests(message),
            AccessError::Store(message) => ApiError::internal(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"
        default = "deny"
        [[role]]
        name = "analyst"
        read = ["https://kg.example/public/*", "https://kg.example/sales"]
        write = ["https://kg.example/analyst/*"]
        default_graph = "read"
        [[role]]
        name = "editor"
        write = ["https://kg.example/public/*"]
    "#;

    #[test]
    fn policies_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.toml");
        std::fs::write(&path, "[[role]]\nname = \"a\"\n[[role]]\nname = \"a\"\n").unwrap();
        assert!(load(&path).is_err());
        std::fs::write(&path, "[[role]]\nname = \"a\"\nreed = []\n").unwrap();
        assert!(load(&path).is_err());
        std::fs::write(&path, POLICY).unwrap();
        assert_eq!(load(&path).unwrap().roles.len(), 2);
    }
}
