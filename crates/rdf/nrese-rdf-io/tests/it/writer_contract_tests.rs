//! What a written document says, checked by reading it back or by its bytes:
//!
//! - a prefix abbreviates IRIs of the namespace it is declared for (N3 and Turtle's
//!   `@prefix`; a reader takes the last declaration of a name);
//! - a serializer that failed never finishes a document as if complete (the serializer's
//!   documented contract);
//! - a `VERSION` label is one string (RDF 1.2 Turtle and N-Triples `versionDirective`:
//!   `'VERSION' STRING_LITERAL_QUOTE`, escapes as `ECHAR`), never statements.
//!
//! Each table checks every row and reports all that fail (`crate::table`).

use std::io::{self, Write};

use nrese_rdf::{BaseDirection, GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::n3::{N3Parser, N3Serializer};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

use crate::table::{Table, same};

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn triple(s: &str, p: &str, o: impl Into<Term>) -> Quad {
    Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
}

#[test]
fn a_rebound_n3_prefix_abbreviates_only_its_last_namespace() {
    let mut table = Table::default();
    for name in ["ex", ""] {
        // (guard, IRIs): the last namespace's IRIs were written right already.
        for (guard, (s, p, o)) in [
            (false, ("http://a/s", "http://a/p", "http://a/o")),
            (true, ("http://b/s", "http://b/p", "http://b/o")),
        ] {
            let quad = triple(s, p, iri(o));
            let mut serializer = N3Serializer::new()
                .with_prefix(name, "http://a/")
                .with_prefix(name, "http://b/");
            serializer.serialize_quad(quad.clone());
            let outcome = serializer
                .finish(Vec::new())
                .map_err(|e| e.to_string())
                .and_then(|document| {
                    let read: Vec<_> = N3Parser::new()
                        .for_slice(&document)
                        .collect::<Result<_, _>>()
                        .map_err(|e| e.to_string())?;
                    let read: Vec<_> = read.into_iter().map(|q| q.into_quad().ok()).collect();
                    same(read, vec![Some(quad)])
                        .map_err(|e| format!("{e} from {}", String::from_utf8_lossy(&document)))
                });
            table.row(guard, (name, s), outcome);
        }
    }
    table.done();
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

/// Expects an error from a call.
fn fails<T>(what: &str, result: io::Result<T>) -> Result<(), String> {
    match result {
        Err(_) => Ok(()),
        Ok(_) => Err(format!("{what} succeeded")),
    }
}

#[test]
fn a_statement_the_format_cant_hold_ends_the_document() {
    let good = triple("urn:s", "urn:p", iri("urn:o"));
    let directional: Term =
        Literal::new_directional_language_tagged_literal_unchecked("x", "ar", BaseDirection::Rtl)
            .into();
    let mut table = Table::default();
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
        let outcome = serializer
            .serialize_quad(&good)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                fails(
                    "the statement it can't hold",
                    serializer.serialize_quad(&bad),
                )
            })
            .and_then(|()| fails("a statement after it", serializer.serialize_quad(&good)))
            .and_then(|()| fails("finish", serializer.finish()));
        table.row(false, format, outcome);
    }
    // N-Triples refuses a named graph before writing anything, and goes on.
    let mut serializer = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(Vec::new());
    let named = Quad::new(iri("urn:s"), iri("urn:p"), iri("urn:o"), iri("urn:g"));
    let outcome = fails("a named graph", serializer.serialize_quad(&named))
        .and_then(|()| serializer.serialize_quad(&good).map_err(|e| e.to_string()))
        .and_then(|()| serializer.finish().map_err(|e| e.to_string()))
        .and_then(|document| same(document, b"<urn:s> <urn:p> <urn:o> .\n".to_vec()));
    table.row(true, "N-Triples named graph", outcome);
    table.done();
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
    let failed = (0..2000).any(|_| serializer.serialize_quad(&quad).is_err());
    assert!(failed, "the writer's fault is reported");
    let outcome = fails(
        "a statement after the fault",
        serializer.serialize_quad(&quad),
    )
    .and_then(|()| fails("finish", serializer.finish()));
    assert_eq!(outcome, Ok(()));
}

#[test]
fn a_version_label_is_one_string() {
    let injected = "1.2\"\n<urn:s> <urn:p> <urn:o> .\n#";
    let quad = triple("urn:a", "urn:p", iri("urn:b"));
    let mut table = Table::default();
    for format in [
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::Turtle,
        RdfFormat::TriG,
    ] {
        // (guard, label, header): "1.2" was written so already.
        for (guard, label, header) in [
            (
                false,
                injected,
                r#"VERSION "1.2\"\n<urn:s> <urn:p> <urn:o> .\n#""#,
            ),
            (false, "a\"b\\c\rd", r#"VERSION "a\"b\\c\rd""#),
            (true, "1.2", r#"VERSION "1.2""#),
        ] {
            for statements in [vec![], vec![quad.clone()]] {
                let mut serializer = RdfSerializer::from_format(format)
                    .with_version(label)
                    .for_writer(Vec::new());
                let outcome = statements
                    .iter()
                    .try_for_each(|quad| serializer.serialize_quad(quad))
                    .and_then(|()| serializer.finish())
                    .map_err(|e| e.to_string())
                    .and_then(|document| String::from_utf8(document).map_err(|e| e.to_string()))
                    .and_then(|text| {
                        if !text.starts_with(&format!("{header}\n")) {
                            return Err(format!("the header of {text:?}"));
                        }
                        let read: Vec<Quad> = RdfParser::from_format(format)
                            .for_slice(text.as_bytes())
                            .collect::<Result<_, _>>()
                            .map_err(|e| format!("{e} in {text:?}"))?;
                        same(read, statements.clone())
                    });
                table.row(guard, (format, label, statements.len()), outcome);
            }
        }
    }
    table.done();
}
