//! Saved queries: queries (and updates) kept under a name in a space, a user's personal
//! space (`~alice`) or a workspace. Who may read the space reads its queries; its editors
//! and owners (and administrators) change them. They are kept in the system store's
//! [`QUERIES_GRAPH`], one resource each, and checked to parse before they are stored.
//!
//! ```turtle
//! <urn:nrese:query:project/top-authors> a nra:SavedQuery ; nra:space "project" ;
//!     nra:name "top-authors" ; nra:text "SELECT …" ; nra:title "Top authors" ;
//!     nra:repository "nrese" ; nra:author "alice" ;
//!     nra:time "2026-10-02T09:30:00Z"^^xsd:dateTime .
//! ```
//!
//! They are content, not policy: a change keeps its author and time, not an entry in the
//! access history.

use std::collections::BTreeMap;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use serde::{Deserialize, Serialize};

use super::rdf::{self, iri, term};
use super::{AccessError, AccessState, Level, Principal};
use crate::{NamespaceMap, SparqlUpdateRequest, StoreService};

/// The graph of the saved queries in the system store.
pub const QUERIES_GRAPH: &str = "urn:nrese:system:queries";
const QUERY: &str = "urn:nrese:query:";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// A saved query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SavedQuery {
    /// Its space: a personal space (`~alice`) or a workspace.
    pub space: String,
    /// Its name in the space: letters, digits, `.`, `_` and `-`.
    pub name: String,
    /// The SPARQL query or update.
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The repository it is meant for; absent: any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Who changed it last.
    pub author: String,
    /// When, UTC, as `xsd:dateTime`.
    pub time: String,
}

/// What a client sends to save a query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct QueryDraft {
    pub query: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

/// The saved queries, by space and name.
pub(super) type Queries = BTreeMap<(String, String), SavedQuery>;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `principal`'s level in `space`: owner for administrators, else its membership.
fn level(state: &AccessState, principal: &Principal, space: &str) -> Option<Level> {
    if state.is_admin(principal) {
        return Some(Level::Owner);
    }
    let user = principal.user.as_deref()?;
    state
        .memberships(user, None)
        .into_iter()
        .find(|(name, _)| name == space)
        .map(|(_, level)| level)
}

/// Fails unless `text` parses as a SPARQL query or update; a prefix it doesn't declare
/// means what `namespaces` bind it to.
fn check(text: &str, namespaces: &NamespaceMap) -> Result<(), AccessError> {
    if nrese_sparql::compat::parse_query(text, Some(namespaces)).is_ok() {
        return Ok(());
    }
    let mut parser = nrese_sparql_syntax::SparqlParser::new();
    for (prefix, namespace) in namespaces {
        parser = match parser
            .clone()
            .with_prefix(prefix.clone(), namespace.clone())
        {
            Ok(with) => with,
            Err(_) => parser,
        };
    }
    if parser.parse_update(text).is_ok() {
        return Ok(());
    }
    // The query's error: what a saved query mostly is.
    match nrese_sparql::compat::parse_query(text, Some(namespaces)) {
        Err(error) => Err(AccessError::Invalid(format!(
            "neither a SPARQL query nor an update: {error}"
        ))),
        Ok(_) => Ok(()),
    }
}

fn resource(space: &str, name: &str) -> NamedNode {
    iri(&format!("{QUERY}{space}/{name}"))
}

fn quads(query: &SavedQuery) -> Vec<Quad> {
    let graph = GraphName::NamedNode(iri(QUERIES_GRAPH));
    let subject = resource(&query.space, &query.name);
    let text = |value: &str| Term::Literal(Literal::new_simple_literal(value));
    let mut out = vec![
        (
            NamedNode::new_unchecked(RDF_TYPE),
            Term::NamedNode(term("SavedQuery")),
        ),
        (term("space"), text(&query.space)),
        (term("name"), text(&query.name)),
        (term("text"), text(&query.query)),
        (term("author"), text(&query.author)),
        (
            term("time"),
            Term::Literal(Literal::new_typed_literal(
                &query.time,
                NamedNode::new_unchecked(XSD_DATE_TIME),
            )),
        ),
    ];
    for (local, value) in [
        ("title", &query.title),
        ("description", &query.description),
        ("repository", &query.repository),
    ] {
        if let Some(value) = value {
            out.push((term(local), text(value)));
        }
    }
    out.into_iter()
        .map(|(predicate, object)| Quad::new(subject.clone(), predicate, object, graph.clone()))
        .collect()
}

