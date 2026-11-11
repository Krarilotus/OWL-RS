//! The settings that are the process's, not a store's: the index encoding, the vocabulary,
//! huge pages and the reasoning release (D16-489). The engine and the allocator keep one
//! value each for the whole process, so the first store the process opens sets them, and
//! every store opened after it leaves them as they are: the server opens its configured
//! store first, so a store it opens for itself (the system store, a draft check's scratch
//! store) or another repository can't replace the operator's choice.

use std::sync::OnceLock;

use crate::config::StoreConfig;

/// The process-wide values of a store's settings, as the process runs with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSettings {
    pub index_encoding: nrese_engine::IndexEncoding,
    pub vocabulary: nrese_engine::VocabularyEncoding,
    pub huge_pages: nrese_exec::memory::HugePages,
    pub reasoning_release: bool,
}

impl ProcessSettings {
    fn of(config: &StoreConfig) -> Self {
        Self {
            index_encoding: config.index_encoding,
            vocabulary: config.vocabulary,
            huge_pages: config.huge_pages,
            reasoning_release: config.reasoning_release,
        }
    }
}

static APPLIED: OnceLock<ProcessSettings> = OnceLock::new();

/// Sets the process's values from `config` if no store has set them yet, before the first
/// store opens (its recovery builds index blocks in the operator's encoding); afterwards
/// leaves them as they are.
pub(crate) fn apply(config: &StoreConfig) {
    APPLIED.get_or_init(|| {
        let settings = ProcessSettings::of(config);
        nrese_engine::set_index_encoding(settings.index_encoding);
        nrese_engine::set_vocabulary_encoding(settings.vocabulary);
        // On leaves what the process was started with (an operator's `MIMALLOC_ALLOW_THP=0`
        // switches them off the same way); bulk phases follow `Bulk` (the engine's loads
        // and rematerialisations hold one).
        nrese_exec::memory::set_huge_pages(settings.huge_pages);
        nrese_engine::memory::set_release_between_rounds(settings.reasoning_release);
        settings
    });
}

/// The values the process runs with: the first opened store's; `None` before any store.
pub fn process_settings() -> Option<ProcessSettings> {
    APPLIED.get().copied()
}
