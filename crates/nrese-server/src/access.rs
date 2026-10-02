//! Graph-level access control (O1): which named graphs a user may read and write, from the
//! roles its credentials carry.
//!
//! A policy file (`auth.access_policy`, `NRESE_ACCESS_POLICY`) names, per role, the graphs
//! it may read and write, by IRI or by IRI prefix (an entry ending in `*`), and whether the
//! store's default graph:
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
//! A user's rights are the union of its roles' rules; what it may write it may read. An
//! explicit deny wins: a graph any of its roles denies it may neither read nor write.
//! Administrators are unrestricted, and so is everyone without a policy. Without
//! authentication every request has the role `anonymous`.
//!
//! The policy grants as well as restricts: an authenticated user whose roles have rules may
//! query and read graphs (in the graphs it may read), and one whose roles may write a graph
//! may update, even without the authentication's read or admin role. So the policy alone
//! makes a role an editor of some graphs, without making it an administrator.
//!
//! Reads see a dataset restricted to the readable graphs: the others are absent, not
//! forbidden ([`nrese_sparql::GraphAccess`]). Inferred statements live in the default
//! graph and are derived from statements in any graph: a user who may not read every
//! graph sees them only with `inferred = "visible"` (until the reasoner records which
//! graphs an inference came from). A write that changes a graph its user may not write is
//! refused as a whole.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use nrese_rdf::GraphName;
use nrese_sparql::GraphAccess;
use serde::Deserialize;

use crate::auth::Identity;
use crate::policy::PolicyAction;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    /// A user no rule names may read and write nothing.
    #[default]
    Deny,
    /// A user no rule names is unrestricted.
    Allow,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Inferred {
    /// Hidden from users who may not read every graph.
    #[default]
    Hidden,
    /// Visible to everyone who may read the default graph.
    Visible,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultGraphRight {
    #[default]
    None,
    Read,
    Write,
    /// Neither, whatever other roles grant.
    Deny,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleRule {
    pub name: String,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    /// Graphs (IRIs, or prefixes ending in `*`) the role's users may neither read nor
    /// write, whatever their other roles grant.
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub default_graph: DefaultGraphRight,
}

/// The access policy: rules per role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicy {
    #[serde(default)]
    pub default: Fallback,
    #[serde(default)]
    pub inferred: Inferred,
    #[serde(default, rename = "role")]
    pub roles: Vec<RoleRule>,
}

/// Adds a policy entry to `set`: an IRI, or an IRI prefix ending in `*`.
fn add(set: &mut GraphAccess, entry: &str) {
    match entry.strip_suffix('*') {
        Some(prefix) => set.prefixes.push(prefix.to_owned()),
        None => set.graphs.push(entry.to_owned()),
    }
}

/// Excludes a policy entry from `set`, whatever else it holds.
fn exclude(set: &mut GraphAccess, entry: &str) {
    match entry.strip_suffix('*') {
        Some(prefix) => set.excluded_prefixes.push(prefix.to_owned()),
        None => set.excluded.push(entry.to_owned()),
    }
}

/// What one request may read and write: `None` is unrestricted.
#[derive(Debug, Clone, Default)]
pub struct AccessView {
    pub read: Option<Arc<GraphAccess>>,
    pub write: Option<Arc<GraphAccess>>,
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
        self.write.as_ref().is_none_or(|set| set.allows_graph(graph))
    }

    /// Whether the requester sees the inferred statements.
    pub fn sees_inferred(&self) -> bool {
        self.read.as_ref().is_none_or(|set| set.inferred)
    }

    /// Whether the user may read every graph (and the inferred statements).
    pub fn reads_everything(&self) -> bool {
        self.read.is_none()
    }
}

impl AccessPolicy {
    /// Reads and checks a policy file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("access policy {}", path.display()))?;
        let policy: Self =
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

    /// Whether the policy lets `identity` do `action` (beyond its credentials' grants):
    /// reads with any rule for one of its roles, data writes with a rule that writes.
    pub fn allows(&self, identity: &Identity, action: PolicyAction) -> bool {
        let mut rules = self
            .roles
            .iter()
            .filter(|rule| identity.roles.contains(&rule.name));
        match action {
            PolicyAction::QueryRead
            | PolicyAction::GraphRead
            | PolicyAction::ServiceDescriptionRead => rules.next().is_some(),
            PolicyAction::UpdateWrite | PolicyAction::GraphWrite | PolicyAction::TellWrite => {
                rules.any(|rule| {
                    !rule.write.is_empty() || rule.default_graph == DefaultGraphRight::Write
                })
            }
            PolicyAction::OperatorRead | PolicyAction::AdminWrite | PolicyAction::MetricsRead => {
                false
            }
        }
    }

