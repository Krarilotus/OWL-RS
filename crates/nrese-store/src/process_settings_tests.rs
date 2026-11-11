//! D16-489: the process's settings are the first opened store's. A draft check's scratch
//! store and a store opened after the first (the server's system store, another repository)
//! leave them, read back and through what the index encoding decides: the size of an index
//! built from the same data. Each test in a process of its own: the settings are the
//! process's.

use nrese_engine::{IndexEncoding, VocabularyEncoding};
use nrese_exec::memory::{self, HugePages};

use crate::{
    BulkLoadRequest, CancellationToken, DraftInputs, DraftLimits, DraftOperation, GraphTarget,
    ProcessSettings, StoreConfig, StoreService, process_settings, run_draft_check,
};

/// The operator's choice: a value other than the default for each setting the process
/// keeps (`huge_pages` as given).
fn operator(huge_pages: HugePages) -> StoreConfig {
    StoreConfig {
        index_encoding: IndexEncoding::Compact,
        vocabulary: VocabularyEncoding::Fsst,
        reasoning_release: true,
        huge_pages,
        ..StoreConfig::in_memory()
    }
}

fn settings_of(config: &StoreConfig) -> ProcessSettings {
    ProcessSettings {
        index_encoding: config.index_encoding,
        vocabulary: config.vocabulary,
        huge_pages: config.huge_pages,
        reasoning_release: config.reasoning_release,
    }
}

/// 20,000 statements whose index is smaller in the compact encoding than in the fast one,
/// loaded in place of a store's data.
fn data() -> (tempfile::TempDir, BulkLoadRequest) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("data.nt");
    let text: String = (0..20_000)
        .map(|i| {
            format!(
                "<http://example.com/s{}> <http://example.com/p{}> <http://example.com/o{}> .\n",
                i / 4,
                i % 3,
                i % 50
            )
        })
        .collect();
    std::fs::write(&input, text).unwrap();
    let request = BulkLoadRequest {
        files: vec![input],
        replace: true,
        graph: GraphTarget::DefaultGraph,
        invalid: None,
    };
    (dir, request)
}

/// The heap bytes of `store`'s index once `request` has replaced its data.
fn index_bytes_after_load(store: &StoreService, request: &BulkLoadRequest) -> u64 {
    store.bulk_load(request).unwrap();
    store.engine_stats().index_bytes
}

/// A reader's draft check (its scratch store opens with the defaults) leaves the operator's
/// settings: the same data loaded again builds the same compact index.
#[test]
fn a_draft_check_leaves_the_operators_settings() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let (_dir, request) = data();
    let config = operator(HugePages::Off);
    let store = StoreService::new(config.clone()).unwrap();
    let compact = index_bytes_after_load(&store, &request);
    assert_eq!(process_settings(), Some(settings_of(&config)));

    let checked = run_draft_check(
        DraftOperation::Query,
        &DraftInputs {
            data: Some(
                "<http://example.com/x> <http://example.com/p> <http://example.com/y> .\n"
                    .to_owned(),
            ),
            query: Some("ASK { ?s ?p ?o }".to_owned()),
            ..DraftInputs::default()
        },
        &DraftLimits {
            max_seconds: 30.0,
            max_results: 10,
            max_triples: 1_000,
        },
        &CancellationToken::new(),
        None,
    );
    assert_eq!(checked.observed_ask, Some(true), "{checked:?}");
    assert_eq!(process_settings(), Some(settings_of(&config)));
    assert_eq!(
        index_bytes_after_load(&store, &request),
        compact,
        "the operator's compact index after a draft check"
    );
    // The witness tells the encodings apart: under fast the same data builds a larger index.
    nrese_engine::set_index_encoding(IndexEncoding::Fast);
    assert!(
        index_bytes_after_load(&store, &request) > compact,
        "the data's index is larger under fast"
    );
}

/// A store opened after the first with other values, as the server's system store opens
/// after its configured store (`AppState::try_new`), leaves the operator's settings: the
/// same compact index, and huge pages as the operator's `on` left them.
#[test]
fn a_store_opened_after_the_first_leaves_the_operators_settings() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let started = memory::transparent_huge_pages();
    let (_dir, request) = data();
    let config = operator(HugePages::On);
    let store = StoreService::new(config.clone()).unwrap();
    let compact = index_bytes_after_load(&store, &request);

    let other = StoreService::new(StoreConfig {
        huge_pages: HugePages::Off,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    assert_eq!(process_settings(), Some(settings_of(&config)));
    assert_eq!(
        memory::transparent_huge_pages(),
        started,
        "huge pages as the process started, under the operator's on"
    );
    assert_eq!(
        index_bytes_after_load(&store, &request),
        compact,
        "the operator's compact index after another store opened"
    );
    drop(other);
}
