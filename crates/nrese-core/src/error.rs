use thiserror::Error;

pub type NreseResult<T> = Result<T, NreseError>;

#[derive(Debug, Error)]
pub enum NreseError {
    #[error("invalid IRI: {0}")]
    InvalidIri(String),
    #[error("operation is not supported: {0}")]
    Unsupported(&'static str),
    #[error("configuration error: {0}")]
    Configuration(String),
}
