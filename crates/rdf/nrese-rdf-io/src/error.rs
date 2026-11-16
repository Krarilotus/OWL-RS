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

/// Input that isn't valid in the format: what is wrong, and where. One pointer wide: every
/// result a parser returns, and every error it travels in, carries it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct RdfSyntaxError(Box<Detail>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Detail {
    message: String,
    location: Range<TextPosition>,
    /// The statement it is in, where the parser knows it.
    statement: Option<SkippedStatement>,
    /// Nothing of the document after it is read; `cut`: it ended inside a statement. `lines`:
    /// the location's line and column are known (RDF/XML and Binary RDF keep the offset only).
    ends: bool,
    cut: bool,
    lines: bool,
}

/// The longest text of a skipped statement a consumer keeps whole (a quarantine), and so the
/// most a recovering Turtle or TriG parser reads ahead for a string's closing quote: a
/// statement longer than that can't be kept, and past it the error ends the document.
pub const MAX_STATEMENT_TEXT: usize = 1 << 20;

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
        Self(Box::new(Detail {
            message: message.into(),
            location,
            statement: None,
            ends: false,
            cut: false,
            lines: true,
        }))
    }

    /// The error with its location's offset only: no line or column it doesn't know.
    pub(crate) fn without_lines(mut self) -> Self {
        self.0.lines = false;
        self
    }

    /// Whether its location's line and column are known; its text says what is otherwise.
    pub fn knows_lines(&self) -> bool {
        self.0.lines
    }

    pub fn message(&self) -> &str {
        &self.0.message
    }

    pub fn location(&self) -> Range<TextPosition> {
        self.0.location.clone()
    }

    /// The statement the error is in, where the parser knows it.
    pub fn statement(&self) -> Option<&SkippedStatement> {
        self.0.statement.as_ref()
    }

    pub(crate) fn with_statement(mut self, statement: SkippedStatement) -> Self {
        self.0.statement = Some(statement);
        self
    }

    /// The error with its lines counted from `lines` earlier: a chunk's lines are numbered
    /// from its start, the document's from the first line.
    pub(crate) fn after_lines(mut self, lines: u64) -> Self {
        self.0.location.start.line += lines;
        self.0.location.end.line += lines;
        if let Some(statement) = &mut self.0.statement {
            statement.span.start.line += lines;
            statement.span.end.line += lines;
        }
        self
    }
}

const CUT: &str = "; the document ends inside a statement, so it may be cut off";

impl fmt::Display for RdfSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let error = &self.0;
        let start = error.location.start;
        let cut = if error.cut { CUT } else { "" };
        match (error.lines, start.offset) {
            (true, _) => write!(f, "line {}, column {}: ", start.line + 1, start.column + 1)?,
            (false, 0) => {}
            (false, offset) => write!(f, "byte {offset}: ")?,
        }
        write!(f, "{}{cut}", error.message)
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

// Every parse result carries this error, and the store's errors and a mutation's carry it in
// turn: it stays two pointers wide, a read error or the syntax error behind its box.
const _: () = assert!(std::mem::size_of::<RdfParseError>() <= 16);

impl RdfParseError {
    /// Whether the parser reads nothing of the document after this error (a read error
    /// always): skipping it would take the document's first part for the whole.
    pub fn ends_document(&self) -> bool {
        !matches!(self, Self::Syntax(error) if !error.0.ends)
    }

    /// The error, as the document's last if `ends`; `cut`: the document ended inside the
    /// statement, whose start it then shows.
    pub(crate) fn ending(self, ends: bool, cut: bool) -> Self {
        match self {
            Self::Syntax(mut error) if ends => {
                let detail = &mut *error.0;
                detail.ends = true;
                if cut && let Some(statement) = &detail.statement {
                    detail.location.start = statement.span.start;
                }
                detail.cut |= cut;
                Self::Syntax(error)
            }
            other => other,
        }
    }
}

impl From<RdfParseError> for io::Error {
    fn from(error: RdfParseError) -> Self {
        match error {
            RdfParseError::Io(error) => error,
            RdfParseError::Syntax(error) => io::Error::new(io::ErrorKind::InvalidData, error),
        }
    }
}
