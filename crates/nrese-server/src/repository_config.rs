//! A repository's settings from the configuration RDF4J and GraphDB clients send with
//! `PUT /repositories/{id}` (RDF4J's `RepositoryConfig` as RDF, in either of its
//! vocabularies: `tag:rdf4j.org,2023:config/` and the older `http://www.openrdf.org/config/…`;
//! GraphDB's `graphdb:` / `owlim:` sail settings).
//!
//! Read from it:
//! - the title: the repository's `rdfs:label`;
//! - the reasoning: GraphDB's `ruleset` (`empty`, `rdfs`, `rdfsplus`, `owl-horst`,
//!   `owl2-ql`, `owl2-rl`, each also `-optimized`; or an NRESE reasoning mode's name), else
//!   from the sail stack: an RDFS inferencer (`…RDFSInferencer`) reasons with RDFS, a stack
//!   without one doesn't reason. A configuration that names neither (or no configuration)
//!   keeps the server's reasoning.
//!
//! The repository id it names (`rep.id`, `repositoryID`) must be the one in the path.
//! Everything else (the store type, its persistence and indexes) is NRESE's own: a
//! repository is stored as the server stores its default one.

use nrese_rdf::{Quad, Term};
use nrese_reasoner::ReasoningMode;
use serde::{Deserialize, Serialize};

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// What a repository is created with besides the server's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySettings {
    /// Shown in the repository list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The repository's reasoning, by mode name; the server's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl RepositorySettings {
    /// The reasoning mode, if the settings choose one.
    pub fn reasoning_mode(&self) -> Option<ReasoningMode> {
        self.reasoning.as_deref().and_then(ReasoningMode::from_name)
    }
}

/// The part of a configuration IRI after its namespace: `rep.id`, `repositoryID`.
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/', ':']).next().unwrap_or(iri)
}

fn text(term: &Term) -> Option<&str> {
    match term {
        Term::Literal(literal) => Some(literal.value()),
        Term::NamedNode(node) => Some(node.as_str()),
        _ => None,
    }
}

/// The reasoning mode of a GraphDB ruleset name (or an NRESE mode's name).
fn ruleset_mode(name: &str) -> Option<ReasoningMode> {
    let name = name.trim().to_ascii_lowercase();
    let name = name.strip_suffix("-optimized").unwrap_or(&name);
    match name {
        "empty" => Some(ReasoningMode::Disabled),
        "rdfs" => Some(ReasoningMode::Rdfs),
        "rdfsplus" => Some(ReasoningMode::RdfsPlus),
        "owl-horst" => Some(ReasoningMode::OwlHorst),
        "owl2-ql" => Some(ReasoningMode::Owl2Ql),
        "owl2-rl" => Some(ReasoningMode::Owl2Rl),
        // User rules come with the server's configuration only.
        _ => ReasoningMode::from_name(name).filter(|mode| *mode != ReasoningMode::Custom),
    }
}

/// The settings `quads` (a repository configuration) give repository `id`.
pub fn from_config(id: &str, quads: &[Quad]) -> Result<RepositorySettings, String> {
    let mut settings = RepositorySettings::default();
    let mut repository = None;
    let mut sail_types = Vec::new();
    let mut ruleset = None;
    for quad in quads {
        let Some(value) = text(&quad.object) else {
            continue;
        };
        match local_name(quad.predicate.as_str()) {
            "rep.id" | "repositoryID" => {
                if value != id {
                    return Err(format!(
                        "the configuration is for repository '{value}', not '{id}'"
                    ));
                }
                repository = Some(&quad.subject);
            }
            "sail.type" | "sailType" => sail_types.push(value),
            "ruleset" => ruleset = Some(value),
            _ => {}
        }
    }
    // The repository's label, else the first label.
    let labels = quads
        .iter()
        .filter(|quad| quad.predicate.as_str() == RDFS_LABEL);
    settings.title = labels
        .clone()
        .find(|quad| Some(&quad.subject) == repository)
        .or_else(|| labels.clone().next())
        .and_then(|quad| match &quad.object {
            Term::Literal(literal) => Some(literal.value().to_owned()),
            _ => None,
        });
    let mode = match ruleset {
        Some(name) => Some(ruleset_mode(name).ok_or_else(|| {
            format!(
                "ruleset '{name}' isn't supported (empty, rdfs, rdfsplus, owl-horst, owl2-ql, owl2-rl, or an NRESE reasoning mode)"
            )
        })?),
        None if sail_types.is_empty() => None,
        None if sail_types
            .iter()
            .any(|sail| local_name(sail).ends_with("RDFSInferencer")) =>
        {
            Some(ReasoningMode::Rdfs)
        }
        None => Some(ReasoningMode::Disabled),
    };
    settings.reasoning = mode.map(|mode| mode.as_str().to_owned());
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use nrese_rdf::{BlankNode, GraphName};
    use nrese_rdf::{Literal, NamedNode, Quad};

    use super::*;

    fn quad(subject: &str, predicate: &str, object: Term) -> Quad {
        Quad::new(
            BlankNode::new_unchecked(subject),
            NamedNode::new_unchecked(predicate),
            object,
            GraphName::DefaultGraph,
        )
    }

    fn literal(value: &str) -> Term {
        Literal::new_simple_literal(value).into()
    }

    const CONFIG: &str = "tag:rdf4j.org,2023:config/";

    #[test]
    fn rdf4j_memory_store_with_rdfs() {
        let quads = vec![
            quad("r", &format!("{CONFIG}rep.id"), literal("bench")),
            quad("r", RDFS_LABEL, literal("Bench data")),
            quad("other", RDFS_LABEL, literal("not this")),
            quad(
                "s",
                &format!("{CONFIG}sail.type"),
                literal("rdf4j:SchemaCachingRDFSInferencer"),
            ),
            quad(
                "d",
                &format!("{CONFIG}sail.type"),
                literal("openrdf:MemoryStore"),
            ),
        ];
        let settings = from_config("bench", &quads).unwrap();
        assert_eq!(settings.title.as_deref(), Some("Bench data"));
        assert_eq!(settings.reasoning_mode(), Some(ReasoningMode::Rdfs));
        assert!(from_config("other", &quads).is_err());
    }

    #[test]
    fn plain_stores_and_graphdb_rulesets() {
        let old = "http://www.openrdf.org/config/sail#sailType";
        let plain = vec![quad("s", old, literal("openrdf:NativeStore"))];
        assert_eq!(
            from_config("x", &plain).unwrap().reasoning_mode(),
            Some(ReasoningMode::Disabled)
        );
        let graphdb = vec![
            quad("s", old, literal("graphdb:Sail")),
            quad(
                "s",
                "http://www.ontotext.com/config/graphdb#ruleset",
                literal("owl2-rl-optimized"),
            ),
        ];
        assert_eq!(
            from_config("x", &graphdb).unwrap().reasoning_mode(),
            Some(ReasoningMode::Owl2Rl)
        );
        let max = vec![quad(
            "s",
            "http://www.ontotext.com/trree/owlim#ruleset",
            literal("owl-max"),
        )];
        assert!(from_config("x", &max).is_err());
        assert_eq!(
            from_config("x", &[]).unwrap(),
            RepositorySettings::default()
        );
    }
}
