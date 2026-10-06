//! The W3C OWL 2 test cases as the store's runners read them (`all.rdf` from
//! scripts/fetch-w3c-tests.sh, or the file `NRESE_W3C_OWL_TESTS` points to): shared by
//! the RL and QL runners.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use nrese_rdf::{NamedOrBlankNode, Term, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

pub const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

pub fn suite_path() -> PathBuf {
    std::env::var_os("NRESE_W3C_OWL_TESTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/owl-test/all.rdf"),
        PathBuf::from,
    )
}

/// RDF/XML with its entity declarations in double quotes: the test cases write
/// `<!ENTITY owl 'http://…'>`, which the parser doesn't take.
pub fn quoted_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!ENTITY") {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let end = from.find('>').map_or(from.len(), |e| e + 1);
        let declaration = &from[..end];
        if declaration.contains('"') {
            out.push_str(declaration);
        } else {
            out.push_str(&declaration.replace('\'', "\""));
        }
        rest = &from[end..];
    }
    out.push_str(rest);
    out
}

pub fn parse_rdf_xml(text: &str) -> Result<Vec<Triple>, String> {
    let text = quoted_entities(text);
    RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2007/OWL/test-base/")
        .map_err(|e| e.to_string())?
        .for_reader(text.as_bytes())
        .map(|quad| quad.map(Triple::from).map_err(|e| e.to_string()))
        .collect()
}

/// One test case: its kinds (the local names of its types) and ontologies.
#[derive(Default)]
pub struct Case {
    pub kinds: BTreeSet<String>,
    pub premise: Option<String>,
    pub conclusion: Option<String>,
    pub non_conclusion: Option<String>,
    pub imports: bool,
    /// The profiles it is in (`EL`, `QL`, `RL`).
    pub profiles: BTreeSet<String>,
    /// The semantics it holds under (`DIRECT`, `RDF-BASED`).
    pub semantics: BTreeSet<String>,
    pub rejected: bool,
}

pub fn cases(triples: &[Triple]) -> BTreeMap<String, Case> {
    let mut by_node: HashMap<NamedOrBlankNode, Case> = HashMap::new();
    let mut names: HashMap<NamedOrBlankNode, String> = HashMap::new();
    for t in triples {
        let Some(local) = t.predicate.as_str().strip_prefix(TEST) else {
            if t.predicate.as_str() == RDF_TYPE
                && let Term::NamedNode(class) = &t.object
                && let Some(kind) = class.as_str().strip_prefix(TEST)
            {
                by_node
                    .entry(t.subject.clone())
                    .or_default()
                    .kinds
                    .insert(kind.to_owned());
            }
            continue;
        };
        let case = by_node.entry(t.subject.clone()).or_default();
        let text = || match &t.object {
            Term::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        let object = |name: &str| matches!(&t.object, Term::NamedNode(n) if n.as_str() == format!("{TEST}{name}"));
        match local {
            "identifier" => {
                if let Some(name) = text() {
                    names.insert(t.subject.clone(), name);
                }
            }
            "rdfXmlPremiseOntology" => case.premise = text(),
            "rdfXmlConclusionOntology" => case.conclusion = text(),
            "rdfXmlNonConclusionOntology" => case.non_conclusion = text(),
            "importedOntology" => case.imports = true,
            "profile" | "semantics" => {
                if let Term::NamedNode(n) = &t.object
                    && let Some(name) = n.as_str().strip_prefix(TEST)
                {
                    let set = if local == "profile" {
                        &mut case.profiles
                    } else {
                        &mut case.semantics
                    };
                    set.insert(name.to_owned());
                }
            }
            "status" => case.rejected |= object("Rejected") || object("Extracredit"),
            _ => {}
        }
    }
    by_node
        .into_iter()
        .filter_map(|(node, case)| Some((names.get(&node)?.clone(), case)))
        .collect()
}
