//! Direct result serialisation: SPARQL Query Results JSON, TSV and CSV written from id
//! tables.
//!
//! The general path decodes every id into an owned term, builds a solution per row and
//! hands it to the results serialiser. Here dictionary terms are written from their
//! borrowed views (no allocation), each distinct id is formatted once (a bounded cache of
//! the serialised bytes, since joins repeat ids), and output goes out in 64 KiB chunks.
//! Each term is written by `nrese_sparql_results::write_term`, so the bytes are those of
//! its serialiser, RDF 1.2 terms included.

use std::collections::HashMap;
use std::io::Write;

use nrese_engine::{Snapshot, TermId, TermView};
use nrese_exec::{IdTable, UNDEF, computed_index};
use nrese_rdf::{BlankNodeRef, LiteralRef, NamedNodeRef, Term, TermRef, Variable};
use nrese_sparql_results::QueryResultsFormat;

/// Serialised terms kept per query.
const CACHE_ENTRIES: usize = 1 << 16;
/// Output is written in chunks of this size.
const CHUNK: usize = 1 << 16;

/// Writes `s` as a JSON string, escaped as `json-event-parser` does: `\` and `"`, and
/// control characters as `\b \f \n \r \t` or `\u00xx`; everything else verbatim.
fn escaped(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'\\' => b"\\\\",
            b'"' => b"\\\"",
            0x08 => b"\\b",
            0x0C => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0..=0x1F => {
                out.extend_from_slice(&bytes[start..i]);
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(b >> 4) as usize],
                    HEX[(b & 15) as usize],
                ]);
                start = i + 1;
                continue;
            }
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        out.extend_from_slice(escape);
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// The results formats written directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsFormat {
    Json,
    Tsv,
    Csv,
}

/// A dictionary view as a borrowed term; `None` for a triple term, whose view is text.
fn view_term(view: TermView<'_>) -> Option<TermRef<'_>> {
    Some(match view {
        TermView::Iri(iri) => NamedNodeRef::new_unchecked(iri).into(),
        TermView::BlankNode(label) => BlankNodeRef::new_unchecked(label).into(),
        TermView::String(value) => LiteralRef::new_simple_literal(value).into(),
        TermView::LangString {
            value,
            language,
            direction: None,
        } => LiteralRef::new_language_tagged_literal_unchecked(value, language).into(),
        TermView::LangString {
            value,
            language,
            direction: Some(direction),
        } => LiteralRef::new_directional_language_tagged_literal_unchecked(
            value, language, direction,
        )
        .into(),
        TermView::Typed { value, datatype } => {
            LiteralRef::new_typed_literal(value, NamedNodeRef::new_unchecked(datatype)).into()
        }
        TermView::Triple(_) => return None,
    })
}

impl ResultsFormat {
    fn results_format(self) -> QueryResultsFormat {
        match self {
            Self::Json => QueryResultsFormat::Json,
            Self::Tsv => QueryResultsFormat::Tsv,
            Self::Csv => QueryResultsFormat::Csv,
        }
    }
}

/// Serialises the solutions of a native SELECT.
pub(super) struct DirectResults<'a> {
    pub snapshot: &'a Snapshot,
    pub computed: &'a [Term],
    pub format: ResultsFormat,
}

