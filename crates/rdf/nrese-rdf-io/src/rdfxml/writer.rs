//! The RDF/XML writer: streaming, one `rdf:Description` per run of statements about a
//! subject.
//!
//! A predicate is written as a qualified name: its IRI split before the longest local part
//! that is an XML name. Namespaces declared with prefixes go on `rdf:RDF`; others are
//! declared on the property element itself. Blank nodes whose labels aren't XML names get
//! names that are (an injective encoding). What XML 1.0 can't hold is an error rather than
//! a silent loss: a statement in a named graph, a predicate with no XML name at its end, a
//! character XML 1.0 doesn't allow.
//!
//! RDF 1.2 (only where the data needs it): a triple term is a property element with
//! `rdf:parseType="Triple"` holding one description, and a directional literal carries
//! `its:dir`; the outermost such element announces `rdf:version="1.2"`, which both need.

use std::io;

use nrese_rdf::vocab::xsd;
use nrese_rdf::{NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, QuadRef, TermRef};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const ITS: &str = "http://www.w3.org/2005/11/its";

pub(crate) struct RdfXmlWriter {
    prefixes: Vec<(String, String)>,
    started: bool,
    subject: Option<NamedOrBlankNode>,
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl RdfXmlWriter {
    pub(crate) fn new(prefixes: Vec<(String, String)>) -> Self {
        Self {
            prefixes,
            started: false,
            subject: None,
        }
    }

    pub(crate) fn write(&mut self, out: &mut Vec<u8>, quad: QuadRef<'_>) -> io::Result<()> {
        if !quad.graph_name.is_default_graph() {
            return Err(invalid(format!("RDF/XML has no named graphs: {quad}")));
        }
        self.start(out);
        if self.subject.as_ref().map(NamedOrBlankNode::as_ref) != Some(quad.subject) {
            self.close_description(out);
            out.push(b'\t');
            description(quad.subject, out)?;
            self.subject = Some(quad.subject.into_owned());
        }
        self.property(out, quad.predicate, quad.object, 2, false)
    }

    /// The property element of `predicate object`, `depth` tabs in; `versioned`: an
    /// enclosing element announces RDF 1.2 already.
    fn property(
        &self,
        out: &mut Vec<u8>,
        predicate: NamedNodeRef<'_>,
        object: TermRef<'_>,
        depth: usize,
        versioned: bool,
    ) -> io::Result<()> {
        // The predicate as a qualified name.
        let predicate = predicate.as_str();
        let split = predicate.len() - local_name_length(predicate);
        let (namespace, local) = predicate.split_at(split);
        if local.is_empty() {
            return Err(invalid(format!(
                "the predicate <{predicate}> can't be an XML name in RDF/XML"
            )));
        }
        let prefix = if namespace == RDF {
            Some("rdf")
        } else {
            self.prefixes
                .iter()
                .find(|(_, ns)| ns == namespace)
                .map(|(p, _)| p.as_str())
        };
        indent(out, depth);
        out.push(b'<');
        let name = match prefix {
            Some(p) => format!("{p}:{local}"),
            None => format!("ns:{local}"),
        };
        out.extend_from_slice(name.as_bytes());
        if prefix.is_none() {
            out.extend_from_slice(b" xmlns:ns=\"");
            escape_attribute(namespace, out)?;
            out.push(b'"');
        }
        let needs_version = match object {
            TermRef::Triple(_) => true,
            TermRef::Literal(literal) => literal.direction().is_some(),
            _ => false,
        };
        if needs_version && !versioned {
            out.extend_from_slice(b" rdf:version=\"1.2\"");
        }
        match object {
            TermRef::NamedNode(n) => {
                out.extend_from_slice(b" rdf:resource=\"");
                escape_attribute(n.as_str(), out)?;
                out.extend_from_slice(b"\"/>\n");
            }
            TermRef::BlankNode(b) => {
                out.extend_from_slice(b" rdf:nodeID=\"");
                node_id(b.as_str(), out);
                out.extend_from_slice(b"\"/>\n");
            }
            TermRef::Literal(literal) => {
                if let Some(language) = literal.language() {
                    out.extend_from_slice(b" xml:lang=\"");
                    escape_attribute(language, out)?;
                    out.push(b'"');
                    if let Some(direction) = literal.direction() {
                        out.extend_from_slice(b" xmlns:its=\"");
                        out.extend_from_slice(ITS.as_bytes());
                        out.extend_from_slice(b"\" its:dir=\"");
                        out.extend_from_slice(direction.as_str().as_bytes());
                        out.push(b'"');
                    }
                } else if literal.datatype() != xsd::STRING {
                    out.extend_from_slice(b" rdf:datatype=\"");
                    escape_attribute(literal.datatype().as_str(), out)?;
                    out.push(b'"');
                }
                out.push(b'>');
                escape_text(literal.value(), out)?;
                out.extend_from_slice(b"</");
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b">\n");
            }
            TermRef::Triple(triple) => {
                out.extend_from_slice(b" rdf:parseType=\"Triple\">\n");
                indent(out, depth + 1);
                description(triple.subject.as_ref(), out)?;
                self.property(
                    out,
                    triple.predicate.as_ref(),
                    triple.object.as_ref(),
                    depth + 2,
                    true,
                )?;
                indent(out, depth + 1);
                out.extend_from_slice(b"</rdf:Description>\n");
                indent(out, depth);
                out.extend_from_slice(b"</");
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b">\n");
            }
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, out: &mut Vec<u8>) {
        self.start(out);
        self.close_description(out);
        out.extend_from_slice(b"</rdf:RDF>\n");
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rdf:RDF xmlns:rdf=\"");
        out.extend_from_slice(RDF.as_bytes());
        out.push(b'"');
        for (prefix, namespace) in &self.prefixes {
            if prefix == "rdf" || prefix.is_empty() || !is_nc_name(prefix) {
                continue;
            }
            out.extend_from_slice(b"\n\txmlns:");
            out.extend_from_slice(prefix.as_bytes());
            out.extend_from_slice(b"=\"");
            let _ = escape_attribute(namespace, out);
            out.push(b'"');
        }
        out.extend_from_slice(b">\n");
    }

    fn close_description(&mut self, out: &mut Vec<u8>) {
        if self.subject.take().is_some() {
            out.extend_from_slice(b"\t</rdf:Description>\n");
        }
    }
}

