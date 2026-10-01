//! SPARQL Query Results CSV (§2 of the CSV/TSV specification): values only, no term
//! syntax; written, never read (it is lossy).

use nrese_rdf::{TermRef, Variable};

pub(crate) fn write_head(out: &mut Vec<u8>, variables: &[Variable]) {
    for (i, variable) in variables.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(variable.as_str().as_bytes());
    }
    out.extend_from_slice(b"\r\n");
}

pub(crate) fn write_row(out: &mut Vec<u8>, row: &[Option<TermRef<'_>>]) {
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        if let Some(value) = value {
            write_term(out, *value);
        }
    }
    out.extend_from_slice(b"\r\n");
}

pub(crate) fn write_term(out: &mut Vec<u8>, term: TermRef<'_>) {
    match term {
        // Of the characters CSV quotes, an IRI can only hold `,`.
        TermRef::NamedNode(iri) => {
            if memchr::memchr(b',', iri.as_str().as_bytes()).is_some() {
                write_field(out, iri.as_str());
            } else {
                out.extend_from_slice(iri.as_str().as_bytes());
            }
        }
        TermRef::BlankNode(b) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
        TermRef::Literal(literal) => write_field(out, literal.value()),
        // §2.2 (SPARQL 1.2): `<<( s p o )>>` with the parts as TSV writes them, the field
        // quoted as CSV needs.
        TermRef::Triple(_) => {
            let mut text = Vec::new();
            crate::tsv::write_term(&mut text, term);
            write_field(out, &String::from_utf8(text).expect("TSV is UTF-8"));
        }
    }
}

/// A field, quoted (RFC 4180) if it holds `"`, `,`, LF or CR.
fn write_field(out: &mut Vec<u8>, text: &str) {
    // All four are below 64: one bit test per byte, with no early exit so it vectorises.
    const QUOTED: u64 = 1 << b'"' | 1 << b',' | 1 << b'\n' | 1 << b'\r';
    if !text
        .bytes()
        .fold(false, |found, b| found | (b < 64 && QUOTED >> b & 1 == 1))
    {
        out.extend_from_slice(text.as_bytes());
        return;
    }
    out.push(b'"');
    for part in text.split('"').enumerate() {
        if part.0 > 0 {
            out.extend_from_slice(b"\"\"");
        }
        out.extend_from_slice(part.1.as_bytes());
    }
    out.push(b'"');
}
