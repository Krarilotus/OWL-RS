//! The quarantine (docs/design/quarantine.md): a bulk load loads every valid statement and
//! keeps each invalid one verbatim; quarantined statements reach no query, inference,
//! export or count; re-admitted ones, as kept, fixed or repaired, behave like loaded ones
//! and pass the commit gates like any write; `strict` stops the load; a load without
//! errors leaves nothing behind.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadReport, BulkLoadRequest, DatasetBackupFormat, EntryState, GraphTarget,
    InvalidStatements, MutationError, MutationPipeline, MutationTicket, Outcome, QuarantineFilter,
    Readmission, Repair, StoreConfig, StoreError, StoreMode, StoreService,
};
use tempfile::tempdir;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> ";

/// The N-Triples lines: a domain axiom, then statements about `ex:obj{k}`, three of them
/// invalid as in the Claros case (a `%` that starts no escape, a space in an IRI, no
/// object). Enough lines that the file is parsed in several chunks.
fn lines() -> Vec<String> {
    (0..6000)
        .map(|k| match k {
            0 => format!("<{EX}p> <http://www.w3.org/2000/01/rdf-schema#domain> <{EX}C> ."),
            1000 => format!("<{EX}claros?genre=%&a=1> <{EX}p> \"bad\" ."),
            3000 => format!("<{EX}has space> <{EX}p> \"bad\" ."),
            5999 => format!("<{EX}obj5999> <{EX}p> ."),
            k => format!("<{EX}obj{k}> <{EX}p> \"value {k}\" ."),
        })
        .collect()
}

const TRIG: &str = "@prefix ex: <http://example.com/> .\n\
                    ex:g2 { ex:u1 ex:p 1 . ex:u2 ex:p <bad iri> . ex:u3 ex:p 3 . }\n";

const TURTLE: &str = "@prefix ex: <http://example.com/> .\n\
                      ex:t1 ex:p 1 .\n\
                      ex:t2 ex:p ex:q ex:r .\n\
                      ex:y a <http://www.w3.org/2002/07/owl#Nothing> ex:extra .\n\
                      ex:t3 ex:p 3 .\n\
                      _:b ex:p _:c ex:oops .\n";

struct Files {
    _dir: tempfile::TempDir,
    store: PathBuf,
    nt: PathBuf,
    trig: PathBuf,
    ttl: PathBuf,
}

fn files() -> Files {
    let dir = tempdir().unwrap();
    let nt = dir.path().join("claros.nt");
    fs::write(&nt, lines().join("\n") + "\n").unwrap();
    assert!(fs::metadata(&nt).unwrap().len() > 8 * 16 * 1024, "splits");
    let trig = dir.path().join("graphs.trig");
    fs::write(&trig, TRIG).unwrap();
    let ttl = dir.path().join("doc.ttl");
    fs::write(&ttl, TURTLE).unwrap();
    Files {
        store: dir.path().join("store"),
        _dir: dir,
        nt,
        trig,
        ttl,
    }
}

fn on_disk(dir: &Path) -> StoreConfig {
    StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: dir.to_path_buf(),
        ..StoreConfig::in_memory()
    }
}

fn pipeline(config: StoreConfig) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn load(store: &StoreService, files: &[&Path], graph: GraphTarget) -> BulkLoadReport {
    store
        .bulk_load(&BulkLoadRequest {
            files: files.iter().map(|path| path.to_path_buf()).collect(),
            replace: false,
            graph,
            invalid: None,
        })
        .expect("load")
}

fn ask(store: &StoreService, query: &str) -> bool {
    let result = store
        .execute_query_str(&format!("{PREFIXES}ASK {{ {query} }}"))
        .expect("query");
    let json: serde_json::Value = serde_json::from_slice(&result.payload).unwrap();
    json["boolean"].as_bool().expect("a boolean")
}

/// Both loads of the files, the second into `ex:g`.
fn load_all(store: &StoreService, files: &Files) -> (BulkLoadReport, BulkLoadReport) {
    let first = load(store, &[&files.nt, &files.trig], GraphTarget::DefaultGraph);
    let second = load(
        store,
        &[&files.ttl],
        GraphTarget::NamedGraph(format!("{EX}g")),
    );
    (first, second)
}