    /// What `identity` may read and write.
    pub fn view(&self, identity: &Identity) -> AccessView {
        if identity.admin {
            return AccessView::unrestricted();
        }
        let rules: Vec<&RoleRule> = self
            .roles
            .iter()
            .filter(|rule| identity.roles.contains(&rule.name))
            .collect();
        if rules.is_empty() && self.default == Fallback::Allow {
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
        if rules
            .iter()
            .any(|rule| rule.default_graph == DefaultGraphRight::Deny)
        {
            read.default_graph = false;
            write.default_graph = false;
        }
        read.inferred = read.default_graph && self.inferred == Inferred::Visible;
        AccessView {
            read: Some(Arc::new(read)),
            write: Some(Arc::new(write)),
        }
    }
}

#[cfg(test)]
mod tests {
    use nrese_rdf::NamedNode;

    use super::*;

    fn identity(roles: &[&str]) -> Identity {
        Identity {
            admin: false,
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

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

    fn named(iri: &str) -> GraphName {
        GraphName::NamedNode(NamedNode::new_unchecked(iri))
    }

    #[test]
    fn roles_union_their_rules() {
        let policy: AccessPolicy = toml::from_str(POLICY).unwrap();
        let analyst = policy.view(&identity(&["analyst"]));
        assert!(analyst.can_read(&named("https://kg.example/public/x")));
        assert!(analyst.can_read(&named("https://kg.example/sales")));
        assert!(!analyst.can_read(&named("https://kg.example/sales/2")));
        assert!(
            analyst.can_read(&named("https://kg.example/analyst/notes")),
            "writable is readable"
        );
        assert!(analyst.can_read(&GraphName::DefaultGraph));
        assert!(!analyst.can_write(&GraphName::DefaultGraph));
        assert!(!analyst.can_write(&named("https://kg.example/public/x")));
        assert!(!analyst.reads_everything());
        // Inferred statements stay hidden by default.
        assert!(!analyst.read.as_ref().unwrap().inferred);
        let both = policy.view(&identity(&["analyst", "editor"]));
        assert!(both.can_write(&named("https://kg.example/public/x")));
        // No rule: nothing under "deny", everything under "allow" and for administrators.
        let stranger = policy.view(&identity(&["guest"]));
        assert!(!stranger.can_read(&GraphName::DefaultGraph));
        assert!(!stranger.can_read(&named("https://kg.example/public/x")));
        let open = AccessPolicy {
            default: Fallback::Allow,
            ..policy.clone()
        };
        assert!(open.view(&identity(&["guest"])).reads_everything());
        let admin = Identity {
            admin: true,
            roles: BTreeSet::new(),
        };
        assert!(policy.view(&admin).reads_everything());
    }

    #[test]
    fn the_policy_grants_reads_and_writes_to_its_roles() {
        let policy: AccessPolicy = toml::from_str(POLICY).unwrap();
        let analyst = identity(&["analyst"]);
        assert!(policy.allows(&analyst, PolicyAction::QueryRead));
        assert!(policy.allows(&analyst, PolicyAction::UpdateWrite));
        assert!(!policy.allows(&analyst, PolicyAction::AdminWrite));
        let reader: AccessPolicy =
            toml::from_str("[[role]]\nname = \"r\"\nread = [\"https://kg.example/*\"]\n").unwrap();
        assert!(reader.allows(&identity(&["r"]), PolicyAction::GraphRead));
        assert!(!reader.allows(&identity(&["r"]), PolicyAction::GraphWrite));
        assert!(!reader.allows(&identity(&["other"]), PolicyAction::QueryRead));
    }

    #[test]
    fn an_explicit_deny_wins() {
        let policy: AccessPolicy = toml::from_str(&format!(
            "{POLICY}\n[[role]]\nname = \"contractor\"\ndeny = [\"https://kg.example/public/hr/*\"]\ndefault_graph = \"deny\"\n"
        ))
        .unwrap();
        let view = policy.view(&identity(&["analyst", "editor", "contractor"]));
        assert!(view.can_read(&named("https://kg.example/public/x")));
        assert!(!view.can_read(&named("https://kg.example/public/hr/salaries")));
        assert!(!view.can_write(&named("https://kg.example/public/hr/salaries")));
        assert!(view.can_write(&named("https://kg.example/public/y")));
        assert!(!view.can_read(&GraphName::DefaultGraph));
    }

    #[test]
    fn inferred_statements_when_the_policy_shows_them() {
        let policy: AccessPolicy =
            toml::from_str(&format!("inferred = \"visible\"\n{POLICY}")).unwrap();
        assert!(policy.view(&identity(&["analyst"])).read.unwrap().inferred);
        // Not without the default graph, where they are.
        assert!(!policy.view(&identity(&["editor"])).read.unwrap().inferred);
    }

    #[test]
    fn policies_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.toml");
        std::fs::write(&path, "[[role]]\nname = \"a\"\n[[role]]\nname = \"a\"\n").unwrap();
        assert!(AccessPolicy::load(&path).is_err());
        std::fs::write(&path, "[[role]]\nname = \"a\"\nreed = []\n").unwrap();
        assert!(AccessPolicy::load(&path).is_err());
        std::fs::write(&path, POLICY).unwrap();
        assert_eq!(AccessPolicy::load(&path).unwrap().roles.len(), 2);
    }
}
