//! The Turtle and TriG writer: streaming, grouping statements that follow each other.
//!
//! Consecutive statements with the same subject continue with `;`, with the same subject and
//! predicate with `,`; in TriG, a change of graph closes one `{ … }` block and opens the
//! next (statements of the default graph stand outside blocks). IRIs become prefixed names
//! where a declared prefix fits and the rest is a plain local name; `rdf:type` is `a`; numbers
//! and booleans are written bare when their lexical form is exactly the Turtle token, so
//! that reading them back gives the same literal. Terms are written straight into the
//! output; the previous subject, predicate and graph are kept to compare with.

use std::io::{self, Write};

use nrese_rdf::term::write_quoted_str;
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    GraphName, GraphNameRef, LiteralRef, NamedNode, NamedNodeRef, NamedOrBlankNode,
    NamedOrBlankNodeRef, QuadRef, TermRef,
};

/// The state of a Turtle or TriG document being written.
pub(crate) struct TurtleWriter {
    trig: bool,
    prefixes: Vec<(String, String)>,
    started: bool,
    /// The graph of the open block (TriG), and the subject and predicate of the previous
    /// statement.
    graph: Option<GraphName>,
    subject: Option<NamedOrBlankNode>,
    predicate: Option<NamedNode>,
}

/// A buffer that formatting writes into (`write!` on a `Vec<u8>` can't fail).
struct Out<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Out<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

impl TurtleWriter {
    pub(crate) fn new(trig: bool, prefixes: Vec<(String, String)>) -> Self {
        Self {
            trig,
            prefixes,
            started: false,
            graph: None,
            subject: None,
            predicate: None,
        }
    }

    pub(crate) fn write(&mut self, out: &mut Vec<u8>, quad: QuadRef<'_>) -> io::Result<()> {
        if !self.started {
            self.started = true;
            for (name, iri) in &self.prefixes {
                writeln!(out, "@prefix {name}: <{iri}> .")?;
            }
            if !self.prefixes.is_empty() {
                out.push(b'\n');
            }
        }
        if !quad.graph_name.is_default_graph() && !self.trig {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Turtle has no named graphs: {quad}"),
            ));
        }
        // The graph: close the open block and open another if it changes.
        let graph = match quad.graph_name {
            GraphNameRef::DefaultGraph => None,
            other => Some(other),
        };
        if graph != self.graph.as_ref().map(GraphName::as_ref) {
            self.end_statement(out);
            if self.graph.is_some() {
                out.extend_from_slice(b"}\n");
            }
            if let Some(graph) = graph {
                match graph {
                    GraphNameRef::NamedNode(n) => self.write_iri(out, n),
                    GraphNameRef::BlankNode(b) => write!(out, "_:{}", b.as_str())?,
                    GraphNameRef::DefaultGraph => {}
                }
                out.extend_from_slice(b" {\n");
            }
            self.graph = graph.map(GraphNameRef::into_owned);
        }
        let indent: &[u8] = if self.graph.is_some() { b"\t" } else { b"" };
        let same_subject =
            self.subject.as_ref().map(NamedOrBlankNode::as_ref) == Some(quad.subject);
        let same_predicate =
            same_subject && self.predicate.as_ref().map(NamedNode::as_ref) == Some(quad.predicate);
        if same_predicate {
            out.extend_from_slice(b" ,\n");
            out.extend_from_slice(indent);
            out.extend_from_slice(b"\t\t");
        } else {
            if same_subject {
                out.extend_from_slice(b" ;\n");
                out.extend_from_slice(indent);
                out.push(b'\t');
            } else {
                self.end_statement(out);
                out.extend_from_slice(indent);
                match quad.subject {
                    NamedOrBlankNodeRef::NamedNode(n) => self.write_iri(out, n),
                    NamedOrBlankNodeRef::BlankNode(b) => write!(out, "_:{}", b.as_str())?,
                }
                out.push(b' ');
                self.subject = Some(quad.subject.into_owned());
            }
            if quad.predicate == rdf::TYPE {
                out.push(b'a');
            } else {
                self.write_iri(out, quad.predicate);
            }
            out.push(b' ');
            self.predicate = Some(quad.predicate.into_owned());
        }
        match quad.object {
            TermRef::NamedNode(n) => self.write_iri(out, n),
            TermRef::BlankNode(b) => write!(out, "_:{}", b.as_str())?,
            TermRef::Literal(literal) => self.write_literal(out, literal),
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, out: &mut Vec<u8>) {
        self.end_statement(out);
        if self.graph.take().is_some() {
            out.extend_from_slice(b"}\n");
        }
    }

    fn end_statement(&mut self, out: &mut Vec<u8>) {
        if self.subject.take().is_some() {
            out.extend_from_slice(b" .\n");
        }
        self.predicate = None;
    }

    /// An IRI: a prefixed name where a prefix fits, otherwise `<…>`.
    fn write_iri(&self, out: &mut Vec<u8>, iri: NamedNodeRef<'_>) {
        let text = iri.as_str();
        for (name, namespace) in &self.prefixes {
            if let Some(local) = text.strip_prefix(namespace.as_str())
                && is_plain_local_name(local)
            {
                out.extend_from_slice(name.as_bytes());
                out.push(b':');
                out.extend_from_slice(local.as_bytes());
                return;
            }
        }
        out.push(b'<');
        out.extend_from_slice(text.as_bytes());
        out.push(b'>');
    }

    fn write_literal(&self, out: &mut Vec<u8>, literal: LiteralRef<'_>) {
        let value = literal.value();
        let datatype = literal.datatype();
        let bare = (datatype == xsd::INTEGER && is_integer(value))
            || (datatype == xsd::DECIMAL && is_decimal(value))
            || (datatype == xsd::DOUBLE && is_double(value))
            || (datatype == xsd::BOOLEAN && matches!(value, "true" | "false"));
        if bare {
            out.extend_from_slice(value.as_bytes());
            return;
        }
        let _ = write_quoted_str(value, &mut Out(out));
        if let Some(language) = literal.language() {
            out.push(b'@');
            out.extend_from_slice(language.as_bytes());
        } else if datatype != xsd::STRING {
            out.extend_from_slice(b"^^");
            self.write_iri(out, datatype);
        }
    }
}

