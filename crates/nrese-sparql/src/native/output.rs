//! Direct result serialisation: SPARQL 1.1 Query Results JSON, TSV and CSV written from id
//! tables.
//!
//! The general path decodes every id into an owned term, builds a solution per row and
//! hands it to the results serialiser. Here dictionary terms are written from their
//! borrowed views (no allocation), each distinct id is formatted once (a bounded cache of
//! the serialised bytes, since joins repeat ids), and output goes out in 64 KiB chunks. The
//! bytes are exactly those of `sparesults` (same layout, same escaping and number forms),
//! which a differential test checks.

use std::collections::HashMap;
use std::io::Write;

use nrese_engine::{Snapshot, TermId, TermView};
use nrese_exec::{IdTable, UNDEF, computed_index};
use oxrdf::vocab::xsd;
use oxrdf::{Term, Variable};

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

/// A term as a borrowed literal/IRI/blank view.
#[derive(Clone, Copy)]
enum Parts<'a> {
    Iri(&'a str),
    Blank(&'a str),
    Literal {
        value: &'a str,
        language: Option<&'a str>,
        datatype: &'a str,
    },
}

impl<'a> From<TermView<'a>> for Parts<'a> {
    fn from(view: TermView<'a>) -> Self {
        match view {
            TermView::Iri(iri) => Parts::Iri(iri),
            TermView::BlankNode(label) => Parts::Blank(label),
            TermView::String(value) => Parts::Literal {
                value,
                language: None,
                datatype: xsd::STRING.as_str(),
            },
            TermView::LangString { value, language } => Parts::Literal {
                value,
                language: Some(language),
                datatype: "",
            },
            TermView::Typed { value, datatype } => Parts::Literal {
                value,
                language: None,
                datatype,
            },
        }
    }
}

impl<'a> From<&'a Term> for Parts<'a> {
    fn from(term: &'a Term) -> Self {
        match term {
            Term::NamedNode(n) => Parts::Iri(n.as_str()),
            Term::BlankNode(b) => Parts::Blank(b.as_str()),
            Term::Literal(l) => Parts::Literal {
                value: l.value(),
                language: l.language(),
                datatype: l.datatype().as_str(),
            },
        }
    }
}

fn json(parts: Parts<'_>, out: &mut Vec<u8>) {
    match parts {
        Parts::Iri(iri) => {
            out.extend_from_slice(b"{\"type\":\"uri\",\"value\":");
            escaped(iri, out);
            out.push(b'}');
        }
        Parts::Blank(label) => {
            out.extend_from_slice(b"{\"type\":\"bnode\",\"value\":");
            escaped(label, out);
            out.push(b'}');
        }
        Parts::Literal {
            value,
            language,
            datatype,
        } => {
            out.extend_from_slice(b"{\"type\":\"literal\",\"value\":");
            escaped(value, out);
            if let Some(language) = language {
                out.extend_from_slice(b",\"xml:lang\":");
                escaped(language, out);
            } else if datatype != xsd::STRING.as_str() {
                out.extend_from_slice(b",\"datatype\":");
                escaped(datatype, out);
            }
            out.push(b'}');
        }
    }
}

/// A TSV string: quoted, with `\t \n \r \" \\` escaped (as `sparesults`).
fn tsv_quoted(value: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for &b in value.as_bytes() {
        match b {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

/// `[+-]?` followed by the rest, as the Turtle number grammar checks it.
fn unsigned(value: &str) -> &[u8] {
    let bytes = value.as_bytes();
    match bytes.first() {
        Some(b'+' | b'-') => &bytes[1..],
        _ => bytes,
    }
}

fn digits(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|b| b.is_ascii_digit()).count()
}

fn turtle_integer(value: &str) -> bool {
    let v = unsigned(value);
    !v.is_empty() && digits(v) == v.len()
}

fn turtle_decimal(value: &str) -> bool {
    let v = unsigned(value);
    let v = &v[digits(v)..];
    match v.strip_prefix(b".") {
        Some(rest) => !rest.is_empty() && digits(rest) == rest.len(),
        None => false,
    }
}

fn turtle_double(value: &str) -> bool {
    let v = unsigned(value);
    let before = digits(v);
    let mut v = &v[before..];
    let mut after = 0;
    if let Some(rest) = v.strip_prefix(b".") {
        after = digits(rest);
        v = &rest[after..];
    }
    let Some(rest) = v.strip_prefix(b"e").or_else(|| v.strip_prefix(b"E")) else {
        return false;
    };
    let rest = match rest.first() {
        Some(b'+' | b'-') => &rest[1..],
        _ => rest,
    };
    (before > 0 || after > 0) && !rest.is_empty() && digits(rest) == rest.len()
}

fn tsv(parts: Parts<'_>, out: &mut Vec<u8>) {
    match parts {
        Parts::Iri(iri) => {
            out.push(b'<');
            out.extend_from_slice(iri.as_bytes());
            out.push(b'>');
        }
        Parts::Blank(label) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(label.as_bytes());
        }
        Parts::Literal {
            value,
            language: Some(language),
            ..
        } => {
            tsv_quoted(value, out);
            out.push(b'@');
            out.extend_from_slice(language.as_bytes());
        }
        Parts::Literal {
            value, datatype, ..
        } => {
            let bare = (datatype == xsd::BOOLEAN.as_str() && matches!(value, "true" | "false"))
                || (datatype == xsd::INTEGER.as_str() && turtle_integer(value))
                || (datatype == xsd::DECIMAL.as_str() && turtle_decimal(value))
                || (datatype == xsd::DOUBLE.as_str() && turtle_double(value));
            if bare {
                out.extend_from_slice(value.as_bytes());
            } else if datatype == xsd::STRING.as_str() {
                tsv_quoted(value, out);
            } else {
                tsv_quoted(value, out);
                out.extend_from_slice(b"^^<");
                out.extend_from_slice(datatype.as_bytes());
                out.push(b'>');
            }
        }
    }
}

fn csv(parts: Parts<'_>, out: &mut Vec<u8>) {
    match parts {
        Parts::Iri(iri) => out.extend_from_slice(iri.as_bytes()),
        Parts::Blank(label) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(label.as_bytes());
        }
        Parts::Literal { value, .. } => {
            if value
                .bytes()
                .any(|b| matches!(b, b'"' | b',' | b'\n' | b'\r'))
            {
                out.push(b'"');
                for &b in value.as_bytes() {
                    if b == b'"' {
                        out.push(b'"');
                    }
                    out.push(b);
                }
                out.push(b'"');
            } else {
                out.extend_from_slice(value.as_bytes());
            }
        }
    }
}

impl ResultsFormat {
    fn term(self, parts: Parts<'_>, out: &mut Vec<u8>) {
        match self {
            Self::Json => json(parts, out),
            Self::Tsv => tsv(parts, out),
            Self::Csv => csv(parts, out),
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
        let format = self.format;
        if let Some(index) = computed_index(id) {
            if let Some(t) = self.computed.get(index as usize) {
                format.term(t.into(), out);
            }
            return;
        }
        let id = TermId::from_raw(id);
        if self
            .snapshot
            .with_view(id, |v| format.term(v.into(), out))
            .is_none()
            && let Some(t) = self.snapshot.decode(id)
        {
            format.term((&t).into(), out);
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
            }
        }
        if json {
            buffer.extend_from_slice(b"]}}");
        }
        out.write_all(&buffer)
    }
}

/// The JSON document of a boolean result, as `sparesults` writes it.
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
