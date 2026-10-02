//! Reads W3C test manifests (`mf:`/`qt:`/`ut:` vocabularies) into [`Test`]s.
//!
//! Manifests are parsed with their *official* base IRI, so every file reference and graph
//! name is the IRI the expected results were written against. [`Suite::local_path`] maps
//! those IRIs to the local checkout.

use std::path::{Path, PathBuf};

use nrese_rdf::{Graph, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Term, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const UT: &str = "http://www.w3.org/2009/sparql/tests/test-update#";
const DAWGT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-dawg#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// Official location of the SPARQL 1.1 tests, which the manifests' IRIs are relative to.
pub const OFFICIAL_BASE: &str = "http://www.w3.org/2009/sparql/docs/tests/data-sparql11/";
/// Official location of the SPARQL 1.2 tests.
pub const OFFICIAL_BASE_12: &str = "https://w3c.github.io/rdf-tests/sparql/sparql12/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    QueryEvaluation,
    UpdateEvaluation,
    PositiveSyntax,
    NegativeSyntax,
    PositiveUpdateSyntax,
    NegativeUpdateSyntax,
    CsvResultFormat,
}

/// A graph to load: into the default graph (`name` = `None`) or a named graph.
#[derive(Debug, Clone)]
pub struct GraphFile {
    pub name: Option<NamedNode>,
    pub file: NamedNode,
}

#[derive(Debug, Clone)]
pub struct Test {
    pub id: String,
    pub kind: Kind,
    /// The query or update file (syntax tests: the file to parse).
    pub action: Option<NamedNode>,
    /// The initial dataset.
    pub data: Vec<GraphFile>,
    /// Query tests: the expected results file.
    pub result: Option<NamedNode>,
    /// Update tests: the expected dataset.
    pub expected_data: Vec<GraphFile>,
    /// Tests that need a remote `SERVICE` endpoint.
    pub needs_service: bool,
    /// The endpoints such a test calls and their data (`qt:serviceData`).
    pub service_data: Vec<(NamedNode, Vec<GraphFile>)>,
}

pub struct Suite {
    root: PathBuf,
    base: &'static str,
}

impl Suite {
    /// `root` is the local directory of a suite of `w3c/rdf-tests` (`sparql/sparql11`),
    /// `base` its official location.
    pub fn new(root: PathBuf, base: &'static str) -> Self {
        Self { root, base }
    }

    pub fn local_path(&self, iri: NamedNodeRef<'_>) -> Option<PathBuf> {
        iri.as_str()
            .strip_prefix(self.base)
            .map(|relative| self.root.join(relative))
    }

