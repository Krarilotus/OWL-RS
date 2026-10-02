//! The access state as RDF, in the system store's state graph: one resource per role,
//! user and workspace, in the vocabulary `https://nrese.dev/ns/access#`, so that
//! administrators can query it (and its history) with SPARQL.
//!
//! ```turtle
//! <urn:nrese:access:settings> nra:enforced true ; nra:fallback "deny" ; nra:inferred "hidden" ;
//!     nra:usersCreateWorkspaces true .
//! <urn:nrese:role:analyst> a nra:Role ; nra:name "analyst" ; nra:read "https://kg.example/public/*" ;
//!     nra:defaultGraph "read" .
//! <urn:nrese:user:alice> a nra:User ; nra:name "alice" ; nra:admin false ; nra:role "analyst" .
//! <urn:nrese:workspace:project> a nra:Workspace ; nra:name "project" ; nra:title "Project" ;
//!     nra:owner <urn:nrese:user:alice> ; nra:viewer <urn:nrese:user:bob> .
//! ```

use std::collections::BTreeMap;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

use super::{AccessState, DefaultGraphRight, Fallback, Inferred, Level, RoleRule, encode_name};

pub(super) const NS: &str = "https://nrese.dev/ns/access#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const SETTINGS: &str = "urn:nrese:access:settings";
const ROLE: &str = "urn:nrese:role:";
const USER: &str = "urn:nrese:user:";
const WORKSPACE: &str = "urn:nrese:workspace:";

pub(super) fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

pub(super) fn term(local: &str) -> NamedNode {
    iri(&format!("{NS}{local}"))
}

fn text(value: &str) -> Term {
    Literal::new_simple_literal(value).into()
}

fn boolean(value: bool) -> Term {
    Literal::new_typed_literal(value.to_string(), iri(XSD_BOOLEAN)).into()
}

fn user_iri(name: &str) -> NamedNode {
    iri(&format!("{USER}{}", encode_name(name)))
}

/// `text` with percent-encoded bytes decoded.
fn decode_name(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn default_graph_name(right: DefaultGraphRight) -> &'static str {
    match right {
        DefaultGraphRight::None => "none",
        DefaultGraphRight::Read => "read",
        DefaultGraphRight::Write => "write",
        DefaultGraphRight::Deny => "deny",
    }
}

/// The state's statements, in `graph`.
pub(super) fn encode(state: &AccessState, graph: &NamedNode) -> Vec<Quad> {
    let graph = GraphName::NamedNode(graph.clone());
    let mut quads = Vec::new();
    let mut push = |subject: &NamedNode, predicate: &str, object: Term| {
        let predicate = match predicate {
            "a" => iri(RDF_TYPE),
            local => term(local),
        };
        quads.push(Quad::new(subject.clone(), predicate, object, graph.clone()));
    };
    let settings = iri(SETTINGS);
    let s = &state.settings;
    push(&settings, "enforced", boolean(s.enforced));
    push(
        &settings,
        "fallback",
        text(match s.fallback {
            Fallback::Deny => "deny",
            Fallback::Allow => "allow",
        }),
    );
    push(
        &settings,
        "inferred",
        text(match s.inferred {
            Inferred::Hidden => "hidden",
            Inferred::Visible => "visible",
        }),
    );
    push(
        &settings,
        "usersCreateWorkspaces",
        boolean(s.users_create_workspaces),
    );
    let integer = |value: u32| -> Term {
        Literal::new_typed_literal(value.to_string(), iri(XSD_INTEGER)).into()
    };
    push(
        &settings,
        "minPasswordLength",
        integer(s.min_password_length),
    );
    push(&settings, "sessionHours", integer(s.session_hours));
    for rule in state.roles.values() {
        let subject = iri(&format!("{ROLE}{}", encode_name(&rule.name)));
        push(&subject, "a", term("Role").into());
        push(&subject, "name", text(&rule.name));
        for (predicate, entries) in [
            ("read", &rule.read),
            ("write", &rule.write),
            ("deny", &rule.deny),
        ] {
            for entry in entries {
                push(&subject, predicate, text(entry));
            }
        }
        push(
            &subject,
            "defaultGraph",
            text(default_graph_name(rule.default_graph)),
        );
        if rule.service {
            push(&subject, "service", boolean(true));
        }
    }
    for (name, user) in &state.users {
        let subject = user_iri(name);
        push(&subject, "a", term("User").into());
        push(&subject, "name", text(name));
        push(&subject, "admin", boolean(user.admin));
        for role in &user.roles {
            push(&subject, "role", text(role));
        }
        if let Some(hash) = &user.password_hash {
            push(&subject, "passwordHash", text(hash));
        }
    }
    for (name, workspace) in &state.workspaces {
        let subject = iri(&format!("{WORKSPACE}{}", encode_name(name)));
        push(&subject, "a", term("Workspace").into());
        push(&subject, "name", text(name));
        if let Some(title) = &workspace.title {
            push(&subject, "title", text(title));
        }
        if let Some(repository) = &workspace.repository {
            push(&subject, "repository", text(repository));
        }
        for entry in &workspace.graphs {
            push(&subject, "graph", text(entry));
        }
        for (member, level) in &workspace.members {
            push(&subject, level.name(), user_iri(member).into());
        }
    }
    quads
}