#[test]
fn a_load_keeps_the_valid_part_and_quarantines_each_bad_statement_verbatim() {
    let files = files();
    let store = StoreService::new(on_disk(&files.store)).expect("store");
    let (first, second) = load_all(&store, &files);
    // Every valid statement, each invalid one quarantined.
    assert_eq!((first.inserted, first.quarantined), (5997 + 2, 4));
    assert_eq!((second.inserted, second.quarantined), (2, 3));
    assert_eq!(store.stats().unwrap().quad_count, 5997 + 2 + 2);
    assert_ne!(first.load, second.load);

    // The report's examples: the first in the files' order, verbatim, where and why.
    let lines = lines();
    let where_: Vec<_> = first
        .examples
        .iter()
        .map(|e| (Path::new(&e.file).file_name().unwrap().to_owned(), e.line))
        .collect();
    let named = |name: &str, line| (std::ffi::OsString::from(name), line);
    assert_eq!(
        where_,
        [
            named("claros.nt", 1001),
            named("claros.nt", 3001),
            named("claros.nt", 6000),
            named("graphs.trig", 2),
        ]
    );
    for (example, k) in first.examples.iter().zip([1000, 3000, 5999]) {
        assert_eq!(example.text.as_deref(), Some(lines[k].as_str()));
        assert!(!example.reason.is_empty() && example.column == 1);
        assert_eq!(example.load, *first.load.as_ref().unwrap());
    }
    let trig = &first.examples[3];
    assert_eq!(trig.text.as_deref(), Some("ex:u2 ex:p <bad iri> ."));
    assert_eq!(trig.graph.as_deref(), Some("<http://example.com/g2>"));
    assert!(trig.context.contains("@prefix ex: <http://example.com/> ."));
    let turtle: Vec<_> = second.examples.iter().map(|e| e.line).collect();
    assert_eq!(turtle, [3, 4, 6]);
    assert_eq!(
        second.examples[0].target.as_deref(),
        Some("<http://example.com/g>")
    );

    // Listed: all seven, open, with unique ids; the same after a restart.
    let quarantine = store.quarantine();
    let listed = quarantine.list(&QuarantineFilter::default()).unwrap();
    assert_eq!(listed.len(), 7);
    assert_eq!(quarantine.open_count(), 7);
    let mut ids: Vec<_> = listed.iter().map(|i| i.statement.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 7);
    let of_second = QuarantineFilter {
        load: second.load.clone(),
        ..QuarantineFilter::default()
    };
    assert_eq!(quarantine.list(&of_second).unwrap().len(), 3);
    let mut exported = Vec::new();
    assert_eq!(
        quarantine
            .export(&QuarantineFilter::default(), &mut exported)
            .unwrap(),
        7
    );
    assert_eq!(String::from_utf8(exported).unwrap().lines().count(), 7);
    drop(store);
    let store = StoreService::new(on_disk(&files.store)).expect("reopen");
    assert_eq!(store.quarantine().open_count(), 7);
    assert_eq!(
        store
            .quarantine()
            .list(&QuarantineFilter::default())
            .unwrap(),
        listed
    );
}

#[test]
fn quarantined_statements_reach_no_query_inference_export_or_count() {
    let files = files();
    let pipeline = pipeline(on_disk(&files.store));
    let store = pipeline.store();
    load_all(store, &files);
    let rules = pipeline.reasoner().config().materialised_program().unwrap();
    store.rematerialise(rules).expect("materialise");

    // The domain axiom types the loaded subjects, and nothing of the quarantined ones.
    assert!(ask(store, "ex:obj1 a ex:C"));
    for bad in ["claros", "has space", "obj5999", "u2", "t2", "ex:y"] {
        let query = format!("?s ?p ?o FILTER(CONTAINS(STR(?s), \"{bad}\"))");
        assert!(!ask(store, &query), "{bad}");
        let query = format!("GRAPH ?g {{ ?s ?p ?o }} FILTER(CONTAINS(STR(?s), \"{bad}\"))");
        assert!(!ask(store, &query), "{bad}");
    }
    assert!(!ask(store, "?s a owl:Nothing"));
    let stats = store.stats().unwrap();
    assert_eq!(stats.quad_count, 5997 + 2 + 2);
    let export = store
        .export_dataset(&nrese_store::ReadScope::All, DatasetBackupFormat::NQuads)
        .unwrap();
    assert_eq!(export.quad_count, 5997 + 2 + 2);
    let text = String::from_utf8(export.payload).unwrap();
    for bad in [
        "claros",
        "has space",
        "obj5999",
        "u2",
        "t2",
        "example.com/y>",
    ] {
        assert!(!text.contains(bad), "{bad}");
    }
}