/// The saved queries in the system store.
pub(super) fn load(store: &StoreService) -> Result<Queries, AccessError> {
    let quads = store
        .statements(
            &crate::ReadContext::all().infer(false),
            &crate::StatementPattern {
                contexts: vec![GraphName::NamedNode(iri(QUERIES_GRAPH))],
                ..crate::StatementPattern::default()
            },
        )
        .map_err(|error| AccessError::Store(error.to_string()))?;
    let mut fields: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for quad in quads {
        let Some(local) = quad.predicate.as_str().strip_prefix(rdf::NS) else {
            continue;
        };
        let Term::Literal(value) = &quad.object else {
            continue;
        };
        fields
            .entry(quad.subject.to_string())
            .or_default()
            .insert(local.to_owned(), value.value().to_owned());
    }
    let mut queries = Queries::new();
    for mut fields in fields.into_values() {
        let mut take = |key: &str| fields.remove(key);
        let (Some(space), Some(name), Some(text)) = (take("space"), take("name"), take("text"))
        else {
            continue;
        };
        let query = SavedQuery {
            space: space.clone(),
            name: name.clone(),
            query: text,
            title: take("title"),
            description: take("description"),
            repository: take("repository"),
            author: take("author").unwrap_or_default(),
            time: take("time").unwrap_or_default(),
        };
        queries.insert((space, name), query);
    }
    Ok(queries)
}

impl super::AccessControl {
    /// The saved queries `principal` may read, by space and name.
    pub fn saved_queries(&self, principal: &Principal) -> Vec<SavedQuery> {
        let state = self.state();
        self.queries
            .read()
            .expect("queries")
            .values()
            .filter(|query| level(&state, principal, &query.space).is_some())
            .cloned()
            .collect()
    }

    /// Saved query `name` in `space`, if `principal` may read it (else as if there were
    /// none).
    pub fn saved_query(
        &self,
        principal: &Principal,
        space: &str,
        name: &str,
    ) -> Result<SavedQuery, AccessError> {
        let missing = || AccessError::NotFound(format!("no saved query '{space}/{name}'"));
        if level(&self.state(), principal, space).is_none() {
            return Err(missing());
        }
        self.queries
            .read()
            .expect("queries")
            .get(&(space.to_owned(), name.to_owned()))
            .cloned()
            .ok_or_else(missing)
    }

    /// Saves `draft` as `name` in `space` (its editors and owners), replacing a query of
    /// that name; checked to parse first, a prefix it doesn't declare meaning what
    /// `namespaces` (its repository's) bind it to.
    pub fn save_query(
        &self,
        principal: &Principal,
        space: &str,
        name: &str,
        draft: QueryDraft,
        namespaces: &NamespaceMap,
    ) -> Result<SavedQuery, AccessError> {
        let state = self.state();
        let exists = match space.strip_prefix('~') {
            Some(user) => super::valid_user_name(user),
            None => state.workspaces.contains_key(space),
        };
        if !exists {
            return Err(AccessError::NotFound(format!("no space '{space}'")));
        }
        match level(&state, principal, space) {
            Some(Level::Editor | Level::Owner) => {}
            Some(Level::Viewer) => {
                return Err(AccessError::Forbidden(format!(
                    "only editors and owners of '{space}' save queries there"
                )));
            }
            None => {
                return Err(AccessError::Forbidden(format!(
                    "the requester isn't a member of '{space}'"
                )));
            }
        }
        if !valid_name(name) {
            return Err(AccessError::Invalid(format!(
                "'{name}' can't name a query (letters, digits, '.', '_', '-')"
            )));
        }
        check(&draft.query, namespaces)?;
        let blank = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
        let saved = SavedQuery {
            space: space.to_owned(),
            name: name.to_owned(),
            query: draft.query,
            title: blank(draft.title),
            description: blank(draft.description),
            repository: blank(draft.repository),
            author: principal.display(),
            time: super::control::now(),
        };
        let mut queries = self.queries.write().expect("queries");
        self.write_query(space, name, Some(&saved))?;
        queries.insert((space.to_owned(), name.to_owned()), saved.clone());
        Ok(saved)
    }

    /// Removes saved query `name` from `space` (its editors and owners).
    pub fn delete_query(
        &self,
        principal: &Principal,
        space: &str,
        name: &str,
    ) -> Result<(), AccessError> {
        let found = self.saved_query(principal, space, name)?;
        if !matches!(
            level(&self.state(), principal, &found.space),
            Some(Level::Editor | Level::Owner)
        ) {
            return Err(AccessError::Forbidden(format!(
                "only editors and owners of '{space}' remove its queries"
            )));
        }
        let mut queries = self.queries.write().expect("queries");
        self.write_query(space, name, None)?;
        queries.remove(&(space.to_owned(), name.to_owned()));
        Ok(())
    }

    /// Replaces query `space/name` in the system store by `query` (`None`: removes it).
    fn write_query(
        &self,
        space: &str,
        name: &str,
        query: Option<&SavedQuery>,
    ) -> Result<(), AccessError> {
        let mut update = format!(
            "DELETE WHERE {{ GRAPH <{QUERIES_GRAPH}> {{ {} ?p ?o }} }}",
            resource(space, name)
        );
        if let Some(query) = query {
            update.push_str(&format!(
                " ;\nINSERT DATA {{\n{}\n}}",
                super::control::data(&quads(query))
            ));
        }
        self.store
            .execute_update(&SparqlUpdateRequest::new(update))
            .map(drop)
            .map_err(|error| AccessError::Store(error.to_string()))
    }
}
