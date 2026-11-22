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
    /// What the test assumes of an implementation (`mf:requires`), for the report.
    pub requires: Vec<String>,
}

/// Every entry of a manifest tree: the tests this runner runs, and the others with why not.
#[derive(Default)]
pub struct Entries {
    pub tests: Vec<Test>,
    /// `(entry, reason)`: entries this runner doesn't run.
    pub untested: Vec<(String, String)>,
    /// The `mf:entries` members read, counted apart from the two lists.
    pub count: usize,
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

    /// The suite's local directory.
    pub fn root(&self) -> &Path {
        &self.root
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

    /// All entries of a top-level manifest (e.g. `manifest-sparql11-query.ttl`), following
    /// `mf:include`, into `entries`.
    pub fn tests(&self, manifest: &str, entries: &mut Entries) -> Result<(), String> {
        self.collect(
            &NamedNode::new_unchecked(format!("{}{manifest}", self.base)),
            entries,
        )
    }

    fn collect(&self, manifest: &NamedNode, entries: &mut Entries) -> Result<(), String> {
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
                    self.collect(&include, entries)?;
                }
            }
        }
        for head in heads("entries") {
            for entry in list(&graph, Some(head.as_ref())) {
                entries.count += 1;
                match read_test(&graph, &entry) {
                    Ok(test) => entries.tests.push(test),
                    Err(reason) => entries.untested.push((entry.to_string(), reason)),
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

/// The test of `entry`, or why this runner doesn't run it.
fn read_test(graph: &Graph, entry: &Term) -> Result<Test, String> {
    let subject = as_subject(entry.as_ref()).ok_or("the entry is not a resource")?;
    let kind_iri = named(object(graph, subject, &iri(RDF, "type"))).ok_or("no test type")?;
    let kind = match kind_iri.as_str().strip_prefix(MF).unwrap_or_default() {
        "QueryEvaluationTest" => Kind::QueryEvaluation,
        "UpdateEvaluationTest" => Kind::UpdateEvaluation,
        "PositiveSyntaxTest11" | "PositiveSyntaxTest" => Kind::PositiveSyntax,
        "NegativeSyntaxTest11" | "NegativeSyntaxTest" => Kind::NegativeSyntax,
        "PositiveUpdateSyntaxTest11" | "PositiveUpdateSyntaxTest" => Kind::PositiveUpdateSyntax,
        "NegativeUpdateSyntaxTest11" | "NegativeUpdateSyntaxTest" => Kind::NegativeUpdateSyntax,
        "CSVResultFormatTest" => Kind::CsvResultFormat,
        // Protocol, service description, graph store: HTTP-level suites.
        _ => {
            return Err(format!(
                "the test type {kind_iri} is not run by this runner"
            ));
        }
    };
    let approval = named(object(graph, subject, &iri(DAWGT, "approval")));
    if let Some(approval) =
        approval.filter(|a| a.as_str().ends_with("Withdrawn") || a.as_str().ends_with("Rejected"))
    {
        return Err(format!("not run: {approval}"));
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
        requires: objects(graph, subject, &mf("requires"))
            .map(|term| term.to_string())
            .collect(),
    };
    let unreadable = || "its action can't be read".to_owned();
    match kind {
        Kind::PositiveSyntax
        | Kind::NegativeSyntax
        | Kind::PositiveUpdateSyntax
        | Kind::NegativeUpdateSyntax => test.action = named(action),
        Kind::QueryEvaluation | Kind::CsvResultFormat => {
            let action = action.and_then(as_subject).ok_or_else(unreadable)?;
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
            let action = action.and_then(as_subject).ok_or_else(unreadable)?;
            test.action = named(object(graph, action, &iri(UT, "request")));
            test.data = update_graphs(graph, action);
            if let Some(result) = result.and_then(as_subject) {
                test.expected_data = update_graphs(graph, result);
            }
        }
    }
    Ok(test)
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
