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
//!   keeps the server's reasoning;
//! - user rules: a `ruleset` that is the path of a GraphDB ruleset file (`….pie`, read on
//!   the server: GraphDB's custom rulesets), or rules in the configuration itself
//!   (`nrc:rules`, Notation3 unless `nrc:rulesFormat "pie"`, with `nrc:` =
//!   `https://nrese.dev/ns/config#`). A `.pie` file is the whole program (mode `custom`);
//!   inline rules add to the ruleset named beside them, or are the whole program without
//!   one. They are checked when the repository is created and kept with its settings.
//!
//! The repository id it names (`rep.id`, `repositoryID`) must be the one in the path.
//! Everything else (the store type, its persistence and indexes) is NRESE's own: a
//! repository is stored as the server stores its default one.

use nrese_rdf::{Quad, Term};
use nrese_reasoner::ReasoningMode;

pub use nrese_store::catalog::{RepositoryRules, RepositorySettings};

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// User rules in a repository's configuration (NRESE's own vocabulary).
const CONFIG_RULES: &str = "https://nrese.dev/ns/config#rules";
const CONFIG_RULES_FORMAT: &str = "https://nrese.dev/ns/config#rulesFormat";

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
        // `custom` needs rules: a `.pie` file or rules in the configuration.
        _ => ReasoningMode::from_name(name).filter(|mode| *mode != ReasoningMode::Custom),
    }
}

/// The settings `quads` (a repository configuration) give repository `id`.
pub fn from_config(id: &str, quads: &[Quad]) -> Result<RepositorySettings, String> {
    let mut settings = RepositorySettings::default();
    let mut repository = None;
    let mut sail_types = Vec::new();
    let mut ruleset = None;
    let (mut inline_rules, mut rules_format) = (None, None);
    for quad in quads {
        let Some(value) = text(&quad.object) else {
            continue;
        };
        match quad.predicate.as_str() {
            CONFIG_RULES => inline_rules = Some(value),
            CONFIG_RULES_FORMAT => rules_format = Some(value),
            _ => {}
        }
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
    // A GraphDB ruleset file: the whole program.
    if let Some(path) = ruleset.filter(|name| name.trim().to_ascii_lowercase().ends_with(".pie")) {
        let path = std::path::Path::new(path.trim());
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("ruleset {}: {error}", path.display()))?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        settings.rules = Some(RepositoryRules {
            name,
            format: "pie".to_owned(),
            text,
        });
        settings.reasoning = Some(ReasoningMode::Custom.as_str().to_owned());
        settings.reasoner_config()?;
        return Ok(settings);
    }
    if let Some(text) = inline_rules {
        let format = match rules_format.map(str::trim) {
            None | Some("n3") => "n3",
            Some("pie") => "pie",
            Some(other) => return Err(format!("rules format '{other}' isn't n3 or pie")),
        };
        settings.rules = Some(RepositoryRules {
            name: "configuration".to_owned(),
            format: format.to_owned(),
            text: text.to_owned(),
        });
    }
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
    // Rules alone are the whole program; rules with a disabled ruleset make no sense.
    let mode = match (mode, &settings.rules) {
        (None, Some(_)) => Some(ReasoningMode::Custom),
        (Some(ReasoningMode::Disabled), Some(_)) if ruleset.is_none() => {
            Some(ReasoningMode::Custom)
        }
        (mode, _) => mode,
    };
    settings.reasoning = mode.map(|mode| mode.as_str().to_owned());
    // Checked now: a mistake in the rules fails the creation, with the rule it is in.
    settings.reasoner_config()?;
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

    #[test]
    fn rules_in_the_configuration_and_from_a_ruleset_file() {
        let rule =
            "@prefix ex: <http://example.com/> .\n{ ?x ex:parent ?y } => { ?y ex:child ?x } .";
        let inline = vec![quad("r", CONFIG_RULES, literal(rule))];
        let settings = from_config("x", &inline).unwrap();
        assert_eq!(settings.reasoning_mode(), Some(ReasoningMode::Custom));
        assert!(settings.reasoner_config().unwrap().unwrap().rules.is_some());
        let with_ruleset = vec![
            quad("r", CONFIG_RULES, literal(rule)),
            quad(
                "s",
                "http://www.ontotext.com/config/graphdb#ruleset",
                literal("rdfs"),
            ),
        ];
        assert_eq!(
            from_config("x", &with_ruleset).unwrap().reasoning_mode(),
            Some(ReasoningMode::Rdfs)
        );
        let broken = vec![quad("r", CONFIG_RULES, literal("{ ?x } => "))];
        assert!(from_config("x", &broken).is_err());
        let wrong_format = vec![
            quad("r", CONFIG_RULES, literal(rule)),
            quad("r", CONFIG_RULES_FORMAT, literal("swrl")),
        ];
        assert!(from_config("x", &wrong_format).is_err());
        // A .pie file is the whole program, its text kept with the settings.
        let dir = tempfile::tempdir().unwrap();
        let pie = dir.path().join("family.pie");
        std::fs::write(
            &pie,
            "Prefices\n{\n  ex : http://example.com/\n}\nAxioms\n{\n}\nRules\n{\nId: child\n  x <ex:parent> y\n  ------------------------------------\n  y <ex:child> x\n}\n",
        )
        .unwrap();
        let file = vec![quad(
            "s",
            "http://www.ontotext.com/config/graphdb#ruleset",
            literal(pie.to_str().unwrap()),
        )];
        let settings = from_config("x", &file).unwrap();
        assert_eq!(settings.reasoning_mode(), Some(ReasoningMode::Custom));
        assert_eq!(settings.rules.as_ref().unwrap().format, "pie");
        let missing = vec![quad(
            "s",
            "http://www.ontotext.com/config/graphdb#ruleset",
            literal("/no/such/file.pie"),
        )];
        assert!(from_config("x", &missing).is_err());
    }
}
