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
                    payload: document.as_bytes().to_vec(),
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