#[test]
fn readmitted_statements_behave_like_loaded_ones() {
    let files = files();
    let pipeline = pipeline(on_disk(&files.store));
    let store = pipeline.store().clone();
    let (first, second) = load_all(&store, &files);
    let id = |report: &BulkLoadReport, line: u64| {
        report
            .examples
            .iter()
            .find(|e| e.line == line)
            .unwrap()
            .id
            .clone()
    };
    let readmit = |id: &str, request: Readmission| {
        nrese_store::readmit(
            &pipeline,
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
            id,
            &request,
        )
    };
    let repaired = Readmission {
        repairs: vec![Repair::PercentEncodeIris],
        ..Readmission::default()
    };
    let fixed = |text: &str| Readmission {
        text: Some(text.to_owned()),
        ..Readmission::default()
    };

    // Repaired on request: the repair and what was admitted are recorded, and the rules
    // apply to it as to a loaded statement.
    let claros = readmit(&id(&first, 1001), repaired.clone()).expect("readmitted");
    assert!(matches!(claros.outcome, Outcome::Readmitted { .. }));
    assert_eq!(claros.repairs, [Repair::PercentEncodeIris]);
    assert_eq!(
        claros.text.as_deref(),
        Some("<http://example.com/claros?genre=%25&a=1> <http://example.com/p> \"bad\" .")
    );
    assert!(ask(
        &store,
        "<http://example.com/claros?genre=%25&a=1> ex:p \"bad\""
    ));
    assert!(ask(
        &store,
        "<http://example.com/claros?genre=%25&a=1> a ex:C"
    ));
    readmit(&id(&first, 3001), repaired).expect("readmitted");
    assert!(ask(&store, "<http://example.com/has%20space> a ex:C"));

    // Still invalid as kept: refused, and still open; fixed, admitted.
    let no_object = id(&first, 6000);
    assert!(matches!(
        readmit(&no_object, Readmission::default()),
        Err(MutationError::Store {
            source: StoreError::RdfParse(_),
            ..
        })
    ));
    let fix = "<http://example.com/obj5999> <http://example.com/p> \"fixed\" .";
    let resolution = readmit(&no_object, fixed(fix)).expect("readmitted");
    assert_eq!(resolution.text.as_deref(), Some(fix));
    assert!(ask(&store, "ex:obj5999 ex:p \"fixed\" . ex:obj5999 a ex:C"));
    // Resolved once only.
    assert!(readmit(&no_object, fixed(fix)).is_err());

    // A TriG statement goes back into its block's graph, a Turtle one into the load's
    // graph, with the directives they were read with.
    readmit(&id(&first, 2), fixed("ex:u2 ex:p ex:fixed .")).expect("readmitted");
    assert!(ask(&store, "GRAPH ex:g2 { ex:u2 ex:p ex:fixed }"));
    readmit(&id(&second, 3), fixed("ex:t2 ex:p ex:q .")).expect("readmitted");
    assert!(ask(&store, "GRAPH ex:g { ex:t2 ex:p ex:q }"));

    // The commit gates decide as for any write: an inconsistency is rejected, and the
    // statement stays open.
    let nothing = id(&second, 4);
    let rejected = readmit(
        &nothing,
        fixed("ex:y a <http://www.w3.org/2002/07/owl#Nothing> ."),
    );
    assert!(
        matches!(rejected, Err(MutationError::Rejected(_))),
        "{rejected:?}"
    );
    assert!(!ask(&store, "GRAPH ex:g { ex:y ?p ?o }"));

    // Blank nodes: refused unless fresh ones will do.
    let blank = id(&second, 6);
    let refused = readmit(&blank, fixed("_:b ex:p _:c ."));
    assert!(
        matches!(
            refused,
            Err(MutationError::Store {
                source: StoreError::Quarantine(_),
                ..
            })
        ),
        "{refused:?}"
    );
    readmit(
        &blank,
        Readmission {
            fresh_blank_nodes: true,
            ..fixed("_:b ex:p _:c .")
        },
    )
    .expect("readmitted");

    // What is left: the rejected one; discarded, nothing.
    let quarantine = store.quarantine();
    let open: Vec<_> = quarantine
        .list(&QuarantineFilter::default())
        .unwrap()
        .into_iter()
        .map(|item| item.statement.id)
        .collect();
    assert_eq!(open, std::slice::from_ref(&nothing));
    assert_eq!(
        quarantine
            .discard(std::slice::from_ref(&nothing), Some("not ours".to_owned()))
            .unwrap(),
        1
    );
    assert!(
        quarantine
            .discard(std::slice::from_ref(&nothing), None)
            .is_err()
    );
    assert_eq!(quarantine.open_count(), 0);

    // The resolutions survive a restart; so does what was admitted.
    let revision = store.current_revision();
    drop((pipeline, store));
    let store = StoreService::new(on_disk(&files.store)).expect("reopen");
    assert_eq!(store.current_revision(), revision);
    let quarantine = store.quarantine();
    assert_eq!(quarantine.open_count(), 0);
    let resolved = quarantine
        .list(&QuarantineFilter {
            state: EntryState::Resolved,
            ..QuarantineFilter::default()
        })
        .unwrap();
    assert_eq!(resolved.len(), 7);
    let discarded = resolved
        .iter()
        .find(|item| item.statement.id == nothing)
        .and_then(|item| item.resolution.as_ref())
        .unwrap();
    assert_eq!(discarded.outcome, Outcome::Discarded);
    assert_eq!(discarded.note.as_deref(), Some("not ours"));
    assert!(ask(
        &store,
        "<http://example.com/claros?genre=%25&a=1> ex:p \"bad\""
    ));
}

