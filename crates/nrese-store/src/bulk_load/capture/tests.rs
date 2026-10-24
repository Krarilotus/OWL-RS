use super::*;
use crate::{
    BulkLoadRequest, DatasetBackupFormat, GraphTarget, InvalidStatements, ReadScope, StoreConfig,
    StoreService,
};
use nrese_exec::measure;
use nrese_rdf::{Literal, NamedNode, Quad, Triple};
use std::collections::{BTreeMap, BTreeSet};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn blank(s: &str) -> BlankNode {
    BlankNode::new_unchecked(s)
}
fn named(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}
fn request(files: Vec<PathBuf>) -> BulkLoadRequest {
    BulkLoadRequest {
        files,
        replace: false,
        graph: GraphTarget::NamedGraph("http://target".into()),
        invalid: Some(InvalidStatements::Strict),
    }
}

/// Spec: actual LOAD shares one labelled-node map but retains exact request-entry and
/// effective-graph associations, including nested terms; independently listed below.
#[test]
fn observed_load_keeps_file_occurrence_node_graph_correspondence() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.nq");
    let b = dir.path().join("b.nq");
    let nested = dir.path().join("nested.ttl");
    std::fs::write(
        &a,
        "_:shared <http://p> _:other <http://g> .\n_:shared <http://p> _:shared _:graph .\n",
    )
    .unwrap();
    std::fs::write(&b, "_:shared <http://q> \"Größe\"@de <http://h> .\n").unwrap();
    std::fs::write(
        &nested,
        "<http://holder> <http://says> <<( _:inside <http://p> _:shared )>> .\n",
    )
    .unwrap();
    let files = vec![a.clone(), b.clone(), a, nested];
    let load = request(files.clone());
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let resources = Resources {
        memory: Envelope::new(Some(1 << 20), None),
        ..Default::default()
    };
    let progress = LoadProgress::default();
    let (report, data) = store
        .bulk_load_observed(&load, &progress, &resources)
        .unwrap();
    assert_eq!((report.parsed, report.inserted, report.revision), (6, 4, 1));
    assert_eq!(progress.files(), (4, 4));
    assert_eq!(data.files(), files.as_slice());
    assert!(!data.load_scope().is_empty());
    let labels: BTreeMap<_, _> = (0..data.node_count())
        .map(|id| {
            let (input, scoped) = data.node(id).unwrap();
            (input.as_str().to_owned(), scoped.into_owned())
        })
        .collect();
    assert_eq!(
        labels.keys().map(String::as_str).collect::<Vec<_>>(),
        ["graph", "inside", "other", "shared"]
    );
    let observed: BTreeSet<_> = data
        .associations()
        .iter()
        .map(|&(file, node, graph)| {
            let (input, _) = data.node(node).unwrap();
            let graph = match data.graph(graph).unwrap() {
                GraphNameRef::DefaultGraph => "default".to_owned(),
                GraphNameRef::NamedNode(n) => n.as_str().to_owned(),
                GraphNameRef::BlankNode(n) => {
                    assert_eq!(n.as_str(), labels["graph"].as_str());
                    "blank:graph".to_owned()
                }
            };
            (file, input.as_str().to_owned(), graph)
        })
        .collect();
    let expected: BTreeSet<_> = [
        (0, "shared", "http://g"),
        (0, "other", "http://g"),
        (0, "shared", "blank:graph"),
        (0, "graph", "blank:graph"),
        (1, "shared", "http://h"),
        (2, "shared", "http://g"),
        (2, "other", "http://g"),
        (2, "shared", "blank:graph"),
        (2, "graph", "blank:graph"),
        (3, "inside", "http://target"),
        (3, "shared", "http://target"),
    ]
    .into_iter()
    .map(|(f, n, g)| (f, n.into(), g.into()))
    .collect();
    assert_eq!(
        data.associations().len(),
        11,
        "actual file occurrence relation was lost"
    );
    assert_eq!(observed, expected);
    assert_eq!((data.node_count(), data.graph_count()), (4, 4));
    let payload = store
        .export_dataset(&ReadScope::All, DatasetBackupFormat::NQuads)
        .unwrap()
        .payload;
    let actual: BTreeSet<_> = nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::NQuads)
        .for_slice(&payload)
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: BTreeSet<_> = [
        Quad::new(
            labels["shared"].clone(),
            named("http://p"),
            labels["other"].clone(),
            named("http://g"),
        ),
        Quad::new(
            labels["shared"].clone(),
            named("http://p"),
            labels["shared"].clone(),
            labels["graph"].clone(),
        ),
        Quad::new(
            labels["shared"].clone(),
            named("http://q"),
            Literal::new_language_tagged_literal("Größe", "de").unwrap(),
            named("http://h"),
        ),
        Quad::new(
            named("http://holder"),
            named("http://says"),
            Triple::new(
                labels["inside"].clone(),
                named("http://p"),
                labels["shared"].clone(),
            ),
            named("http://target"),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(actual, expected);
    assert!(resources.memory.used() > 0);
    drop(data);
    assert_eq!(resources.memory.used(), 0);
    // Ordinary API/defaults remain available without enabling a capture.
    assert_eq!(store.bulk_load(&request(vec![b])).unwrap().parsed, 1);
}

/// Spec: first original capture refusal is final even when later headroom returns;
/// actual owned representation and charge survive until disposal, with no truncation.
#[test]
fn capture_first_failure_keeps_its_cause_and_stops_growth() {
    if !measure::alone() {
        return;
    }
    let files = vec![PathBuf::from("actual-path.nq")];
    let progress = LoadProgress::default();
    let input = blank("a");
    let scoped = blank("scope-a");
    let after = blank("after");
    let long = blank(&"x".repeat(4096));
    // A real growing collector, with NO blocker credit, observes actual requested heap
    // versus admission while its complete payload is retained after capture finality.
    let inputs: Vec<_> = (0..64).map(|i| blank(&format!("n{i}_ä"))).collect();
    let scoped_inputs: Vec<_> = (0..64).map(|i| blank(&format!("scope_n{i}_ä"))).collect();
    let resources = Resources {
        memory: Envelope::new(Some(1 << 20), None),
        ..Default::default()
    };
    let (((data, used), requested), excess) = measure::measure_within(&resources.memory, || {
        measure::measure(|| {
            let capture = Capture::new(&files, &resources, &progress).unwrap();
            capture.set_scope("scope").unwrap();
            for (input, scoped) in inputs.iter().zip(&scoped_inputs) {
                capture
                    .record(0, input, scoped, &GraphName::DefaultGraph)
                    .unwrap();
                capture
                    .record(0, input, scoped, &GraphName::DefaultGraph)
                    .unwrap();
            }
            let report = super::super::BulkLoadReport {
                revision: 0,
                parsed: 0,
                inserted: 0,
                deleted: 0,
                quarantined: 0,
                load: None,
                examples: Vec::new(),
                elapsed: std::time::Duration::ZERO,
            };
            let (_, data) = capture.finish(Ok(report)).unwrap();
            let used = resources.memory.used();
            (data, used)
        })
    });
    assert_eq!(
        excess, 0,
        "complete collector requested bytes escaped admission"
    );
    assert_eq!(
        (
            data.node_count(),
            data.graph_count(),
            data.associations().len()
        ),
        (64, 1, 64)
    );
    assert_eq!(resources.memory.used(), used);
    eprintln!(
        "capture_allocation requested_peak={requested} retained_charge={used} admitted_peak={}",
        resources.memory.peak()
    );
    drop(data);
    assert_eq!(resources.memory.used(), 0);
    for partial in [false, true] {
        let resources = Resources {
            memory: Envelope::new(Some(8192), None),
            ..Default::default()
        };
        let ((), excess) = measure::measure_within(&resources.memory, || {
            let capture =
                Capture::new(if partial { &files } else { &[] }, &resources, &progress).unwrap();
            if partial {
                capture.set_scope("scope").unwrap();
                capture
                    .record(0, &input, &scoped, &GraphName::DefaultGraph)
                    .unwrap();
            }
            let remaining = resources.memory.remaining();
            let mut blocker = Reservation::new(resources.memory.clone());
            blocker.charge(remaining).unwrap();
            let refused = if partial {
                capture.record(0, &long, &long, &GraphName::DefaultGraph)
            } else {
                capture.set_scope("scope")
            };
            assert_eq!(refused, Err(()));
            let before = {
                let state = capture.state.lock();
                (
                    state.data.node_count(),
                    state.data.graph_count(),
                    state.data.associations().len(),
                )
            };
            assert_eq!(before, if partial { (1, 1, 1) } else { (0, 0, 0) });
            blocker.release(remaining);
            assert_eq!(
                capture.record(0, &after, &after, &GraphName::DefaultGraph),
                Err(())
            );
            {
                let state = capture.state.lock();
                assert_eq!(
                    (
                        state.data.node_count(),
                        state.data.graph_count(),
                        state.data.associations().len()
                    ),
                    before
                );
                if partial {
                    assert_eq!(state.data.node(0).unwrap().0.as_str(), "a");
                }
            }
            let error = capture.finish(Err(StoreError::LoadCancelled)).unwrap_err();
            let StoreError::SparqlEvaluation(nrese_sparql::QueryEvaluationError::MemoryLimit(
                error,
            )) = error
            else {
                panic!("original typed refusal lost")
            };
            assert_eq!((error.limit, error.used, error.shared), (8192, 8192, false));
            assert_eq!(resources.memory.used(), 0);
        });
        assert_eq!(
            excess, 0,
            "actual collector bytes beyond admission, partial={partial}"
        );
    }
    // Cancellation owns its original type, and no producer can append after publication.
    let token = nrese_sparql::CancellationToken::new();
    let resources = Resources {
        cancellation: Some(token.clone()),
        ..Default::default()
    };
    let capture = Capture::new(&[], &resources, &progress).unwrap();
    token.cancel();
    assert_eq!(capture.checkpoint(), Err(()));
    let joins = rayon::join(
        || capture.record(0, &input, &scoped, &GraphName::DefaultGraph),
        || capture.record(0, &after, &after, &GraphName::DefaultGraph),
    );
    assert_eq!(joins, (Err(()), Err(())));
    assert!(matches!(
        capture.finish(Err(StoreError::LoadCancelled)),
        Err(StoreError::SparqlEvaluation(
            nrese_sparql::QueryEvaluationError::Cancelled
        ))
    ));
    assert_eq!(resources.memory.used(), 0);
    // Existing RAII on unwind frees the partial capture and its backing before leases.
    let resources = Resources::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let capture = Capture::new(&files, &resources, &progress).unwrap();
        capture.set_scope("scope").unwrap();
        capture
            .record(0, &input, &scoped, &GraphName::DefaultGraph)
            .unwrap();
        panic!("owned capture unwind control");
    }));
    assert!(result.is_err());
    assert_eq!(resources.memory.used(), 0);
    // Actual failed observed load aborts, joins and releases its writer before return.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.nq");
    let short = dir.path().join("short.nq");
    std::fs::write(&short, "_:a <http://p> _:b .\n").unwrap();
    std::fs::write(
        &path,
        format!("_:{} <http://p> _:a .\n", "z".repeat(20_000)).repeat(32),
    )
    .unwrap();
    for limit in [0, 4096] {
        let store = StoreService::new(StoreConfig::in_memory()).unwrap();
        let bounded = Resources {
            memory: Envelope::new(Some(limit), None),
            ..Default::default()
        };
        let error = store
            .bulk_load_observed(&request(vec![path.clone()]), &progress, &bounded)
            .unwrap_err();
        let StoreError::SparqlEvaluation(nrese_sparql::QueryEvaluationError::MemoryLimit(error)) =
            error
        else {
            panic!("load lost typed refusal")
        };
        assert_eq!(error.limit, limit);
        if limit > 0 {
            assert!(
                error.requested >= 20_000,
                "must reach actual label-copy admission after parsing starts"
            );
        }
        assert_eq!(store.current_revision(), 0);
        assert_eq!(bounded.memory.used(), 0);
        assert_eq!(
            store
                .bulk_load(&request(vec![short.clone()]))
                .unwrap()
                .revision,
            1
        );
    }
}
