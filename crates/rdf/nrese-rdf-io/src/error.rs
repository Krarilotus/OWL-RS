//! Parse errors: input that can't be read, and input that isn't the format, with where.

use std::fmt;
use std::io;
use std::ops::Range;

/// A position in the input: line and column from 0 (the column counts characters), and
/// the byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPosition {
    pub line: u64,
    pub column: u64,
    pub offset: u64,
}

/// Input that isn't valid in the format: what is wrong, and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct RdfSyntaxError {
    message: String,
    location: Range<TextPosition>,
}

impl RdfSyntaxError {
    pub(crate) fn new(message: impl Into<String>, location: Range<TextPosition>) -> Self {
        Self {
            message: message.into(),
            location,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn location(&self) -> Range<TextPosition> {
        self.location.clone()
    }
}

impl fmt::Display for RdfSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let start = self.location.start;
        write!(
            f,
            "line {}, column {}: {}",
            start.line + 1,
            start.column + 1,
            self.message
        )
    }
}

/// Why parsing stopped.
#[derive(Debug, thiserror::Error)]
pub enum RdfParseError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Syntax(#[from] RdfSyntaxError),
}

impl From<RdfParseError> for io::Error {
    fn from(error: RdfParseError) -> Self {
        match error {
            RdfParseError::Io(error) => error,
            RdfParseError::Syntax(error) => io::Error::new(io::ErrorKind::InvalidData, error),
        }
    }
}