    pub fn read(&self, iri: NamedNodeRef<'_>) -> Result<String, String> {
        let path = self
            .local_path(iri)
            .ok_or_else(|| format!("{iri} is outside the suite"))?;
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// All tests of a top-level manifest (e.g. `manifest-sparql11-query.ttl`), following
    /// `mf:include`.
    pub fn tests(&self, manifest: &str) -> Result<Vec<Test>, String> {
        let mut tests = Vec::new();
        self.collect(
            &NamedNode::new_unchecked(format!("{}{manifest}", self.base)),
            &mut tests,
        )?;
        Ok(tests)
    }

    fn collect(&self, manifest: &NamedNode, tests: &mut Vec<Test>) -> Result<(), String> {
        let graph = self.parse_graph(manifest.as_ref())?;
        // The manifest resource is the file in SPARQL 1.1 and a resource of its own in
        // SPARQL 1.2 (`trs:manifest`): follow whatever includes and lists entries.
        let heads = |predicate: &str| -> Vec<Term> {
            graph
                .triples_for_predicate(&mf(predicate))
                .map(|t| t.object.into_owned())
                .collect()
        };
        for head in heads("include") {
            for include in list(&graph, Some(head.as_ref())) {
                if let Term::NamedNode(include) = include {
                    self.collect(&include, tests)?;
                }
            }
        }
        for head in heads("entries") {
            for entry in list(&graph, Some(head.as_ref())) {
                if let Some(test) = read_test(&graph, &entry) {
                    tests.push(test);
                }
            }
        }
        Ok(())
    }

    /// Parses an RDF file of the suite with its official IRI as base, keeping the graph
    /// names of a dataset format (TriG, N-Quads).
    pub fn parse_quads(&self, iri: NamedNodeRef<'_>) -> Result<Vec<nrese_rdf::Quad>, String> {
        let format = RdfFormat::from_extension(iri.as_str().rsplit('.').next().unwrap_or(""))
            .ok_or_else(|| format!("unknown RDF format: {iri}"))?;
        let text = self.read(iri)?;
        RdfParser::from_format(format)
            .with_base_iri(iri.as_str())
            .map_err(|error| error.to_string())?
            .for_slice(text.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{iri}: {error}"))
    }

    /// Parses an RDF file of the suite with its official IRI as base.
    pub fn parse_graph(&self, iri: NamedNodeRef<'_>) -> Result<Graph, String> {
        let format = RdfFormat::from_extension(iri.as_str().rsplit('.').next().unwrap_or(""))
            .ok_or_else(|| format!("unknown RDF format: {iri}"))?;
        let text = self.read(iri)?;
        RdfParser::from_format(format)
            .with_base_iri(iri.as_str())
            .map_err(|error| error.to_string())?
            .for_slice(text.as_bytes())
            .map(|quad| quad.map(Into::<nrese_rdf::Triple>::into))
            .collect::<Result<Graph, _>>()
            .map_err(|error| format!("{iri}: {error}"))
    }
}

fn mf(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{MF}{local}"))
}

fn iri(namespace: &str, local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{namespace}{local}"))
}

fn object<'a>(
    graph: &'a Graph,
    subject: NamedOrBlankNodeRef<'_>,
    predicate: &NamedNode,
) -> Option<TermRef<'a>> {
    graph.object_for_subject_predicate(subject, predicate)
}

fn objects<'a>(
    graph: &'a Graph,
    subject: NamedOrBlankNodeRef<'a>,
    predicate: &'a NamedNode,
) -> impl Iterator<Item = TermRef<'a>> + 'a {
    graph.objects_for_subject_predicate(subject, predicate)
}

fn named(term: Option<TermRef<'_>>) -> Option<NamedNode> {
    match term? {
        TermRef::NamedNode(node) => Some(node.into_owned()),
        _ => None,
    }
}

