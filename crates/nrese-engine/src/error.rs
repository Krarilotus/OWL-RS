use thiserror::Error;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
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
    #[error("transaction too large for one WAL record ({bytes} bytes); use the bulk loader")]
    TransactionTooLarge { bytes: u64 },
    #[error("data directory {0} is locked by another process")]
    Locked(std::path::PathBuf),
    #[error("{0} uses an unsupported storage format; reload the data into a new directory")]
    UnsupportedFormat(std::path::PathBuf),
    /// A speculative transaction ([`crate::Engine::speculative`]) was asked to commit.
    #[error("a speculative transaction can't commit")]
    Speculative,
    /// The write-ahead log no longer holds the records a replica asked for
    /// ([`crate::Engine::log_since`]): it starts again from an image of revision `oldest`
    /// or later.
    #[error(
        "the log holds no records after revision {oldest} that were asked for; start from an image"
    )]
    LogTruncated { oldest: u64 },
}
