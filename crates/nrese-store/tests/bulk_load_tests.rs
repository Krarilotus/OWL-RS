//! E5 gate (store side): bulk loading files through parallel parsing and the engine's bulk
//! path.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreError, StoreMode, StoreService};
use tempfile::tempdir;

fn service() -> StoreService {
    StoreService::new(StoreConfig::in_memory()).expect("store")
}

fn request(files: &[&Path]) -> BulkLoadRequest {
    BulkLoadRequest {
        files: files.iter().map(|path| path.to_path_buf()).collect(),
        replace: false,
        graph: GraphTarget::DefaultGraph,
        skip_errors: false,
    }
}

/// A single integer from a `SELECT (COUNT… AS ?n)` query.
fn count(service: &StoreService, query: &str) -> u64 {
    let result = service.execute_query_str(query).expect("query");
    let json: serde_json_lite::Value = serde_json_lite::parse(&result.payload);
    json.first_binding_integer("n")
}

/// Enough lines that the parser splits the file into several chunks (oxttl's minimum chunk
/// is 16 KiB). Every subject is a blank node; `_:b{i}` recurs throughout the file, so a
/// label mapped differently in two chunks would show up as extra nodes.
fn ntriples(lines: usize) -> String {
    let mut out = String::new();
    for i in 0..lines {
        writeln!(
            out,
            "_:b{} <http://example.com/p{}> \"value {i}\" .",
            i % 97,
            i % 5
        )
        .unwrap();
    }
    out
}

#[test]
fn parallel_ntriples_keep_blank_nodes_consistent_across_chunks() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("data.nt");
    fs::write(&path, ntriples(20_000)).unwrap();
    assert!(fs::metadata(&path).unwrap().len() > 8 * 16 * 1024, "splits");

    let service = service();
    let report = service.bulk_load(&request(&[&path])).expect("load");
    assert_eq!(
        (report.parsed, report.inserted, report.revision),
        (20_000, 20_000, 1)
    );
    assert_eq!(
        count(
            &service,
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o }"
        ),
        97
    );

    // A second load of the same file gets fresh blank nodes.
    let report = service.bulk_load(&request(&[&path])).expect("load");
    assert_eq!(report.inserted, 20_000);
    assert_eq!(
        count(
            &service,
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o }"
        ),
        194
    );
}

#[test]
fn quads_turtle_and_graph_targets() {
    let dir = tempdir().unwrap();
    let nquads = dir.path().join("data.nq");
    fs::write(
        &nquads,
        "<http://example.com/a> <http://example.com/p> \"1\" <http://example.com/g1> .\n\
         <http://example.com/a> <http://example.com/p> \"2\" .\n",
    )
    .unwrap();
    let turtle = dir.path().join("data.ttl");
    fs::write(
        &turtle,
        "@prefix ex: <http://example.com/> .\n\
         ex:b ex:p [ ex:q \"nested\" ] .\n\
         <relative> ex:p ex:c .\n",
    )
    .unwrap();

    let service = service();
    let mut load = request(&[&nquads, &turtle]);
    load.graph = GraphTarget::NamedGraph("http://example.com/g2".to_owned());
    let report = service.bulk_load(&load).expect("load");
    assert_eq!(report.inserted, 5);
    // N-Quads keep their graphs; Turtle goes to the target graph.
    let in_graph = |graph: &str| {
        count(
            &service,
            &format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{graph}> {{ ?s ?p ?o }} }}"),
        )
    };
    assert_eq!(in_graph("http://example.com/g1"), 1);
    assert_eq!(in_graph("http://example.com/g2"), 3);
    assert_eq!(
        count(&service, "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"),
        1
    );
    // Relative IRIs resolve against the file's URL.
    let relative = url_of(&turtle).replace("data.ttl", "relative");
    assert_eq!(
        count(
            &service,
            &format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <http://example.com/g2> {{ <{relative}> ?p ?o }} }}"
            )
        ),
        1
    );
}

fn url_of(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap();
    url::Url::from_file_path(canonical).unwrap().to_string()
}