impl DirectResults<'_> {
    /// Appends id `id` (a stored, inline or computed term) in the format.
    fn id(&self, id: u64, out: &mut Vec<u8>) {
        let format = self.format.results_format();
        if let Some(index) = computed_index(id) {
            if let Some(t) = self.computed.get(index as usize) {
                nrese_sparql_results::write_term(format, t.as_ref(), out);
            }
            return;
        }
        let id = TermId::from_raw(id);
        let written = self.snapshot.with_view(id, |v| match view_term(v) {
            Some(term) => {
                nrese_sparql_results::write_term(format, term, out);
                true
            }
            None => false,
        });
        // Inline ids and triple terms: through the decoded term.
        if written != Some(true)
            && let Some(t) = self.snapshot.decode(id)
        {
            nrese_sparql_results::write_term(format, t.as_ref(), out);
        }
    }

    /// Writes the result document; `alive` is polled every few thousand rows (it fails
    /// the write when the query is cancelled).
    pub fn write(
        &self,
        vars: &[Variable],
        table: &IdTable,
        out: &mut dyn Write,
        alive: &dyn Fn() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK + 4096);
        let json = self.format == ResultsFormat::Json;
        let (separator, end_of_row): (u8, &[u8]) = match self.format {
            ResultsFormat::Json => (b',', b"}"),
            ResultsFormat::Tsv => (b'\t', b"\n"),
            ResultsFormat::Csv => (b',', b"\r\n"),
        };
        match self.format {
            ResultsFormat::Json => {
                buffer.extend_from_slice(b"{\"head\":{\"vars\":[");
                for (i, v) in vars.iter().enumerate() {
                    if i > 0 {
                        buffer.push(b',');
                    }
                    escaped(v.as_str(), &mut buffer);
                }
                buffer.extend_from_slice(b"]},\"results\":{\"bindings\":[");
            }
            ResultsFormat::Tsv | ResultsFormat::Csv => {
                for (i, v) in vars.iter().enumerate() {
                    if i > 0 {
                        buffer.push(separator);
                    }
                    if self.format == ResultsFormat::Tsv {
                        buffer.push(b'?');
                    }
                    buffer.extend_from_slice(v.as_str().as_bytes());
                }
                buffer.extend_from_slice(end_of_row);
            }
        }
        // JSON object keys per variable, formatted once.
        let keys: Vec<Vec<u8>> = vars
            .iter()
            .map(|v| {
                let mut key = Vec::new();
                if json {
                    escaped(v.as_str(), &mut key);
                    key.push(b':');
                }
                key
            })
            .collect();
        let mut cache: HashMap<u64, Box<[u8]>> = HashMap::new();
        let mut scratch = Vec::new();
        for row in 0..table.len() {
            if row % 4096 == 0 {
                alive()?;
            }
            if json {
                if row > 0 {
                    buffer.push(b',');
                }
                buffer.push(b'{');
            }
            let mut first = true;
            for (column, key) in keys.iter().enumerate() {
                let id = table.get(row, column);
                if json {
                    // Unbound variables are left out of the binding object.
                    if id == UNDEF {
                        continue;
                    }
                    if !first {
                        buffer.push(b',');
                    }
                } else if column > 0 {
                    // Every column has a field; unbound ones are empty.
                    buffer.push(separator);
                }
                first = false;
                if id == UNDEF {
                    continue;
                }
                buffer.extend_from_slice(key);
                if let Some(bytes) = cache.get(&id) {
                    buffer.extend_from_slice(bytes);
                } else {
                    scratch.clear();
                    self.id(id, &mut scratch);
                    buffer.extend_from_slice(&scratch);
                    if cache.len() < CACHE_ENTRIES {
                        cache.insert(id, scratch.as_slice().into());
                    }
                }
            }
            buffer.extend_from_slice(end_of_row);
            if buffer.len() >= CHUNK {
                out.write_all(&buffer)?;
                buffer.clear();
                // A cancelled query stops within one chunk of output.
                alive()?;
            }
        }
        if json {
            buffer.extend_from_slice(b"]}}");
        }
        out.write_all(&buffer)
    }
}

/// The JSON document of a boolean result, as `nrese_sparql_results` writes it.
pub(super) fn boolean(value: bool) -> &'static [u8] {
    if value {
        b"{\"head\":{},\"boolean\":true}"
    } else {
        b"{\"head\":{},\"boolean\":false}"
    }
}

#[cfg(test)]
mod tests {
    use super::escaped;

    #[test]
    fn escaping_matches_json_event_parser() {
        let mut out = Vec::new();
        escaped("a\"b\\c\nd\te\u{1}f\u{1f}é/ü\u{7f}", &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\"a\\\"b\\\\c\\nd\\te\\u0001f\\u001fé/ü\u{7f}\""
        );
    }
}