fn indent(out: &mut Vec<u8>, depth: usize) {
    out.resize(out.len() + depth, b'\t');
}

/// The opening `rdf:Description` element of `subject`, and the line's end.
fn description(subject: NamedOrBlankNodeRef<'_>, out: &mut Vec<u8>) -> io::Result<()> {
    out.extend_from_slice(b"<rdf:Description ");
    match subject {
        NamedOrBlankNodeRef::NamedNode(n) => {
            out.extend_from_slice(b"rdf:about=\"");
            escape_attribute(n.as_str(), out)?;
        }
        NamedOrBlankNodeRef::BlankNode(b) => {
            out.extend_from_slice(b"rdf:nodeID=\"");
            node_id(b.as_str(), out);
        }
    }
    out.extend_from_slice(b"\">\n");
    Ok(())
}

/// The length of the longest suffix of `iri` that is an XML name (0 if none).
fn local_name_length(iri: &str) -> usize {
    let mut best = 0;
    for (at, _) in iri.char_indices().rev() {
        let suffix = &iri[at..];
        if is_nc_name(suffix) {
            best = suffix.len();
        } else if !suffix.chars().next().is_some_and(name_char) {
            break;
        }
    }
    best
}

fn name_start(c: char) -> bool {
    c.is_ascii_alphabetic()
        || c == '_'
        || matches!(c, '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn name_char(c: char) -> bool {
    name_start(c)
        || c.is_ascii_digit()
        || matches!(c, '-' | '.' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

fn is_nc_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(name_start) && chars.all(name_char)
}

/// A blank node label as an XML name: as it is if it is one, otherwise `x` and its bytes in
/// hexadecimal (injective, and never an XML name to begin with, so no collision).
fn node_id(label: &str, out: &mut Vec<u8>) {
    if is_nc_name(label) && !label.starts_with("x_") {
        out.extend_from_slice(label.as_bytes());
    } else {
        out.extend_from_slice(b"x_");
        for b in label.bytes() {
            out.extend_from_slice(format!("{b:02x}").as_bytes());
        }
    }
}

/// Characters XML 1.0 allows.
fn allowed(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn escape_text(text: &str, out: &mut Vec<u8>) -> io::Result<()> {
    for c in text.chars() {
        match c {
            '&' => out.extend_from_slice(b"&amp;"),
            '<' => out.extend_from_slice(b"&lt;"),
            '>' => out.extend_from_slice(b"&gt;"),
            '\r' => out.extend_from_slice(b"&#xD;"),
            c if allowed(c) => {
                let mut buffer = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
            c => {
                return Err(invalid(format!(
                    "XML 1.0 can't hold the character U+{:04X}",
                    c as u32
                )));
            }
        }
    }
    Ok(())
}

fn escape_attribute(text: &str, out: &mut Vec<u8>) -> io::Result<()> {
    for c in text.chars() {
        match c {
            '&' => out.extend_from_slice(b"&amp;"),
            '<' => out.extend_from_slice(b"&lt;"),
            '"' => out.extend_from_slice(b"&quot;"),
            '\t' => out.extend_from_slice(b"&#x9;"),
            '\n' => out.extend_from_slice(b"&#xA;"),
            '\r' => out.extend_from_slice(b"&#xD;"),
            c if allowed(c) => {
                let mut buffer = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
            c => {
                return Err(invalid(format!(
                    "XML 1.0 can't hold the character U+{:04X}",
                    c as u32
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_and_node_ids() {
        assert_eq!(local_name_length("http://e/ns#name"), 4);
        assert_eq!(local_name_length("http://e/ns/a-b.c"), 5);
        assert_eq!(local_name_length("http://e/ns/123"), 0);
        assert_eq!(local_name_length("http://e/ns/1abc"), 3);
        let mut out = Vec::new();
        node_id("b1", &mut out);
        node_id("1b", &mut out);
        assert_eq!(out, b"b1x_3162");
    }
}
