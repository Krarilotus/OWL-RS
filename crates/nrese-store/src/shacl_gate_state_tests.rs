//! The SHACL gate's kept state ([`crate::StoreService::shacl_gate_state`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_exec::stop::StopSource;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

use super::{SHAPES_DIAGNOSTIC_BYTES, ShaclGateState};
use crate::service::hooks::AFTER_WRITE_PUBLICATION;
use crate::{
    BulkLoadRequest, DEFAULT_SHAPES_GRAPH, GraphTarget, LoadProgress, MutationCommand,
    MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest, StoreConfig, StoreService,
};

const SHAPE: &str = "<urn:shape> a <http://www.w3.org/ns/shacl#NodeShape> .";
const DATA: &str = "<urn:s> <urn:p> <urn:o> .";

fn shapes_insert() -> SparqlUpdateRequest {
    SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ GRAPH <{DEFAULT_SHAPES_GRAPH}> {{ {SHAPE} }} }}"
    ))
}

fn data_insert() -> SparqlUpdateRequest {
    SparqlUpdateRequest::new(format!("INSERT DATA {{ {DATA} }}"))
}

fn load_file(dir: &std::path::Path, contents: &str, replace: bool) -> BulkLoadRequest {
    let file = dir.join("load.trig");
    std::fs::write(&file, contents).unwrap();
    BulkLoadRequest {
        files: vec![file],
        replace,
        graph: GraphTarget::DefaultGraph,
        invalid: Some(crate::InvalidStatements::Strict),
    }
}

fn shapes_load(dir: &std::path::Path, shapes: &str, data: &str, replace: bool) -> BulkLoadRequest {
    load_file(
        dir,
        &format!("<{DEFAULT_SHAPES_GRAPH}> {{ {shapes} }}\n{data}"),
        replace,
    )
}

fn after_publish(hook: impl FnOnce() + 'static) {
    AFTER_WRITE_PUBLICATION.with_borrow_mut(|slot| {
        assert!(slot.replace(Box::new(hook)).is_none());
    });
}

fn replica_batch(store: &StoreService, primary: &StoreService, stop: &StopSource) {
    let after = store.current_revision();
    let batch = primary.replication_log(after, usize::MAX).unwrap();
    store
        .apply_replication_log(&batch.frames, &batch.expectation(after), stop)
        .unwrap();
}

/// Spec (docs/ops/http-api.md): newly published shapes are either already counted
/// (pipeline) or pending (bulk and replica), at the same revision the reader sees.
#[test]
fn published_shapes_never_show_the_old_count() {
    for (path, expected) in [("pipeline", Some(1)), ("bulk", None), ("replica", None)] {
        let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let observed = Arc::new(std::sync::OnceLock::new());
        let state = Arc::clone(&observed);
        let reader = Arc::clone(&store);
        let first = store.current_revision() + 1;
        let hook = move || state.set(reader.shacl_gate_state()).unwrap();
        match path {
            "pipeline" => {
                let reasoner =
                    ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Disabled));
                let pipeline = MutationPipeline::new(Arc::clone(&store), Arc::new(reasoner));
                after_publish(hook);
                pipeline
                    .apply(
                        MutationCommand::Update(shapes_insert()),
                        &Requester::all(),
                        &MutationTicket::new(),
                    )
                    .unwrap();
            }
            "bulk" => {
                after_publish(hook);
                store
                    .bulk_load(&shapes_load(dir.path(), SHAPE, "", false))
                    .unwrap();
            }
            "replica" => {
                let primary = StoreService::new(StoreConfig::on_disk(dir.path())).unwrap();
                primary.update_ungated(&shapes_insert()).unwrap();
                after_publish(hook);
                replica_batch(&store, &primary, &StopSource::none());
            }
            _ => unreachable!(),
        }
        let state: &ShaclGateState = observed.get().expect("observed before returning");
        assert_eq!(state.shapes, expected, "{path}: {state:?}");
        assert_eq!(state.vacuous, expected.map(|_| false), "{path}");
        assert_eq!(state.view_revision, first, "{path}");
        assert_eq!(state.revision, first, "{path}");
        if expected.is_none() {
            assert_eq!(
                state.not_counted.as_deref(),
                Some("being compiled"),
                "{path}"
            );
        }
        assert_eq!(store.shacl_gate_state().shapes, Some(1), "{path}");
    }
}