fn as_subject(term: TermRef<'_>) -> Option<NamedOrBlankNodeRef<'_>> {
    match term {
        TermRef::NamedNode(node) => Some(node.into()),
        TermRef::BlankNode(node) => Some(node.into()),
        _ => None,
    }
}

/// The members of an RDF list.
fn list(graph: &Graph, head: Option<TermRef<'_>>) -> Vec<Term> {
    let (first, rest, nil) = (iri(RDF, "first"), iri(RDF, "rest"), iri(RDF, "nil"));
    let mut members = Vec::new();
    let mut node = head.map(TermRef::into_owned);
    while let Some(current) = node {
        if current == Term::NamedNode(nil.clone()) {
            break;
        }
        let Some(subject) = as_subject(current.as_ref()) else {
            break;
        };
        if let Some(member) = object(graph, subject, &first) {
            members.push(member.into_owned());
        }
        node = object(graph, subject, &rest).map(TermRef::into_owned);
    }
    members
}

fn read_test(graph: &Graph, entry: &Term) -> Option<Test> {
    let subject = as_subject(entry.as_ref())?;
    let kind = match named(object(graph, subject, &iri(RDF, "type")))?
        .as_str()
        .strip_prefix(MF)?
    {
        "QueryEvaluationTest" => Kind::QueryEvaluation,
        "UpdateEvaluationTest" => Kind::UpdateEvaluation,
        "PositiveSyntaxTest11" | "PositiveSyntaxTest" => Kind::PositiveSyntax,
        "NegativeSyntaxTest11" | "NegativeSyntaxTest" => Kind::NegativeSyntax,
        "PositiveUpdateSyntaxTest11" | "PositiveUpdateSyntaxTest" => Kind::PositiveUpdateSyntax,
        "NegativeUpdateSyntaxTest11" | "NegativeUpdateSyntaxTest" => Kind::NegativeUpdateSyntax,
        "CSVResultFormatTest" => Kind::CsvResultFormat,
        _ => return None, // protocol, service description, graph store: HTTP-level suites
    };
    let approval = named(object(graph, subject, &iri(DAWGT, "approval")));
    if approval
        .is_some_and(|a| a.as_str().ends_with("Withdrawn") || a.as_str().ends_with("Rejected"))
    {
        return None;
    }
    let action = object(graph, subject, &mf("action"));
    let result = object(graph, subject, &mf("result"));
    let mut test = Test {
        id: entry.to_string(),
        kind,
        action: None,
        data: Vec::new(),
        result: None,
        expected_data: Vec::new(),
        needs_service: false,
        service_data: Vec::new(),
    };
    match kind {
        Kind::PositiveSyntax
        | Kind::NegativeSyntax
        | Kind::PositiveUpdateSyntax
        | Kind::NegativeUpdateSyntax => test.action = named(action),
        Kind::QueryEvaluation | Kind::CsvResultFormat => {
            let action = as_subject(action?)?;
            test.action = named(object(graph, action, &iri(QT, "query")));
            test.data.extend(
                named(object(graph, action, &iri(QT, "data")))
                    .map(|file| GraphFile { name: None, file }),
            );
            let graph_data = iri(QT, "graphData");
            test.data
                .extend(objects(graph, action, &graph_data).filter_map(|term| {
                    let file = named(Some(term))?;
                    Some(GraphFile {
                        name: Some(file.clone()),
                        file,
                    })
                }));
            for service in objects(graph, action, &iri(QT, "serviceData")) {
                let Some(service) = as_subject(service) else {
                    continue;
                };
                let Some(endpoint) = named(object(graph, service, &iri(QT, "endpoint"))) else {
                    continue;
                };
                let mut files: Vec<GraphFile> = named(object(graph, service, &iri(QT, "data")))
                    .map(|file| GraphFile { name: None, file })
                    .into_iter()
                    .collect();
                files.extend(objects(graph, service, &graph_data).filter_map(|term| {
                    let file = named(Some(term))?;
                    Some(GraphFile {
                        name: Some(file.clone()),
                        file,
                    })
                }));
                test.service_data.push((endpoint, files));
            }
            test.needs_service = !test.service_data.is_empty();
            test.result = named(result);
        }
        Kind::UpdateEvaluation => {
            let action = as_subject(action?)?;
            test.action = named(object(graph, action, &iri(UT, "request")));
            test.data = update_graphs(graph, action);
            if let Some(result) = result.and_then(as_subject) {
                test.expected_data = update_graphs(graph, result);
            }
        }
    }
    Some(test)
}

/// `ut:data` (default graph) and `ut:graphData [ ut:graph <file>; rdfs:label "iri" ]`.
fn update_graphs(graph: &Graph, node: NamedOrBlankNodeRef<'_>) -> Vec<GraphFile> {
    let mut files: Vec<GraphFile> = named(object(graph, node, &iri(UT, "data")))
        .map(|file| GraphFile { name: None, file })
        .into_iter()
        .collect();
    let graph_data = iri(UT, "graphData");
    for entry in objects(graph, node, &graph_data) {
        let Some(entry) = as_subject(entry) else {
            continue;
        };
        let file = named(object(graph, entry, &iri(UT, "graph")));
        let label = match object(graph, entry, &iri(RDFS, "label")) {
            Some(TermRef::Literal(label)) => NamedNode::new(label.value()).ok(),
            _ => None,
        };
        if let (Some(file), Some(name)) = (file, label) {
            files.push(GraphFile {
                name: Some(name),
                file,
            });
        }
    }
    files
}

pub fn suite_root() -> Option<PathBuf> {
    suite_dir("sparql11")
}

/// The local directory of the suite `name` (`sparql11`, `sparql12`).
pub fn suite_dir(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("NRESE_W3C_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/rdf-tests"));
    let dir = root.join("sparql").join(name);
    dir.is_dir().then_some(dir)
}
