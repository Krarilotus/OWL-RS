//! What a written document says, checked by reading it back or by its bytes:
//!
//! - a prefix abbreviates IRIs of the namespace it is declared for (N3 and Turtle's
//!   `@prefix`; a reader takes the last declaration of a name);
//! - a serializer that failed never finishes a document as if complete (the serializer's
//!   documented contract);
//! - a `VERSION` label is one string (RDF 1.2 Turtle and N-Triples `versionDirective`:
//!   `'VERSION' STRING_LITERAL_QUOTE`, escapes as `ECHAR`), never statements.

use std::io::{self, Write};

use nrese_rdf::{BaseDirection, GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::n3::{N3Parser, N3Serializer};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn triple(s: &str, p: &str, o: impl Into<Term>) -> Quad {
    Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
}

#[test]
fn a_rebound_n3_prefix_abbreviates_only_its_last_namespace() {
    for name in ["ex", ""] {
        for (s, p, o) in [
            ("http://a/s", "http://a/p", "http://a/o"),
            ("http://b/s", "http://b/p", "http://b/o"),
        ] {
            let quad = triple(s, p, iri(o));
            let mut serializer = N3Serializer::new()
                .with_prefix(name, "http://a/")
                .with_prefix(name, "http://b/");
            serializer.serialize_quad(quad.clone());
            let document = serializer.finish(Vec::new()).unwrap();
            let read: Vec<_> = N3Parser::new()
                .for_slice(&document)
                .collect::<Result<_, _>>()
                .unwrap();
            let text = String::from_utf8_lossy(&document);
            assert_eq!(read.len(), 1, "{text}");
            assert_eq!(read[0].clone().into_quad().ok(), Some(quad), "{text}");
        }
    }
}

/// A writer that takes `limit` bytes, fails once, then takes everything.
struct FailsOnce {
    taken: Vec<u8>,
    limit: usize,
    failed: bool,
}

impl Write for FailsOnce {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.failed && self.taken.len() + bytes.len() > self.limit {
            let room = self.limit - self.taken.len();
            if room == 0 {
                self.failed = true;
                return Err(io::Error::other("a test fault"));
            }
            self.taken.extend_from_slice(&bytes[..room]);
            return Ok(room);
        }
        self.taken.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_statement_the_format_cant_hold_ends_the_document() {
    let good = triple("urn:s", "urn:p", iri("urn:o"));
    let directional: Term =
        Literal::new_directional_language_tagged_literal_unchecked("x", "ar", BaseDirection::Rtl)
            .into();
    for (format, bad) in [
        (RdfFormat::BinaryRdf, triple("urn:s", "urn:p", directional)),
        (
            RdfFormat::RdfXml,
            triple("urn:s", "urn:p", Literal::new_simple_literal("a\u{1}b")),
        ),
        (
            RdfFormat::Turtle,
            Quad::new(iri("urn:s"), iri("urn:p"), iri("urn:o"), iri("urn:g")),
        ),
    ] {
        let mut serializer = RdfSerializer::from_format(format).for_writer(Vec::new());
        serializer.serialize_quad(&good).unwrap();
        assert!(serializer.serialize_quad(&bad).is_err(), "{format:?}");
        assert!(serializer.serialize_quad(&good).is_err(), "{format:?}");
        assert!(serializer.finish().is_err(), "{format:?}");
    }
    // N-Triples refuses a named graph before writing anything, and goes on.
    let mut serializer = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(Vec::new());
    let named = Quad::new(iri("urn:s"), iri("urn:p"), iri("urn:o"), iri("urn:g"));
    assert!(serializer.serialize_quad(&named).is_err());
    serializer.serialize_quad(&good).unwrap();
    let document = serializer.finish().unwrap();
    assert_eq!(document, b"<urn:s> <urn:p> <urn:o> .\n");
}

#[test]
fn a_failed_write_ends_the_document() {
    // Enough statements to pass the serializer's buffer, written to a writer that takes
    // some bytes, fails once, then would take the rest.
    let mut serializer = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(FailsOnce {
        taken: Vec::new(),
        limit: 1000,
        failed: false,
    });
    let quad = triple(
        "urn:s",
        "urn:p",
        Literal::new_simple_literal("x".repeat(100)),
    );
    let mut failed = false;
    for _ in 0..2000 {
        if serializer.serialize_quad(&quad).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed, "the writer's fault is reported");
    assert!(serializer.serialize_quad(&quad).is_err());
    assert!(serializer.finish().is_err());
}

#[test]
fn a_version_label_is_one_string() {
    let injected = "1.2\"\n<urn:s> <urn:p> <urn:o> .\n#";
    let quad = triple("urn:a", "urn:p", iri("urn:b"));
    for format in [
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::Turtle,
        RdfFormat::TriG,
    ] {
        for (label, header) in [
            (injected, r#"VERSION "1.2\"\n<urn:s> <urn:p> <urn:o> .\n#""#),
            ("a\"b\\c\rd", r#"VERSION "a\"b\\c\rd""#),
            ("1.2", r#"VERSION "1.2""#),
        ] {
            for statements in [vec![], vec![quad.clone()]] {
                let mut serializer = RdfSerializer::from_format(format)
                    .with_version(label)
                    .for_writer(Vec::new());
                for quad in &statements {
                    serializer.serialize_quad(quad).unwrap();
                }
                let document = serializer.finish().unwrap();
                let text = String::from_utf8(document).unwrap();
                assert!(
                    text.starts_with(&format!("{header}\n")),
                    "{format:?} {label:?}: {text:?}"
                );
                let read: Vec<Quad> = RdfParser::from_format(format)
                    .for_slice(text.as_bytes())
                    .collect::<Result<_, _>>()
                    .unwrap_or_else(|e| panic!("{format:?} {label:?}: {e}\n{text}"));
                assert_eq!(read, statements, "{format:?} {label:?}: {text:?}");
            }
        }
    }
}
