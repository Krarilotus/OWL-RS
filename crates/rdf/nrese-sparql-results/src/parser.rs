//! Reading results: from memory or a reader, a boolean or the variables and then one
//! solution at a time.

use std::io::{BufReader, Read};
use std::sync::Arc;

use nrese_json::{ReaderJsonParser, SliceJsonParser};
use nrese_rdf::{Term, Variable};

use crate::error::{QueryResultsParseError, QueryResultsSyntaxError};
use crate::format::QueryResultsFormat;
use crate::json::{self, JsonSolutions};
use crate::solution::QuerySolution;
use crate::tsv;
use crate::xml::{self, XmlSolutions};

/// Reads query results in a [`QueryResultsFormat`] (JSON, XML or TSV; CSV is lossy and
/// can't be read).
#[derive(Debug, Clone, Copy)]
pub struct QueryResultsParser {
    format: QueryResultsFormat,
}

fn csv_error() -> QueryResultsSyntaxError {
    QueryResultsSyntaxError::msg(
        "SPARQL results in CSV can't be read back: CSV doesn't say which values are IRIs, blank \
         nodes or literals",
    )
}

/// A slice can't fail to be read: every error is in its content.
fn syntax_only(error: QueryResultsParseError) -> QueryResultsSyntaxError {
    match error {
        QueryResultsParseError::Syntax(error) => error,
        QueryResultsParseError::Io(error) => QueryResultsSyntaxError::msg(error.to_string()),
    }
}

impl QueryResultsParser {
    pub fn from_format(format: QueryResultsFormat) -> Self {
        Self { format }
    }

    /// Results in memory.
    pub fn for_slice(
        self,
        bytes: &[u8],
    ) -> Result<SliceQueryResultsParserOutput<'_>, QueryResultsSyntaxError> {
        let inner = match self.format {
            QueryResultsFormat::Json => {
                let events = SliceJsonParser::from_bytes(bytes)?;
                match JsonSolutions::start(events).map_err(syntax_only)? {
                    (json::Start::Boolean(value), _) => {
                        return Ok(SliceQueryResultsParserOutput::Boolean(value));
                    }
                    (json::Start::Solutions, solutions) => SliceInner::Json(solutions),
                }
            }
            QueryResultsFormat::Xml => match XmlSolutions::start(bytes).map_err(syntax_only)? {
                (xml::Start::Boolean(value), _) => {
                    return Ok(SliceQueryResultsParserOutput::Boolean(value));
                }
                (xml::Start::Solutions, solutions) => SliceInner::Xml(solutions),
            },
            QueryResultsFormat::Tsv => {
                let mut at = 0;
                let head = match tsv::next_slice_line(bytes, &mut at) {
                    Some(line) => tsv::parse_head(line?)?,
                    None => tsv::Head::Variables(Vec::new()),
                };
                match head {
                    tsv::Head::Boolean(value) => {
                        return Ok(SliceQueryResultsParserOutput::Boolean(value));
                    }
                    tsv::Head::Variables(variables) => SliceInner::Tsv {
                        bytes,
                        at,
                        line: 1,
                        variables: variables.into(),
                    },
                }
            }
            QueryResultsFormat::Csv => return Err(csv_error()),
        };
        Ok(SliceQueryResultsParserOutput::Solutions(
            SliceSolutionsParser { inner, done: false },
        ))
    }

    /// Results from a reader (buffered here; pass the bare reader).
    pub fn for_reader<R: Read>(
        self,
        reader: R,
    ) -> Result<ReaderQueryResultsParserOutput<R>, QueryResultsParseError> {
        let inner = match self.format {
            QueryResultsFormat::Json => {
                match JsonSolutions::start(ReaderJsonParser::new(reader))? {
                    (json::Start::Boolean(value), _) => {
                        return Ok(ReaderQueryResultsParserOutput::Boolean(value));
                    }
                    (json::Start::Solutions, solutions) => ReaderInner::Json(solutions),
                }
            }
            QueryResultsFormat::Xml => match XmlSolutions::start(BufReader::new(reader))? {
                (xml::Start::Boolean(value), _) => {
                    return Ok(ReaderQueryResultsParserOutput::Boolean(value));
                }
                (xml::Start::Solutions, solutions) => ReaderInner::Xml(solutions),
            },
            QueryResultsFormat::Tsv => {
                let mut reader = BufReader::new(reader);
                let mut buffer = String::new();
                let head = if tsv::next_reader_line(&mut reader, &mut buffer)? {
                    tsv::parse_head(&buffer)?
                } else {
                    tsv::Head::Variables(Vec::new())
                };
                match head {
                    tsv::Head::Boolean(value) => {
                        return Ok(ReaderQueryResultsParserOutput::Boolean(value));
                    }
                    tsv::Head::Variables(variables) => ReaderInner::Tsv {
                        reader,
                        buffer,
                        line: 1,
                        variables: variables.into(),
                    },
                }
            }
            QueryResultsFormat::Csv => return Err(csv_error().into()),
        };
        Ok(ReaderQueryResultsParserOutput::Solutions(
            ReaderSolutionsParser { inner, done: false },
        ))
    }
}

