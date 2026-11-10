//! Statements a dataset document puts in another store's name for the default graph
//! (`urn:x-arq:DefaultGraph`, `urn:x-rdflib:default`, `nrese_sparql::compat`) are stored in the
//! default graph, on every input path: bulk load, statement writes and restore, N-Quads and
//! TriG. The SPARQL names then reach them (`GRAPH <alias>`, COPY, DROP), and an export writes
//! them in the default graph, never under an alias.
//!
//! Each row is one input; it fails as a whole if any check after it does. All failed on
//! engine-v2 58af963dc, where the statements were stored in a named graph of the alias's IRI.

use crate::support::Writes;
use std::fs;

use nrese_store::{
    BulkLoadRequest, DatasetBackupFormat, DatasetRestoreRequest, GraphResultFormat, GraphTarget,
    MutationCommand, RdfPayload, ReadScope, Requester, StatementOp, StatementsRequest, StoreConfig,
    StoreService,
};
use tempfile::tempdir;

const EX: &str = "http://example.com/";

const NQUADS: &str = r#"<http://example.com/s1> <http://example.com/p> "1" <urn:x-arq:DefaultGraph> .
<http://example.com/s2> <http://example.com/p> "2" <urn:x-rdflib:default> .
<http://example.com/s3> <http://example.com/p> "3" <http://example.com/g> .
"#;

const TRIG: &str = r#"<urn:x-arq:DefaultGraph> { <http://example.com/s1> <http://example.com/p> "1" . }
<urn:x-rdflib:default> { <http://example.com/s2> <http://example.com/p> "2" . }
<http://example.com/g> { <http://example.com/s3> <http://example.com/p> "3" . }
"#;

/// The store's statements as its N-Quads export writes them, sorted, each `graph:subject`
/// (`DEFAULT` or local names, the graph as written); `alias!` if the export names an alias.
fn exported(service: &StoreService) -> String {
    let artifact = service
        .export_dataset(&ReadScope::All, DatasetBackupFormat::NQuads)
        .unwrap();
    let text = String::from_utf8(artifact.payload).unwrap();
    if text.contains("urn:x-arq:") || text.contains("urn:x-rdflib:") {
        return "alias!".to_owned();
    }
    let local = |term: &str| {
        let iri = term.trim_start_matches('<').trim_end_matches('>');
        iri.strip_prefix(EX).unwrap_or(iri).to_owned()
    };
    let mut all: Vec<String> = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let terms: Vec<&str> = line.split_whitespace().collect();
            let graph = match terms.len() {
                5 => local(terms[3]),
                _ => "DEFAULT".to_owned(),
            };
            format!("{graph}:{}", local(terms[0]))
        })
        .collect();
    all.sort();
    all.join(" ")
}

fn ask(service: &StoreService, query: &str) -> bool {
    let result = service.execute_query_str(query).unwrap();
    String::from_utf8(result.payload).unwrap().contains("true")
}

/// Loads `document` the way `input` names, then reads, copies and drops: what each step
/// leaves, `|`-separated.
fn run(input: &str) -> String {
    let service = StoreService::new(StoreConfig::in_memory()).unwrap();
    let (document, format) = match input.ends_with("trig") {
        true => (TRIG, GraphResultFormat::TriG),
        false => (NQUADS, GraphResultFormat::NQuads),
    };
    let dir = tempdir().unwrap();
    match input {
        "bulk-nquads" | "bulk-trig" => {
            let path = dir.path().join(match format {
                GraphResultFormat::TriG => "data.trig",
                _ => "data.nq",
            });
            fs::write(&path, document).unwrap();
            service
                .bulk_load(&BulkLoadRequest {
                    files: vec![path],
                    replace: false,
                    graph: GraphTarget::DefaultGraph,
                    invalid: Some(nrese_store::InvalidStatements::Strict),
                })
                .unwrap();
        }
        "statements-nquads" | "statements-trig" => {
            let add = StatementOp::Add {
                data: RdfPayload {
                    payload: document.as_bytes().to_vec().into(),
                    format,
                    base_iri: None,
                },
                contexts: Vec::new(),
            };
            let command = MutationCommand::Statements(StatementsRequest::new(vec![add]));
            service.apply(&command, &Requester::all()).unwrap();
        }
        "restore-nquads" => {
            service
                .restore_dataset(&DatasetRestoreRequest {
                    format: DatasetBackupFormat::NQuads,
                    payload: document.as_bytes().to_vec(),
                })
                .unwrap();
        }
        _ => unreachable!("{input}"),
    }
    let loaded = exported(&service);
    let reached = ask(
        &service,
        "ASK { GRAPH <urn:x-arq:DefaultGraph> { <http://example.com/s2> ?p ?o } }",
    );
    service
        .execute_update_str("COPY DEFAULT TO <http://example.com/c>")
        .unwrap();
    let copied = exported(&service);
    service.execute_update_str("DROP DEFAULT").unwrap();
    let dropped = exported(&service);
    format!("{loaded} | {reached} | {copied} | {dropped}")
}