/// The state the statements `quads` describe (any graph), with prefixes under `base`.
pub(super) fn decode(quads: &[Quad], base: &str) -> AccessState {
    let mut state = AccessState::new(base);
    // Statements per subject: (predicate local name, object).
    let mut subjects: BTreeMap<String, Vec<(String, Term)>> = BTreeMap::new();
    for quad in quads {
        let nrese_rdf::NamedOrBlankNode::NamedNode(subject) = &quad.subject else {
            continue;
        };
        let predicate = quad.predicate.as_str();
        let local = match predicate.strip_prefix(NS) {
            Some(local) => local.to_owned(),
            None if predicate == RDF_TYPE => "a".to_owned(),
            None => continue,
        };
        subjects
            .entry(subject.as_str().to_owned())
            .or_default()
            .push((local, quad.object.clone()));
    }
    let value = |object: &Term| -> String {
        match object {
            Term::Literal(literal) => literal.value().to_owned(),
            Term::NamedNode(node) => node.as_str().to_owned(),
            other => other.to_string(),
        }
    };
    for (subject, statements) in &subjects {
        let get = |local: &str| -> Option<String> {
            statements
                .iter()
                .find(|(p, _)| p == local)
                .map(|(_, o)| value(o))
        };
        let all = |local: &str| -> Vec<String> {
            statements
                .iter()
                .filter(|(p, _)| p == local)
                .map(|(_, o)| value(o))
                .collect()
        };
        let flag = |local: &str| get(local).is_some_and(|v| v == "true" || v == "1");
        if subject == SETTINGS {
            let settings = &mut state.settings;
            settings.enforced = flag("enforced");
            settings.fallback = match get("fallback").as_deref() {
                Some("allow") => Fallback::Allow,
                _ => Fallback::Deny,
            };
            settings.inferred = match get("inferred").as_deref() {
                Some("visible") => Inferred::Visible,
                _ => Inferred::Hidden,
            };
            settings.users_create_workspaces =
                get("usersCreateWorkspaces").is_none_or(|v| v == "true" || v == "1");
            let defaults = super::Settings::default();
            settings.min_password_length = get("minPasswordLength")
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.min_password_length);
            settings.session_hours = get("sessionHours")
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.session_hours);
            continue;
        }
        let Some(name) = get("name") else {
            continue;
        };
        if subject.starts_with(ROLE) {
            let default_graph = match get("defaultGraph").as_deref() {
                Some("read") => DefaultGraphRight::Read,
                Some("write") => DefaultGraphRight::Write,
                Some("deny") => DefaultGraphRight::Deny,
                _ => DefaultGraphRight::None,
            };
            state.roles.insert(
                name.clone(),
                RoleRule {
                    name,
                    read: all("read"),
                    write: all("write"),
                    deny: all("deny"),
                    default_graph,
                    service: flag("service"),
                },
            );
        } else if subject.starts_with(USER) {
            let user = state.users.entry(name).or_default();
            user.admin = flag("admin");
            user.roles = all("role").into_iter().collect();
            user.password_hash = get("passwordHash");
        } else if subject.starts_with(WORKSPACE) {
            let workspace = state.workspaces.entry(name).or_default();
            workspace.title = get("title");
            workspace.repository = get("repository");
            workspace.graphs = all("graph");
            for level in [Level::Viewer, Level::Editor, Level::Owner] {
                for member in all(level.name()) {
                    if let Some(user) = member.strip_prefix(USER) {
                        workspace.members.insert(decode_name(user), level);
                    }
                }
            }
        }
    }
    // Statements are a set: entries in the state's order ([`super::normalise`]).
    for rule in state.roles.values_mut() {
        super::normalise(&mut rule.read);
        super::normalise(&mut rule.write);
        super::normalise(&mut rule.deny);
    }
    for workspace in state.workspaces.values_mut() {
        super::normalise(&mut workspace.graphs);
    }
    state
}