#[test]
fn strict_stops_at_the_first_bad_statement_and_keeps_nothing() {
    let files = files();
    let request = |invalid| BulkLoadRequest {
        files: vec![files.nt.clone()],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        invalid,
    };
    // Asked for by the load, or the store's setting.
    let store = StoreService::new(on_disk(&files.store)).expect("store");
    let error = store
        .bulk_load(&request(Some(InvalidStatements::Strict)))
        .unwrap_err();
    assert!(matches!(error, StoreError::FileParse { .. }), "{error}");
    drop(store);
    let config = StoreConfig {
        invalid_statements: InvalidStatements::Strict,
        ..on_disk(&files.store)
    };
    let store = StoreService::new(config).expect("store");
    assert!(store.bulk_load(&request(None)).is_err());
    assert_eq!(store.current_revision(), 0);
    assert_eq!(store.stats().unwrap().quad_count, 0);
    assert_eq!(store.quarantine().open_count(), 0);
    assert!(!files.store.join("quarantine").exists());
    // A load may still ask for the quarantine.
    let report = store
        .bulk_load(&request(Some(InvalidStatements::Quarantine)))
        .unwrap();
    assert_eq!(report.quarantined, 3);
}

/// Without errors, a load leaves no trace of the quarantine; a load that fails leaves
/// none of what it had quarantined.
#[test]
fn a_load_without_errors_or_that_fails_leaves_no_quarantine() {
    let files = files();
    let good = files._dir.path().join("good.nt");
    let text: String = lines()
        .into_iter()
        .enumerate()
        .filter(|(k, _)| ![1000, 3000, 5999].contains(k))
        .map(|(_, line)| line + "\n")
        .collect();
    fs::write(&good, text).unwrap();
    let store = StoreService::new(on_disk(&files.store)).expect("store");
    let report = load(&store, &[&good], GraphTarget::DefaultGraph);
    assert_eq!((report.inserted, report.quarantined), (5997, 0));
    assert_eq!((report.load, report.examples.len()), (None, 0));
    assert!(!files.store.join("quarantine").exists());

    // The bad lines are quarantined as it goes; then a file can't be read.
    let unreadable = files._dir.path().join("directory.nt");
    fs::create_dir(&unreadable).unwrap();
    let failed = store.bulk_load(&BulkLoadRequest {
        files: vec![files.nt.clone(), unreadable],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        invalid: None,
    });
    assert!(failed.is_err());
    assert_eq!(store.quarantine().open_count(), 0);
    let left: Vec<_> = fs::read_dir(files.store.join("quarantine"))
        .map(|dir| dir.map(|entry| entry.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    drop(store);
    let store = StoreService::new(on_disk(&files.store)).expect("reopen");
    assert_eq!(store.quarantine().open_count(), 0);
    assert_eq!(store.stats().unwrap().quad_count, 5997);
}
