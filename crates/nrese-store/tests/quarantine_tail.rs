//! Recovery witnesses from refs/proofs/unowned-sweep-1109-final (Q06).
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::*;

// Tail repair does not make RDF commit + journal append atomic.
fn recover_tail(tail: Option<&[u8]>) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("bad.ttl");
    std::fs::write(
        &input,
        "@prefix : <http://example.com/> . _:a :p :o :extra .\n_:b :p :o :extra .\n",
    )
    .unwrap();
    let config = StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: dir.path().join("store"),
        ..StoreConfig::in_memory()
    };
    let store = StoreService::new(config.clone()).unwrap();
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![input],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: None,
        })
        .unwrap();
    let ids: Vec<_> = store
        .quarantine()
        .list(&QuarantineFilter::default())
        .unwrap()
        .into_iter()
        .map(|item| item.statement.id)
        .collect();
    assert_eq!(ids.len(), 2);
    store.quarantine().discard(&ids[..1], None).unwrap();
    drop(store);
    let log = config.data_dir.join("quarantine/resolutions.jsonl");
    let mut prior = std::fs::read(&log).unwrap();
    if let Some(tail) = tail {
        prior.extend_from_slice(tail);
    } else {
        assert_eq!(prior.pop(), Some(b'\n'));
    }
    std::fs::write(&log, prior).unwrap();
    let reopened = StoreService::new(config.clone())
        .unwrap_or_else(|error| panic!("quarantine recovery must open: {error}"));
    let pipeline = MutationPipeline::new(
        Arc::new(reopened),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    let request = Readmission {
        text: Some("_:b <http://example.com/p> <http://example.com/o> .".into()),
        fresh_blank_nodes: true,
        ..Default::default()
    };
    readmit(
        &pipeline,
        &Requester::all(),
        &MutationTicket::new(),
        &ids[1],
        &request,
    )
    .unwrap();
    drop(pipeline);
    let reopened = StoreService::new(config).unwrap();
    for id in &ids {
        let item = reopened.quarantine().get(id).unwrap().unwrap();
        assert!(
            item.resolution.is_some(),
            "acknowledged resolution swallowed after tail recovery: {item:?}"
        );
    }
}

// Spec: SYN-67 / Q06 r1 F1: an acknowledged readmission after a torn JSON tail
// survives reopen, together with the earlier acknowledged discard.
#[test]
fn q06_quarantine_repairs_torn_tail_before_append() {
    recover_tail(Some(b"{\"id\":"));
}

// Spec: Q06 r1 F1: torn UTF-8 must not refuse startup; subsequent acknowledged
// readmission and the prior discard both survive reopen.
#[test]
fn q06_quarantine_repairs_torn_utf8_before_append() {
    recover_tail(Some(b"{\"id\":\"\xf0\x9f"));
}

// Spec: Q06 r1 F1: a complete last record missing its newline keeps the earlier
// acknowledged discard when the next readmission appends, and both survive reopen.
#[test]
fn q06_quarantine_frames_complete_tail_before_append() {
    recover_tail(None);
}