/// A local name that needs no escapes: `[A-Za-z0-9_]` first, then those and `-`, with dots
/// inside but not at the end (a subset of `PN_LOCAL`).
fn is_plain_local_name(local: &str) -> bool {
    let bytes = local.as_bytes();
    match bytes.first() {
        None => true,
        Some(first) if first.is_ascii_alphanumeric() || *first == b'_' => {
            bytes.last() != Some(&b'.')
                && bytes[1..]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        }
        Some(_) => false,
    }
}

/// `[+-]?[0-9]+`
fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `[+-]?[0-9]*\.[0-9]+`
fn is_decimal(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    match unsigned.split_once('.') {
        Some((whole, fraction)) => {
            whole.bytes().all(|b| b.is_ascii_digit())
                && !fraction.is_empty()
                && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// `[+-]?([0-9]+\.[0-9]*|\.[0-9]+|[0-9]+)[eE][+-]?[0-9]+`
fn is_double(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let Some((mantissa, exponent)) = unsigned.split_once(['e', 'E']) else {
        return false;
    };
    let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty())
        }
        None => !mantissa.is_empty() && digits(mantissa),
    };
    mantissa_ok && !exponent.is_empty() && digits(exponent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerals_are_recognised_exactly() {
        assert!(is_integer("-12") && is_integer("007") && !is_integer("1.0") && !is_integer("+"));
        assert!(is_decimal("1.5") && is_decimal(".5") && !is_decimal("1.") && !is_decimal("1"));
        assert!(is_double("1e5") && is_double("1.e5") && is_double(".5E-3") && !is_double("1.5"));
        assert!(!is_double("e5"));
        assert!(
            is_plain_local_name("a-b.c_1")
                && !is_plain_local_name("a.")
                && !is_plain_local_name("-a")
        );
        assert!(!is_plain_local_name("a/b") && !is_plain_local_name("a#b"));
    }
}
