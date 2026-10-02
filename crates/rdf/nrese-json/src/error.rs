//! Errors: text that isn't JSON, with where; and input that can't be read.

use std::io;

/// Text that isn't JSON (RFC 8259): what is wrong, and where. Lines and columns count from
/// 0; the column counts bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {}, column {}: {message}", .line + 1, .column + 1)]
pub struct JsonSyntaxError {
    message: String,
    line: u64,
    column: u64,
    offset: u64,
}

impl JsonSyntaxError {
    pub(crate) fn new(message: impl Into<String>, line: u64, column: u64, offset: u64) -> Self {
        Self {
            message: message.into(),
            line,
            column,
            offset,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn line(&self) -> u64 {
        self.line
    }

    pub fn column(&self) -> u64 {
        self.column
    }

    /// The byte offset in the whole input.
    pub fn offset(&self) -> u64 {
        self.offset
    }
}

/// Why reading JSON from a reader stopped.
#[derive(Debug, thiserror::Error)]
pub enum JsonParseError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Syntax(#[from] JsonSyntaxError),
}

impl From<JsonParseError> for io::Error {
    fn from(error: JsonParseError) -> Self {
        match error {
            JsonParseError::Io(error) => error,
            JsonParseError::Syntax(error) => io::Error::new(io::ErrorKind::InvalidData, error),
        }
    }
}
