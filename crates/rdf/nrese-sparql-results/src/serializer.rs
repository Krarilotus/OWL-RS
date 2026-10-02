//! Writing results: a boolean, or solutions one at a time into a reused byte buffer that
//! goes to the writer in large pieces.

use std::io::{self, Write};

use nrese_rdf::{TermRef, Variable};

use crate::format::QueryResultsFormat;
use crate::{csv, json, tsv, xml};

/// Bytes collected before they go to the writer.
const FLUSH_AT: usize = 8 * 1024;

/// Writes query results in a [`QueryResultsFormat`].
#[derive(Debug, Clone, Copy)]
pub struct QueryResultsSerializer {
    format: QueryResultsFormat,
    version: Option<&'static str>,
}

impl QueryResultsSerializer {
    pub fn from_format(format: QueryResultsFormat) -> Self {
        Self {
            format,
            version: None,
        }
    }

    /// Announces that the results may use RDF 1.2 (`"1.2"`): JSON's `head` gets a `version`
    /// member (SPARQL 1.2 Query Results JSON §3.1.3). The other formats have no place for it
    /// in the document.
    pub fn with_version(mut self, version: &'static str) -> Self {
        self.version = Some(version);
        self
    }

    /// The result of an `ASK` query.
    pub fn serialize_boolean_to_writer<W: Write>(
        self,
        mut writer: W,
        value: bool,
    ) -> io::Result<W> {
        let mut out = Vec::with_capacity(128);
        match self.format {
            QueryResultsFormat::Json => json::write_boolean(&mut out, value, self.version),
            QueryResultsFormat::Xml => xml::write_boolean(&mut out, value),
            QueryResultsFormat::Csv | QueryResultsFormat::Tsv => {
                out.extend_from_slice(if value { b"true" } else { b"false" });
            }
        }
        writer.write_all(&out)?;
        Ok(writer)
    }

    /// The solutions of a `SELECT` query over `variables`: write them with
    /// [`WriterSolutionsSerializer::serialize`] or [`WriterSolutionsSerializer::serialize_row`],
    /// then [`WriterSolutionsSerializer::finish`].
    pub fn serialize_solutions_to_writer<W: Write>(
        self,
        writer: W,
        variables: Vec<Variable>,
    ) -> io::Result<WriterSolutionsSerializer<W>> {
        let mut out = Vec::with_capacity(FLUSH_AT + 4096);
        match self.format {
            QueryResultsFormat::Json => json::write_head(&mut out, &variables, self.version),
            QueryResultsFormat::Xml => xml::write_head(&mut out, &variables),
            QueryResultsFormat::Csv => csv::write_head(&mut out, &variables),
            QueryResultsFormat::Tsv => tsv::write_head(&mut out, &variables),
        }
        Ok(WriterSolutionsSerializer {
            format: self.format,
            variables,
            out,
            writer,
            first: true,
        })
    }
}

/// Writes solutions; see [`QueryResultsSerializer::serialize_solutions_to_writer`].
#[must_use]
pub struct WriterSolutionsSerializer<W: Write> {
    format: QueryResultsFormat,
    variables: Vec<Variable>,
    out: Vec<u8>,
    writer: W,
    first: bool,
}

impl<W: Write> WriterSolutionsSerializer<W> {
    /// One solution, as the values of named variables (the others are unbound; names not
    /// among the variables are left out).
    pub fn serialize<'a, V: AsRef<str>>(
        &mut self,
        solution: impl IntoIterator<Item = (V, impl Into<TermRef<'a>>)>,
    ) -> io::Result<()> {
        // Most queries have few variables: their row stays on the stack.
        let mut stack = [None; 16];
        let mut heap = Vec::new();
        let row: &mut [Option<TermRef<'a>>] = if self.variables.len() <= stack.len() {
            &mut stack[..self.variables.len()]
        } else {
            heap.resize(self.variables.len(), None);
            &mut heap
        };
        // Values usually come in the variables' order: try the next one first.
        let mut next = 0;
        for (name, value) in solution {
            let name = name.as_ref();
            let i = match self.variables.get(next) {
                Some(v) if v.as_str() == name => Some(next),
                _ => self.variables.iter().position(|v| v.as_str() == name),
            };
            if let Some(i) = i {
                row[i] = Some(value.into());
                next = i + 1;
            }
        }
        self.serialize_row(row)
    }

    /// One solution, as a value or `None` per variable, in their order.
    pub fn serialize_row(&mut self, row: &[Option<TermRef<'_>>]) -> io::Result<()> {
        if row.len() != self.variables.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a row of {} values for {} variables",
                    row.len(),
                    self.variables.len()
                ),
            ));
        }
        let first = std::mem::replace(&mut self.first, false);
        match self.format {
            QueryResultsFormat::Json => json::write_row(&mut self.out, &self.variables, row, first),
            QueryResultsFormat::Xml => xml::write_row(&mut self.out, &self.variables, row),
            QueryResultsFormat::Csv => csv::write_row(&mut self.out, row),
            QueryResultsFormat::Tsv => tsv::write_row(&mut self.out, row),
        }
        if self.out.len() >= FLUSH_AT {
            self.writer.write_all(&self.out)?;
            self.out.clear();
        }
        Ok(())
    }

    /// Writes the end of the document, and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        match self.format {
            QueryResultsFormat::Json => json::write_end(&mut self.out),
            QueryResultsFormat::Xml => xml::write_end(&mut self.out),
            QueryResultsFormat::Csv | QueryResultsFormat::Tsv => {}
        }
        self.writer.write_all(&self.out)?;
        Ok(self.writer)
    }
}

/// One term as it stands in a solution of `format`: a JSON term object, an XML term
/// element, a TSV term, a CSV field. For writers that lay out the rows themselves (the
/// engine writes them from its id tables) and must still write terms exactly as
/// [`WriterSolutionsSerializer`] does.
pub fn write_term(format: QueryResultsFormat, term: TermRef<'_>, out: &mut Vec<u8>) {
    match format {
        QueryResultsFormat::Json => json::write_term(out, term),
        QueryResultsFormat::Xml => xml::write_term(out, term),
        QueryResultsFormat::Csv => csv::write_term(out, term),
        QueryResultsFormat::Tsv => tsv::write_term(out, term),
    }
}
