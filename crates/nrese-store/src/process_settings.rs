//! The settings that are the process's, not a store's: the index encoding, the vocabulary,
//! huge pages and the reasoning release (D16-489). The engine and the allocator keep one
//! value each for the whole process, so they are set once: by the server from its
//! configuration before it opens a store ([`apply_process_settings`]), otherwise by the first
//! store the process opens. Every store opened after that leaves them as they are, so a store
//! the server opens for itself (the system store, a draft check's scratch store) or another
//! repository can't replace the operator's choice.

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
    /// The values `config` asks for.
    pub fn of(config: &StoreConfig) -> Self {
        Self {
            index_encoding: config.index_encoding,
            vocabulary: config.vocabulary,
            huge_pages: config.huge_pages,
            reasoning_release: config.reasoning_release,
        }
    }
}

static APPLIED: OnceLock<ProcessSettings> = OnceLock::new();

/// Sets the process's values from `config` if nothing has set them yet, before the first
/// store opens (its recovery builds index blocks in the operator's encoding); afterwards
/// leaves them as they are. Returns the values in effect.
pub fn apply_process_settings(config: &StoreConfig) -> ProcessSettings {
    *APPLIED.get_or_init(|| {
        let settings = ProcessSettings::of(config);
        nrese_engine::set_index_encoding(settings.index_encoding);
        nrese_engine::set_vocabulary_encoding(settings.vocabulary);
        // On leaves what the process was started with (an operator's `MIMALLOC_ALLOW_THP=0`
        // switches them off the same way); bulk phases follow `Bulk` (the engine's loads
        // and rematerialisations hold one).
        nrese_exec::memory::set_huge_pages(settings.huge_pages);
        nrese_engine::memory::set_release_between_rounds(settings.reasoning_release);
        settings
    })
}

/// The values the process runs with; `None` before anything set them.
pub fn process_settings() -> Option<ProcessSettings> {
    APPLIED.get().copied()
}
