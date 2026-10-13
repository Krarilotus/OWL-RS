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
    /// The statement it is in, where the parser knows it.
    statement: Option<Box<SkippedStatement>>,
}

/// The statement a syntax error is in: where it is in the document, and what reading it
/// again on its own needs. A recovering parser ([`crate::RdfParser::recovering`]) goes
/// on after it. Known for the line formats and for Turtle and TriG; built only for an
/// error, so input without errors costs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkippedStatement {
    /// Its text in the document: from its first character to where parsing resumed after
    /// it (the line's end in N-Triples and N-Quads, past its `.` in Turtle and TriG).
    pub span: Range<TextPosition>,
    /// The directives in effect there (Turtle, TriG), as `@base` and `@prefix` lines: with
    /// them its text reads on its own. Empty where nothing is in effect.
    pub context: String,
    /// The graph block it is in (TriG), in N-Triples syntax (`<iri>` or `_:label`).
    pub graph: Option<String>,
}

impl RdfSyntaxError {
    pub(crate) fn new(message: impl Into<String>, location: Range<TextPosition>) -> Self {
        Self {
            message: message.into(),
            location,
            statement: None,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn location(&self) -> Range<TextPosition> {
        self.location.clone()
    }

    /// The statement the error is in, where the parser knows it.
    pub fn statement(&self) -> Option<&SkippedStatement> {
        self.statement.as_deref()
    }

    pub(crate) fn with_statement(mut self, statement: SkippedStatement) -> Self {
        self.statement = Some(Box::new(statement));
        self
    }

    /// The error with its lines counted from `lines` earlier: a chunk's lines are numbered
    /// from its start, the document's from the first line.
    pub(crate) fn after_lines(mut self, lines: u64) -> Self {
        self.location.start.line += lines;
        self.location.end.line += lines;
        if let Some(statement) = &mut self.statement {
            statement.span.start.line += lines;
            statement.span.end.line += lines;
        }
        self
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
