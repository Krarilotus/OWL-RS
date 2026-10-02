//! Parse errors: input that can't be read, and input that isn't the format, with where.

use std::fmt;
use std::io;
use std::ops::Range;

/// A position in the input: line and column from 0, and the byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPosition {
    pub line: u64,
    pub column: u64,
    pub offset: u64,
}

/// Input that isn't valid in the format: what is wrong, and where if known.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct QueryResultsSyntaxError {
    message: String,
    location: Option<Range<TextPosition>>,
}

impl QueryResultsSyntaxError {
    pub(crate) fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            location: None,
        }
    }

    pub(crate) fn located(message: impl Into<String>, location: Range<TextPosition>) -> Self {
        Self {
            message: message.into(),
            location: Some(location),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn location(&self) -> Option<Range<TextPosition>> {
        self.location.clone()
    }
}

impl fmt::Display for QueryResultsSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(location) => write!(
                f,
                "line {}, column {}: {}",
                location.start.line + 1,
                location.start.column + 1,
                self.message
            ),
            None => f.write_str(&self.message),
        }
    }
}

/// Why parsing stopped.
#[derive(Debug, thiserror::Error)]
pub enum QueryResultsParseError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Syntax(#[from] QueryResultsSyntaxError),
}

impl From<QueryResultsParseError> for io::Error {
    fn from(error: QueryResultsParseError) -> Self {
        match error {
            QueryResultsParseError::Io(error) => error,
            QueryResultsParseError::Syntax(error) => {
                io::Error::new(io::ErrorKind::InvalidData, error)
            }
        }
    }
}

impl From<nrese_json::JsonSyntaxError> for QueryResultsSyntaxError {
    fn from(error: nrese_json::JsonSyntaxError) -> Self {
        Self::msg(format!("not valid JSON: {error}"))
    }
}

impl From<nrese_json::JsonSyntaxError> for QueryResultsParseError {
    fn from(error: nrese_json::JsonSyntaxError) -> Self {
        Self::Syntax(error.into())
    }
}

impl From<nrese_json::JsonParseError> for QueryResultsParseError {
    fn from(error: nrese_json::JsonParseError) -> Self {
        match error {
            nrese_json::JsonParseError::Io(error) => Self::Io(error),
            nrese_json::JsonParseError::Syntax(error) => Self::Syntax(error.into()),
        }
    }
}

impl From<quick_xml::Error> for QueryResultsParseError {
    fn from(error: quick_xml::Error) -> Self {
        match error {
            quick_xml::Error::Io(error) => {
                Self::Io(io::Error::new(error.kind(), error.to_string()))
            }
            error => Self::Syntax(QueryResultsSyntaxError::msg(format!(
                "not valid XML: {error}"
            ))),
        }
    }
}
