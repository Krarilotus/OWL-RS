//! One gated write path (review v2 §3, L1): every public write goes through the mutation
//! pipeline, and a front-end other than SPARQL writes through its extension point
//! ([`DeltaPlanner`]) with the same gates.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nrese_engine::Transaction;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Triple};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_sparql::GraphAccess;
use nrese_store::{
    DEFAULT_SHAPES_GRAPH, DeltaPlanner, GateSeverity, MutationCommand, MutationCommitReport,
    MutationError, MutationKind, MutationPipeline, MutationTicket, ReadScope, Requester, ShaclGate,
    StoreConfig, StoreError, StoreService, UpdateContext, WriteScope,
};

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// A property graph's `CREATE (a:Label)-[:rel {since}]->(b)`, as a front-end other than
/// SPARQL would plan it (the RDF 1.2 reifier mapping): the node's label, the edge, its
/// reifier and the edge's property.
#[derive(Debug)]
struct CreateEdge {
    from: &'static str,
    label: &'static str,
    rel: &'static str,
    to: &'static str,
    since: i64,
}

impl DeltaPlanner for CreateEdge {
    fn name(&self) -> &str {
        "create-edge"
    }

    fn plan(
        &self,
        tx: &mut Transaction<'_>,
        _requester: &Requester,
        context: &UpdateContext<'_>,
    ) -> Result<(), StoreError> {
        assert!(!context.cancellation().is_cancelled());
        let (from, rel, to) = (ex(self.from), ex(self.rel), ex(self.to));
        let reifier = ex(&format!("{}-{}-{}", self.from, self.rel, self.to));
        let reifies =
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies");
        let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let edge = Triple::new(from.clone(), rel.clone(), to.clone());
        for quad in [
            Quad::new(
                from.clone(),
                rdf_type,
                ex(self.label),
                GraphName::DefaultGraph,
            ),
            Quad::new(from, rel, to, GraphName::DefaultGraph),
            Quad::new(reifier.clone(), reifies, edge, GraphName::DefaultGraph),
            Quad::new(
                reifier,
                ex("since"),
                Literal::from(self.since),
                GraphName::DefaultGraph,
            ),
        ] {
            tx.insert(quad.as_ref());
        }
        Ok(())
    }
}

fn create(from: &'static str, label: &'static str, to: &'static str) -> MutationCommand {
    MutationCommand::Delta(Arc::new(CreateEdge {
        from,
        label,
        rel: "knows",
        to,
        since: 2020,
    }))
}

fn holds(pipeline: &MutationPipeline, pattern: &str) -> bool {
    let answer = pipeline
        .store()
        .execute_query_str(&format!("PREFIX ex: <{EX}> ASK {{ {pattern} }}"))
        .expect("ask");
    String::from_utf8(answer.payload).unwrap().contains("true")
}

