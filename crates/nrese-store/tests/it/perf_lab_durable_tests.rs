//! perf_lab's durable mode (examples/perf_lab/durable.rs) checks the data a run leaves, not
//! only its size (Sol r1 R4): a time counts only after its store is reopened and its exact
//! contents found.

#[path = "../../examples/perf_lab/durable.rs"]
#[allow(dead_code)]
mod durable;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};

/// A store holding one statement at revision 1, but another than the workload's, is refused
/// though its count and revision match; a real run reports what it verified.
#[test]
fn the_durable_mode_checks_contents_not_counts() {
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::open(dir.path(), config).unwrap();
    let iri = |s: &str| NamedNode::new_unchecked(s);
    let mut tx = engine.transaction();
    let wrong = Quad::new(
        iri("http://wrong/s"),
        iri("http://wrong/p"),
        iri("http://wrong/o"),
        GraphName::DefaultGraph,
    );
    tx.insert(wrong.as_ref());
    tx.commit().unwrap();
    assert!(
        durable::check(&engine, 1, 1).is_err(),
        "wrong contents of the right size accepted"
    );
    drop(engine);

    let root = tempfile::tempdir().unwrap();
    let report = durable::durable("commit-every-3", root.path(), 2).unwrap();
    assert!(
        report.contains("\"verified\": true, \"statements\": 3, \"revision\": 3,"),
        "{report}"
    );
}