/// `input | after the load | GRAPH <alias> reaches it | after COPY DEFAULT TO <c> | after DROP
/// DEFAULT`.
const ROWS: &str = "
bulk-nquads | DEFAULT:s1 DEFAULT:s2 g:s3 | true | DEFAULT:s1 DEFAULT:s2 c:s1 c:s2 g:s3 | c:s1 c:s2 g:s3
bulk-trig | DEFAULT:s1 DEFAULT:s2 g:s3 | true | DEFAULT:s1 DEFAULT:s2 c:s1 c:s2 g:s3 | c:s1 c:s2 g:s3
statements-nquads | DEFAULT:s1 DEFAULT:s2 g:s3 | true | DEFAULT:s1 DEFAULT:s2 c:s1 c:s2 g:s3 | c:s1 c:s2 g:s3
statements-trig | DEFAULT:s1 DEFAULT:s2 g:s3 | true | DEFAULT:s1 DEFAULT:s2 c:s1 c:s2 g:s3 | c:s1 c:s2 g:s3
restore-nquads | DEFAULT:s1 DEFAULT:s2 g:s3 | true | DEFAULT:s1 DEFAULT:s2 c:s1 c:s2 g:s3 | c:s1 c:s2 g:s3
";

#[test]
fn another_store_s_default_graph_name_is_stored_as_the_default_graph() {
    let mut failures = Vec::new();
    for row in ROWS.lines().filter(|l| !l.is_empty()) {
        let (input, expected) = row.split_once(" | ").unwrap();
        let got = run(input);
        let ok = got == expected;
        println!("{} {input}: {got}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{row}\n  got {got}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// What `op` leaves of a store holding `<t> <p> "t"` in the default graph and in `<g>`: its
/// statements as exported, or how many a read matched, or `refused`.
fn with_context(op: &str, writes: &str) -> String {
    use nrese_rdf::{GraphName, NamedNode};
    use nrese_store::{ReadContext, StatementPattern, WriteScope};
    let service = StoreService::new(StoreConfig::in_memory()).unwrap();
    service
        .execute_update_str(
            "INSERT DATA { <http://example.com/t> <http://example.com/p> \"t\" .
             GRAPH <http://example.com/g> { <http://example.com/t> <http://example.com/p> \"t\" } }",
        )
        .unwrap();
    let alias = || {
        vec![GraphName::NamedNode(NamedNode::new_unchecked(
            "urn:x-arq:DefaultGraph",
        ))]
    };
    let triple = |s: &str| RdfPayload {
        payload: format!("<http://example.com/{s}> <http://example.com/p> \"{s}\" .")
            .into_bytes()
            .into(),
        format: GraphResultFormat::NTriples,
        base_iri: None,
    };
    let t = || nrese_rdf::Term::from(NamedNode::new_unchecked(format!("{EX}t")));
    let pattern = || StatementPattern {
        subject: Some(t()),
        predicate: None,
        object: None,
        contexts: alias(),
    };
    let writable = |graphs: Vec<String>, default_graph| {
        WriteScope::of(Some(std::sync::Arc::new(nrese_sparql::GraphAccess {
            graphs,
            default_graph,
            ..nrese_sparql::GraphAccess::default()
        })))
    };
    let requester = match writes {
        "all" => Requester::all(),
        "default" => Requester::new(ReadScope::All, writable(Vec::new(), true)),
        "alias-iri" => Requester::new(
            ReadScope::All,
            writable(vec!["urn:x-arq:DefaultGraph".to_owned()], false),
        ),
        _ => unreachable!("{writes}"),
    };
    let op = match op {
        "add" => StatementOp::Add {
            data: triple("n"),
            contexts: alias(),
        },
        "remove-data" => StatementOp::RemoveData {
            data: triple("t"),
            contexts: alias(),
        },
        "remove-matching" => StatementOp::RemoveMatching(pattern()),
        "read" => {
            let read = service.statements(&ReadContext::all(), &pattern()).unwrap();
            return format!("read {}", read.len());
        }
        _ => unreachable!("{op}"),
    };
    let command = MutationCommand::Statements(StatementsRequest::new(vec![op]));
    match service.apply(&command, &requester) {
        Ok(_) => exported(&service),
        Err(_) => "refused".to_owned(),
    }
}

/// Review r1, S3-F2 (rows failed on slice 3's 119f32bc): an explicit context naming another
/// store's default graph (RDF4J `context=`, a quarantine replay's target) is the default graph,
/// for writes, removals and reads, and so are its write rights.
#[test]
fn an_explicit_context_naming_the_default_graph_is_the_default_graph() {
    let rows = [
        ("add", "all", "DEFAULT:n DEFAULT:t g:t"),
        ("remove-data", "all", "g:t"),
        ("remove-matching", "all", "g:t"),
        ("read", "all", "read 1"),
        ("add", "default", "DEFAULT:n DEFAULT:t g:t"),
        ("add", "alias-iri", "refused"),
    ];
    let mut failures = Vec::new();
    for (op, writes, expected) in rows {
        let got = with_context(op, writes);
        let ok = got == expected;
        println!("{} {op} {writes}: {got}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failures.push(format!("{op} {writes}: expected {expected}, got {got}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// On a fresh store, a graph write of one triple to `target`, then a delete of it: whether the
/// write created a graph, where the triple went, and whether the graph exists afterwards (for
/// a graph exists check and for a graph read).
fn graph_target(target: &str) -> String {
    use nrese_store::{GraphReadRequest, GraphWriteRequest, MutationCommitReport, ReadContext};
    let service = StoreService::new(StoreConfig::in_memory()).unwrap();
    let target = match target {
        "DEFAULT" => GraphTarget::DefaultGraph,
        named => GraphTarget::NamedGraph(named.replace("ex:", EX)),
    };
    let write = MutationCommand::GraphWrite(GraphWriteRequest {
        target: target.clone(),
        format: GraphResultFormat::NTriples,
        base_iri: None,
        payload: b"<http://example.com/w> <http://example.com/p> \"w\" ."
            .to_vec()
            .into(),
        replace: false,
    });
    let created = match service.apply(&write, &Requester::all()).unwrap() {
        MutationCommitReport::GraphWrite(report) => report.created,
        other => unreachable!("{other:?}"),
    };
    let written = exported(&service);
    service
        .apply(
            &MutationCommand::GraphDelete(target.clone()),
            &Requester::all(),
        )
        .unwrap();
    let exists = service.graph_exists(&ReadContext::all(), &target).unwrap();
    let read = service
        .execute_graph_read(
            &ReadContext::all(),
            &GraphReadRequest {
                target,
                format: GraphResultFormat::NTriples,
            },
        )
        .unwrap()
        .exists;
    format!("created {created} | {written} | exists {exists} | read {read}")
}

/// Review r2, S3-F3 (rows failed on slice 3's 6794ab22): a graph target naming another store's
/// default graph is the default graph for what a graph result says too: a write to it creates
/// no graph, and it exists while empty. An ordinary named graph is created and is gone once
/// empty (guard).
#[test]
fn a_graph_target_naming_the_default_graph_answers_as_the_default_graph() {
    let rows = [
        (
            "DEFAULT",
            "created false | DEFAULT:w | exists true | read true",
        ),
        (
            "urn:x-arq:DefaultGraph",
            "created false | DEFAULT:w | exists true | read true",
        ),
        (
            "urn:x-rdflib:default",
            "created false | DEFAULT:w | exists true | read true",
        ),
        ("ex:g", "created true | g:w | exists false | read false"),
    ];
    let mut failures = Vec::new();
    for (target, expected) in rows {
        let got = graph_target(target);
        let ok = got == expected;
        println!(
            "{} target {target}: {got}",
            if ok { "ok" } else { "FAILED" }
        );
        if !ok {
            failures.push(format!("{target}: expected {expected}, got {got}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
