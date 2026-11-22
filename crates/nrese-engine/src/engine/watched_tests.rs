use std::sync::Arc;

use nrese_exec::stop::StopSource;
use nrese_rdf::{GraphName, NamedNode, NamedNodeRef, Quad};

use super::{BulkMode, Engine, EngineConfig};
use crate::DurabilityConfig;

fn graph() -> NamedNode {
    NamedNode::new_unchecked("urn:watched")
}

fn quad(subject: &str, graph: GraphName) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(subject),
        NamedNode::new_unchecked("urn:p"),
        NamedNode::new_unchecked("urn:o"),
        graph,
    )
}

fn insert(engine: &Engine, quad: &Quad) -> u64 {
    let mut tx = engine.transaction();
    assert!(tx.insert(quad.as_ref()));
    tx.commit().unwrap().revision
}

/// Spec (docs/ops/http-api.md): asserted changes to the watched graph renew its
/// cell. Data changes, inferred changes and layout changes preserve it, including
/// data-only records at the end of a replica batch.
#[test]
fn a_version_keeps_the_cell_until_the_watched_graph_changes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(
        dir.path(),
        EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    let graph = graph();
    engine.watch_graph(graph.clone()).unwrap();
    let initial = engine.snapshot();
    assert!(initial.watched().unwrap().keep(Arc::new("empty")));

    let shapes = quad("urn:shape", graph.clone().into());
    let mut tx = engine.transaction();
    assert!(tx.insert(shapes.as_ref()));
    assert!(tx.pending_snapshot().watched().is_none());
    tx.keep_watched(Arc::new("shapes"));
    let first = tx.commit().unwrap().revision;
    let written = engine.snapshot();
    let kept = written.watched().unwrap().kept().expect("prefilled");
    let data = quad("urn:data", GraphName::DefaultGraph);
    let compactions = engine.stats().compactions;
    insert(&engine, &data);
    assert!(engine.stats().compactions > compactions);
    let compacted = engine.snapshot();
    let batch = engine.log_since(0, usize::MAX).unwrap();
    let mut tx = engine.transaction();
    let intern = |iri: &str| tx.intern(NamedNodeRef::new_unchecked(iri).into());
    let triple = crate::EncodedTriple {
        subject: intern("urn:other"),
        predicate: intern("urn:p"),
        object: intern("urn:o"),
    };
    assert!(tx.insert_inferred(triple));
    tx.commit().unwrap();
    let inferred = engine.snapshot();
    for (path, view) in [
        ("prefilled shapes", &written),
        ("asserted data and compaction", &compacted),
        ("inferred data", &inferred),
    ] {
        let cell = view.watched().unwrap();
        assert_eq!(cell.changed_at(), first, "{path}");
        assert_eq!(
            cell.kept().and_then(|value| value.downcast_ref::<&str>()),
            Some(&"shapes"),
            "{path}"
        );
        assert!(std::ptr::eq(cell.kept().unwrap(), kept), "{path}");
    }
    assert_eq!(initial.watched().unwrap().changed_at(), 0);

    let replica = Engine::new(EngineConfig::default()).unwrap();
    replica.watch_graph(graph).unwrap();
    replica
        .apply_log(&batch.frames, &batch.expectation(0), &StopSource::none())
        .unwrap();
    let published = replica.snapshot();
    let cell = published.watched().unwrap();
    assert_eq!(published.revision(), first + 1);
    assert_eq!(cell.changed_at(), first);
    assert!(cell.kept().is_none());
}

/// Spec (docs/ops/http-api.md): append and replace keep the count and its revision
/// when their asserted watched range is unchanged, including a load that spills.
#[test]
fn a_bulk_load_compares_the_asserted_watched_range() {
    for (label, mode, spill, change, mapped) in [
        ("append elsewhere", BulkMode::Append, false, false, false),
        (
            "identical replacement",
            BulkMode::Replace,
            false,
            false,
            false,
        ),
        ("changed replacement", BulkMode::Replace, false, true, false),
        (
            "identical spilled replacement",
            BulkMode::Replace,
            true,
            false,
            true,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(
            dir.path(),
            EngineConfig {
                durability: DurabilityConfig {
                    map_checkpoints: mapped,
                    bulk_load_memory: spill.then_some(1),
                    ..DurabilityConfig::default()
                },
                ..EngineConfig::default()
            },
        )
        .unwrap();
        let graph = graph();
        engine.watch_graph(graph.clone()).unwrap();
        let original = quad("urn:shape", graph.clone().into());
        let first = insert(&engine, &original);
        let previous = engine.snapshot();
        let previous = previous.watched().unwrap();
        assert!(previous.keep(Arc::new(true)));

        let load = engine.bulk_load(mode);
        if mode == BulkMode::Replace {
            let shapes = if change {
                quad("urn:changed", graph.into())
            } else {
                original
            };
            load.add(&[shapes]);
        }
        load.add(&[quad("urn:data", GraphName::DefaultGraph)]);
        let commit = load.finish().unwrap();
        assert_eq!(commit.revision, first + 1, "{label}");
        let view = engine.snapshot();
        let cell = view.watched().unwrap();
        assert_eq!(
            cell.changed_at(),
            if change { commit.revision } else { first },
            "{label}"
        );
        assert_eq!(
            cell.kept().and_then(|value| value.downcast_ref::<bool>()),
            (!change).then_some(&true),
            "{label}"
        );
        if !change {
            assert!(
                std::ptr::eq(cell.kept().unwrap(), previous.kept().unwrap()),
                "{label}"
            );
        }
    }
}