/// Spec (docs/ops/http-api.md): the count names the latest shapes change, even
/// after a data-only publication or when a refresh sees a later data revision.
#[test]
fn the_shapes_revision_names_the_write_that_changed_the_shapes() {
    for path in [
        "append elsewhere",
        "replace with identical shapes",
        "data-only replica batch",
        "shapes followed by data in a replica batch",
        "data committed before ungated refresh",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
        let first = store.current_revision() + 1;
        match path {
            "append elsewhere" | "replace with identical shapes" => {
                store.update_ungated(&shapes_insert()).unwrap();
                let request = if path == "append elsewhere" {
                    load_file(dir.path(), DATA, false)
                } else {
                    shapes_load(dir.path(), SHAPE, DATA, true)
                };
                store.bulk_load(&request).unwrap();
            }
            "data-only replica batch" | "shapes followed by data in a replica batch" => {
                let primary = StoreService::new(StoreConfig::on_disk(dir.path())).unwrap();
                primary.update_ungated(&shapes_insert()).unwrap();
                let stop = StopSource::none();
                if path == "data-only replica batch" {
                    replica_batch(&store, &primary, &stop);
                }
                primary.update_ungated(&data_insert()).unwrap();
                replica_batch(&store, &primary, &stop);
            }
            "data committed before ungated refresh" => {
                let writer = Arc::clone(&store);
                after_publish(move || {
                    writer.update_ungated(&data_insert()).unwrap();
                });
                store.update_ungated(&shapes_insert()).unwrap();
            }
            _ => unreachable!(),
        }
        let state = store.shacl_gate_state();
        assert_eq!(state.revision, first, "{path}: {state:?}");
        assert_eq!(state.view_revision, first + 1, "{path}");
        assert_eq!(state.shapes, Some(1), "{path}");
    }
}

/// Spec (docs/ops/http-api.md): a malformed stored shape's diagnostic has a byte
/// bound, including when the shape IRI contains multibyte characters.
#[test]
fn the_kept_diagnostic_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let shape = format!(
        "<urn:{}> a <http://www.w3.org/ns/shacl#PropertyShape> ; \
         <http://www.w3.org/ns/shacl#path> \"bad\" .",
        "é".repeat(5120)
    );
    store
        .bulk_load(&shapes_load(dir.path(), &shape, "", false))
        .unwrap();
    let state = store.shacl_gate_state();
    assert_eq!(state.shapes, None, "{state:?}");
    let why = state.not_counted.unwrap();
    assert!(why.len() <= SHAPES_DIAGNOSTIC_BYTES, "{} bytes", why.len());
    assert!(why.contains("don't compile"), "{why}");
}

/// Spec (docs/ops/http-api.md): stopping after publication leaves the new cell
/// unfilled and visibly stopped. The next refresh counts the same shapes revision.
#[test]
fn stopped_writers_leave_the_shapes_for_the_next_refresh() {
    for path in ["replica", "bulk"] {
        let dir = tempfile::tempdir().unwrap();
        let store = StoreService::new(StoreConfig::in_memory()).unwrap();
        let first = store.current_revision() + 1;
        let mut primary = None;
        if path == "bulk" {
            let progress = Arc::new(LoadProgress::default());
            let cancel = Arc::clone(&progress);
            after_publish(move || cancel.cancel());
            store
                .bulk_load_with(&shapes_load(dir.path(), SHAPE, "", false), &progress)
                .unwrap();
        } else {
            let source = StoreService::new(StoreConfig::on_disk(dir.path())).unwrap();
            source.update_ungated(&shapes_insert()).unwrap();
            let flag = Arc::new(AtomicBool::new(false));
            let stop = StopSource::new(Some(Arc::clone(&flag)), None);
            after_publish(move || flag.store(true, Ordering::Relaxed));
            replica_batch(&store, &source, &stop);
            primary = Some(source);
        }
        let state = store.shacl_gate_state();
        assert_eq!(state.shapes, None, "{path}: {state:?}");
        assert_eq!(state.vacuous, None, "{path}");
        assert_eq!(state.revision, first, "{path}");
        assert!(state.not_counted.unwrap().contains("stopped"), "{path}");
        if let Some(primary) = primary {
            primary.update_ungated(&data_insert()).unwrap();
            replica_batch(&store, &primary, &StopSource::none());
        } else {
            store
                .bulk_load(&load_file(dir.path(), DATA, false))
                .unwrap();
        }
        let state = store.shacl_gate_state();
        assert_eq!(state.shapes, Some(1), "{path}: {state:?}");
        assert_eq!(state.revision, first, "{path}");
        assert_eq!(state.not_counted, None, "{path}");
    }
}