#[test]
fn replace_swaps_the_dataset() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first.nt");
    let second = dir.path().join("second.nt");
    fs::write(&first, ntriples(100)).unwrap();
    fs::write(
        &second,
        "<http://example.com/x> <http://example.com/p> <http://example.com/y> .\n",
    )
    .unwrap();
    let service = service();
    service.bulk_load(&request(&[&first])).expect("load");
    let mut replace = request(&[&second]);
    replace.replace = true;
    let report = service.bulk_load(&replace).expect("replace");
    assert_eq!((report.inserted, report.deleted), (1, 100));
    assert_eq!(service.stats().unwrap().quad_count, 1);
}

#[test]
fn a_parse_error_names_the_file_and_changes_nothing() {
    let dir = tempdir().unwrap();
    let good = dir.path().join("good.nt");
    let bad = dir.path().join("bad.nt");
    fs::write(&good, ntriples(10)).unwrap();
    fs::write(&bad, "<http://example.com/x> <broken\n").unwrap();
    let service = service();
    let error = service.bulk_load(&request(&[&good, &bad])).unwrap_err();
    match error {
        StoreError::FileParse { path, .. } => assert_eq!(path, bad),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(service.stats().unwrap().quad_count, 0);
    assert_eq!(service.current_revision(), 0);

    let error = service
        .bulk_load(&request(&[&dir.path().join("data.unknown")]))
        .unwrap_err();
    assert!(matches!(error, StoreError::Configuration(_)), "{error}");
}

/// With `skip_errors`, bad statements are counted and skipped and the rest loads: lines of
/// N-Triples, statements of Turtle.
#[test]
fn skip_errors_loads_the_good_statements() {
    let dir = tempdir().unwrap();
    let lines = dir.path().join("lines.nt");
    let mut text = ntriples(10);
    text.push_str("<http://example.com/x> <broken\n");
    text.push_str("<http://example.com/y> <http://example.com/p> \"ok\" .\n");
    fs::write(&lines, text).unwrap();
    let turtle = dir.path().join("doc.ttl");
    fs::write(
        &turtle,
        "@prefix ex: <http://example.com/> .\nex:t1 ex:p 1 .\nex:t2 ex:p ex:q ex:r .\nex:t3 ex:p 3 .\n",
    )
    .unwrap();
    let service = service();
    let report = service
        .bulk_load(&BulkLoadRequest {
            skip_errors: true,
            ..request(&[&lines, &turtle])
        })
        .unwrap();
    assert_eq!(report.skipped, 2);
    assert_eq!(report.inserted, 13);
    // Without: the first bad statement stops the load.
    let service = self::service();
    assert!(service.bulk_load(&request(&[&lines])).is_err());
}

#[test]
fn durable_bulk_loads_survive_a_restart() {
    let dir = tempdir().unwrap();
    let data = dir.path().join("data.nt");
    fs::write(&data, ntriples(5_000)).unwrap();
    let config = StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: dir.path().join("store"),
        ..StoreConfig::in_memory()
    };
    {
        let service = StoreService::new(config.clone()).expect("store");
        service.bulk_load(&request(&[&data])).expect("load");
        service
            .execute_update_str(
                "INSERT DATA { <http://example.com/after> <http://example.com/p> 1 }",
            )
            .expect("update");
    }
    let service = StoreService::new(config).expect("reopen");
    assert_eq!(service.stats().unwrap().quad_count, 5_001);
    assert_eq!(service.current_revision(), 2);
}

/// Just enough JSON to read a count out of SPARQL JSON results, without a dependency.
mod serde_json_lite {
    pub struct Value(String);

    pub fn parse(bytes: &[u8]) -> Value {
        Value(String::from_utf8(bytes.to_vec()).expect("utf-8"))
    }

    impl Value {
        pub fn first_binding_integer(&self, variable: &str) -> u64 {
            let key = format!("\"{variable}\"");
            let after = &self.0[self.0.find(&key).expect("binding") + key.len()..];
            let value = after.find("\"value\"").expect("value") + "\"value\"".len();
            after[value..]
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .expect("integer")
        }
    }
}
