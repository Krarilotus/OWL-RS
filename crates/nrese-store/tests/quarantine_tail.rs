//! Recovery witnesses from refs/proofs/unowned-sweep-1109-final (Q06).
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::*;

// Spec: an acknowledged fresh-blank-node readmission after recovery remains resolved
// on reopen (SYN-67). Tail repair does not make RDF commit + journal append atomic.
#[test]
fn q06_quarantine_repairs_torn_tail_before_append() {
    for tail in [b"{\"id\":".as_slice(), b"{\"id\":\"\xf0\x9f".as_slice()] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("bad.ttl");
        std::fs::write(
            &input,
            "@prefix : <http://example.com/> . _:b :p :o :extra .\n",
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
        let id = store
            .quarantine()
            .list(&QuarantineFilter::default())
            .unwrap()[0]
            .statement
            .id
            .clone();
        drop(store);
        let log = config.data_dir.join("quarantine/resolutions.jsonl");
        std::fs::write(&log, tail).unwrap();
        let pipeline = MutationPipeline::new(
            Arc::new(StoreService::new(config.clone()).unwrap()),
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
            &id,
            &request,
        )
        .unwrap();
        drop(pipeline);
        let reopened = StoreService::new(config).unwrap();
        let item = reopened.quarantine().get(&id).unwrap().unwrap();
        assert!(
            item.resolution.is_some(),
            "acknowledged resolution swallowed after torn tail: {item:?}"
        );
    }
}