/// A front-end's write is gated as a SPARQL update is: committed with its inferences
/// maintained, refused outside its requester's write scope, rejected when it makes the
/// data inconsistent or breaks a shape, and not committed once cancelled. Nothing of a
/// refused write is visible.
#[test]
fn front_end_writes_go_through_every_gate() {
    let store = StoreService::new(StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    })
    .unwrap();
    let pipeline = MutationPipeline::new(
        Arc::new(store),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    );
    pipeline
        .execute_update_str(&format!(
            "PREFIX ex: <{EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX sh: <http://www.w3.org/ns/shacl#>
             INSERT DATA {{ ex:Person rdfs:subClassOf ex:Agent . ex:Person owl:disjointWith ex:Robot .
                 GRAPH <{DEFAULT_SHAPES_GRAPH}> {{ ex:Shape a sh:NodeShape ;
                     sh:targetClass ex:Thing ; sh:property [ sh:path ex:name ; sh:minCount 1 ] }} }}"
        ))
        .expect("schema and shapes");

    // Committed, with its inferences: alice is an agent.
    let owner = Requester::all();
    let report = pipeline
        .apply(
            create("alice", "Person", "bob"),
            &owner,
            &MutationTicket::new(),
        )
        .expect("committed");
    assert!(matches!(report, MutationCommitReport::Applied { revision } if revision > 0));
    assert!(holds(&pipeline, "ex:alice a ex:Agent"));
    assert!(holds(
        &pipeline,
        "?r <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( ex:alice ex:knows ex:bob )>> ; ex:since 2020"
    ));

    // Outside the requester's write scope: refused, as the front-end's kind.
    let public_only = Requester::new(
        ReadScope::All,
        WriteScope::Graphs(Arc::new(GraphAccess {
            prefixes: vec!["urn:g:public".to_owned()],
            ..GraphAccess::default()
        })),
    );
    let refused = pipeline.apply(
        create("carol", "Person", "bob"),
        &public_only,
        &MutationTicket::new(),
    );
    assert!(
        matches!(
            refused,
            Err(MutationError::Store {
                kind: MutationKind::Delta,
                source: StoreError::Forbidden(_)
            })
        ),
        "{refused:?}"
    );
    assert!(!holds(&pipeline, "ex:carol ?p ?o"));

    // Inconsistent under the reasoning: rejected.
    let robot = pipeline.apply(
        create("alice", "Robot", "dave"),
        &owner,
        &MutationTicket::new(),
    );
    assert!(
        matches!(robot, Err(MutationError::Rejected(_))),
        "{robot:?}"
    );
    assert!(!holds(&pipeline, "ex:alice ex:knows ex:dave"));

    // A shape broken: a thing without a name.
    let shape = pipeline.apply(
        create("erin", "Thing", "bob"),
        &owner,
        &MutationTicket::new(),
    );
    assert!(
        matches!(shape, Err(MutationError::Rejected(_))),
        "{shape:?}"
    );
    assert!(!holds(&pipeline, "ex:erin ?p ?o"));

    // Cancelled before the commit: nothing committed.
    let ticket = MutationTicket::new();
    ticket.cancel();
    let cancelled = pipeline.apply(create("frank", "Person", "bob"), &owner, &ticket);
    assert!(
        matches!(cancelled, Err(MutationError::Cancelled)),
        "{cancelled:?}"
    );
    assert!(!holds(&pipeline, "ex:frank ?p ?o"));
}

/// Every `.rs` file under `dir`, its path relative to `root`.
fn sources(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
}

/// The signature line of the function around line `at` of `lines`.
fn enclosing_fn<'a>(lines: &[&'a str], at: usize) -> &'a str {
    lines[..=at]
        .iter()
        .rev()
        .map(|line| line.trim_start())
        .find(|line| {
            let line = line
                .strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub(super) "))
                .or_else(|| line.strip_prefix("pub "))
                .unwrap_or(line);
            line.starts_with("fn ") || line.starts_with("async fn ")
        })
        .unwrap_or("")
}

/// No public method of the store or the server commits a transaction outside the
/// mutation pipeline (the review's L1): an engine commit outside `mutation/pipeline.rs`
/// is in a function private to its crate, and the store's ungated writes for its own
/// system graphs and scratch stores (`*_ungated`) are called only where listed. Bulk
/// loads, rematerialisation and a replica's log commit through operations of their own
/// (`bulk_load`, `rematerialise`, `apply_replication_log`), not transactions.
#[test]
fn no_public_method_commits_outside_the_pipeline() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in ["nrese-store", "nrese-server"] {
        let root = crates.join(krate);
        let mut found = Vec::new();
        sources(&root, &root.join("src"), &mut found);
        files.extend(
            found
                .into_iter()
                .map(|(name, text)| (format!("{krate}/{name}"), text)),
        );
    }
    assert!(files.len() > 50, "{} files", files.len());
    let ungated_callers = [
        "nrese-store/src/service.rs",
        "nrese-store/src/access/control.rs",
        "nrese-store/src/access/queries.rs",
        "nrese-store/src/draft_check.rs",
        // Unit tests.
        "nrese-store/src/dl/classification.rs",
        "nrese-store/src/dl/bounds_memory_tests.rs",
        "nrese-store/src/query_executor_tests.rs",
    ];
    let mut commits = 0;
    for (name, text) in &files {
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(".commit()") {
                commits += 1;
                let signature = enclosing_fn(&lines, at);
                assert!(
                    name == "nrese-store/src/mutation/pipeline.rs"
                        || !signature.starts_with("pub fn")
                            && !signature.starts_with("pub async fn"),
                    "{name}:{}: a public function commits past the pipeline: {signature}",
                    at + 1
                );
            }
            if code.contains("_ungated(") && !code.contains("fn ") {
                assert!(
                    ungated_callers.contains(&name.as_str()),
                    "{name}:{}: an ungated write outside the store's own: {}",
                    at + 1,
                    line.trim()
                );
            }
        }
    }
    // The pipeline's two commits, the store's own write and its ontology preload at least.
    assert!(commits >= 4, "{commits} commits found");
}
