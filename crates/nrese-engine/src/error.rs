use thiserror::Error;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    /// Work refused by its memory budget before it allocated: nothing was changed.
    #[error("not enough memory: {0}")]
    MemoryLimit(#[from] nrese_exec::BudgetExceeded),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage corruption: {0}")]
    Corruption(String),
    #[error("invalid engine configuration: {0}")]
    Configuration(String),
    #[error("term {0:#x} is not known to the dictionary")]
    UnknownTerm(u64),
    #[error("RDF-star triple terms are not supported by the engine")]
    UnsupportedTerm,
    #[error("the write-ahead log failed earlier; reopen the engine to recover")]
    WalPoisoned,
    /// A commit's note ([`crate::Transaction::note_commit`]) refused it: nothing of the commit
    /// was written, and the log takes the next one.
    #[error("the commit's note refused it: {0}")]
    CommitNoteRefused(std::io::Error),
    /// A bulk load or rematerialisation whose checkpoint may be on disk without its revision
    /// being served (its placement's outcome unknown, or it couldn't be mapped): the engine
    /// takes no more writes, and reopening it recovers whatever is on disk.
    #[error(
        "revision {revision} may be committed but isn't served; the engine takes no more writes: reopen it to recover what is on disk"
    )]
    CommitUnsettled { revision: u64 },
    #[error("transaction too large for one WAL record ({bytes} bytes); use the bulk loader")]
    TransactionTooLarge { bytes: u64 },
    #[error("data directory {0} is locked by another process")]
    Locked(std::path::PathBuf),
    #[error("{0} uses an unsupported storage format; reload the data into a new directory")]
    UnsupportedFormat(std::path::PathBuf),
    /// A speculative transaction ([`crate::Engine::speculative`]) was asked to commit.
    #[error("a speculative transaction can't commit")]
    Speculative,
    /// The engine's writer or checkpoint slot (named) is held, and the caller is a Rayon pool
    /// thread, which never waits for it: the holder's parallel work may need that thread.
    /// Retry later, or ask from a thread of no pool.
    #[error("the engine's {0} slot is busy, and a Rayon pool thread doesn't wait for it")]
    SlotBusy(&'static str),
    /// The write-ahead log no longer holds the records a replica asked for
    /// ([`crate::Engine::log_since`]): it starts again from an image of revision `oldest`
    /// or later.
    #[error(
        "the log holds no records after revision {oldest} that were asked for; start from an image"
    )]
    LogTruncated { oldest: u64 },
}