/// Results read from a slice: a boolean, or solutions.
// Made once per document and matched at once: boxing the parser would put an indirection
// in front of every row instead.
#[allow(clippy::large_enum_variant)]
pub enum SliceQueryResultsParserOutput<'a> {
    Solutions(SliceSolutionsParser<'a>),
    Boolean(bool),
}

enum SliceInner<'a> {
    Json(JsonSolutions<SliceJsonParser<'a>>),
    Xml(XmlSolutions<&'a [u8]>),
    Tsv {
        bytes: &'a [u8],
        at: usize,
        line: u64,
        variables: Arc<[Variable]>,
    },
}

/// The solutions of a slice, one at a time; it stops after the first error.
pub struct SliceSolutionsParser<'a> {
    inner: SliceInner<'a>,
    done: bool,
}

impl SliceSolutionsParser<'_> {
    pub fn variables(&self) -> &[Variable] {
        match &self.inner {
            SliceInner::Json(json) => json.variables(),
            SliceInner::Xml(xml) => xml.variables(),
            SliceInner::Tsv { variables, .. } => variables,
        }
    }

    fn next_row(&mut self) -> Result<Option<Vec<Option<Term>>>, QueryResultsSyntaxError> {
        match &mut self.inner {
            SliceInner::Json(json) => json.next_row().map_err(syntax_only),
            SliceInner::Xml(xml) => xml.next_row().map_err(syntax_only),
            SliceInner::Tsv {
                bytes,
                at,
                line,
                variables,
            } => {
                let Some(text) = tsv::next_slice_line(bytes, at) else {
                    return Ok(None);
                };
                let text = text?;
                *line += 1;
                // A last line with nothing on it is the file's final line break.
                if text.trim_end_matches('\r').is_empty()
                    && *at >= bytes.len()
                    && !variables.is_empty()
                {
                    return Ok(None);
                }
                tsv::parse_row(text, variables.len(), *line - 1).map(Some)
            }
        }
    }
}

impl Iterator for SliceSolutionsParser<'_> {
    type Item = Result<QuerySolution, QueryResultsSyntaxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let variables: Arc<[Variable]> = match &self.inner {
            SliceInner::Json(json) => json.variables().clone(),
            SliceInner::Xml(xml) => xml.variables().clone(),
            SliceInner::Tsv { variables, .. } => variables.clone(),
        };
        match self.next_row() {
            Ok(Some(row)) => Some(Ok(QuerySolution::from((variables, row)))),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

/// Results read from a reader: a boolean, or solutions.
// Made once per document and matched at once: boxing the parser would put an indirection
// in front of every row instead.
#[allow(clippy::large_enum_variant)]
pub enum ReaderQueryResultsParserOutput<R: Read> {
    Solutions(ReaderSolutionsParser<R>),
    Boolean(bool),
}

enum ReaderInner<R: Read> {
    Json(JsonSolutions<ReaderJsonParser<R>>),
    Xml(XmlSolutions<BufReader<R>>),
    Tsv {
        reader: BufReader<R>,
        buffer: String,
        line: u64,
        variables: Arc<[Variable]>,
    },
}

/// The solutions of a reader, one at a time; it stops after the first error.
pub struct ReaderSolutionsParser<R: Read> {
    inner: ReaderInner<R>,
    done: bool,
}

impl<R: Read> ReaderSolutionsParser<R> {
    pub fn variables(&self) -> &[Variable] {
        match &self.inner {
            ReaderInner::Json(json) => json.variables(),
            ReaderInner::Xml(xml) => xml.variables(),
            ReaderInner::Tsv { variables, .. } => variables,
        }
    }

    fn next_row(&mut self) -> Result<Option<Vec<Option<Term>>>, QueryResultsParseError> {
        match &mut self.inner {
            ReaderInner::Json(json) => json.next_row(),
            ReaderInner::Xml(xml) => xml.next_row(),
            ReaderInner::Tsv {
                reader,
                buffer,
                line,
                variables,
            } => {
                if !tsv::next_reader_line(reader, buffer)? {
                    return Ok(None);
                }
                *line += 1;
                let text = buffer.trim_end_matches(['\r', '\n']);
                if text.is_empty() && !variables.is_empty() {
                    // Only a final line break may be empty: check that nothing follows.
                    let mut rest = String::new();
                    if tsv::next_reader_line(reader, &mut rest)? {
                        return Err(QueryResultsSyntaxError::msg(format!(
                            "an empty line {} in TSV results",
                            *line
                        ))
                        .into());
                    }
                    return Ok(None);
                }
                Ok(Some(tsv::parse_row(text, variables.len(), *line - 1)?))
            }
        }
    }
}

impl<R: Read> Iterator for ReaderSolutionsParser<R> {
    type Item = Result<QuerySolution, QueryResultsParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let variables: Arc<[Variable]> = match &self.inner {
            ReaderInner::Json(json) => json.variables().clone(),
            ReaderInner::Xml(xml) => xml.variables().clone(),
            ReaderInner::Tsv { variables, .. } => variables.clone(),
        };
        match self.next_row() {
            Ok(Some(row)) => Some(Ok(QuerySolution::from((variables, row)))),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}
