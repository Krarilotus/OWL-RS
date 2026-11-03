//! Where a document stops, and what its values are, on documents written by hand:
//!
//! - the parser's documented contract (`RdfParser::recovering`): a read error always stops;
//!   a syntax error stops unless the format goes on after one, whichever layer found it;
//! - a statement that fails gives none of its triples (Turtle 1.2 §7: only a conforming
//!   statement's triples are in the graph);
//! - XML 1.0 §3.3.3 (attribute-value normalisation), §4.5 (an entity's replacement text),
//!   §2.2 (legal characters) and §4.1 (a reference must name a declared entity), for
//!   RDF/XML's property attributes, namespace declarations and text;
//! - RDF4J's Binary RDF: a variable-length integer holds at most 32 bits.
//!
//! Each table checks every row and reports all that fail (`crate::table`).

use std::collections::VecDeque;
use std::io::{self, Read};

use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::n3::N3Parser;
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser};

use crate::table::{Table, same};

/// Each item until the end, or the first nine (a parser that never stops shows as nine).
fn drain<T>(items: impl Iterator<Item = Result<T, RdfParseError>>) -> Vec<Result<T, String>> {
    items
        .take(9)
        .map(|item| item.map_err(|e| e.to_string()))
        .collect()
}

/// `Ok` or `Err` per item: the shape of an outcome.
fn shape<T>(items: &[Result<T, String>]) -> Vec<bool> {
    items.iter().map(Result::is_ok).collect()
}

/// The items have the shape `expected`.
fn shaped<T: std::fmt::Debug>(
    items: &[Result<T, String>],
    expected: &[bool],
) -> Result<(), String> {
    if shape(items) == expected {
        Ok(())
    } else {
        Err(format!("want {expected:?}, got {items:?}"))
    }
}

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn triple(s: &str, p: &str, o: impl Into<Term>) -> Quad {
    Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
}

fn recovering_if(parser: RdfParser, recovering: bool) -> RdfParser {
    if recovering {
        parser.recovering()
    } else {
        parser
    }
}

// --- A failed statement gives none of its triples ---------------------------------------

#[test]
fn a_failed_statement_gives_none_of_its_triples() {
    let after = " <http://e/a> <http://e/b> <http://e/c> .";
    let abc = triple("http://e/a", "http://e/b", iri("http://e/c"));
    let mut table = Table::default();
    // (format, statement, whether recovering goes on with the next statement: not when
    // the failing token is the statement's own `.`, which recovery then can't find)
    for (format, statement, resumes) in [
        (RdfFormat::Turtle, "<http://e/s> <http://e/p> 1, .", false),
        (RdfFormat::Turtle, "<http://e/s> <http://e/p> 1, ] .", true),
        (
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> <<( <http://e/a> <http://e/b> <http://e/c> )>>, ] .",
            true,
        ),
        (
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> 1; <http://e/q> ] .",
            true,
        ),
        (
            RdfFormat::TriG,
            "<http://e/g> { <http://e/s> <http://e/p> 1, ] . }",
            true,
        ),
    ] {
        let document = format!("{statement}{after}");
        let strict = drain(RdfParser::from_format(format).for_slice(document.as_bytes()));
        table.row(false, ("strict", statement), shaped(&strict, &[false]));
        if !resumes {
            continue;
        }
        // Recovering skips the statement, all of it, and goes on (recovery cleared the
        // queue before too).
        let recovering = drain(
            RdfParser::from_format(format)
                .recovering()
                .for_slice(document.as_bytes()),
        );
        let outcome = shaped(&recovering, &[false, true])
            .and_then(|()| same(recovering[1].as_ref().ok(), Some(&abc)));
        table.row(true, ("recovering", statement), outcome);
    }
    // N3's own parser too.
    let n3 = drain(N3Parser::new().for_slice(b"<http://e/s> <http://e/p> 1, ."));
    table.row(false, "N3Parser <s> <p> 1, .", shaped(&n3, &[false]));
    table.done();
}

// --- A read error always stops ------------------------------------------------------------

/// A reader that gives its pieces in order: bytes, or a failure of a kind.
struct Pieces(VecDeque<Result<&'static [u8], io::ErrorKind>>);

impl Read for Pieces {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(piece) = self.0.front_mut() else {
            return Ok(0);
        };
        match piece {
            Err(kind) => {
                let kind = *kind;
                self.0.pop_front();
                Err(io::Error::new(kind, "a test fault"))
            }
            Ok(bytes) => {
                let n = bytes.len().min(out.len());
                out[..n].copy_from_slice(&bytes[..n]);
                *bytes = &bytes[n..];
                if bytes.is_empty() {
                    self.0.pop_front();
                }
                Ok(n)
            }
        }
    }
}

/// A reader that fails on every call.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
}

#[test]
fn a_read_error_ends_the_document() {
    use io::ErrorKind::{Interrupted, Other};
    const AB: &[u8] = b"<urn:a> <urn:p> <urn:b> .\n";
    let mut table = Table::default();
    for format in [
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::Turtle,
        RdfFormat::TriG,
    ] {
        // Turtle and TriG reported a failure met inside a statement already.
        let statement_wise = matches!(format, RdfFormat::Turtle | RdfFormat::TriG);
        for recovering in [false, true] {
            let parser = || recovering_if(RdfParser::from_format(format), recovering);
            for (guard, what, pieces, expected) in [
                // A failure, then a reader that would go on: the document has ended.
                (
                    false,
                    "fails, then reads",
                    vec![Err(Other), Ok(AB)],
                    vec![false],
                ),
                // A line, then a failure inside the next line.
                (
                    statement_wise,
                    "reads, fails inside a statement, reads",
                    vec![
                        Ok(AB),
                        Ok(&b"<urn:c> <urn:p>"[..]),
                        Err(Other),
                        Ok(&b" <urn:d> .\n"[..]),
                    ],
                    vec![true, false],
                ),
                // An interrupted read is retried.
                (
                    true,
                    "interrupted, then reads",
                    vec![Err(Interrupted), Ok(AB)],
                    vec![true],
                ),
            ] {
                let items = drain(parser().for_reader(Pieces(pieces.into())));
                table.row(guard, (format, recovering, what), shaped(&items, &expected));
            }
            let items = drain(parser().for_reader(Broken));
            table.row(
                statement_wise,
                (format, recovering, "always fails"),
                shaped(&items, &[false]),
            );
        }
    }
    table.done();
}

// --- An error the parser's own layer finds stops a strict format --------------------------

#[test]
fn a_statement_in_a_named_graph_stops_a_strict_format() {
    let default = triple("urn:a", "urn:p", iri("urn:b"));
    let trig = "<urn:g> { <urn:s> <urn:p> <urn:o> . } <urn:a> <urn:p> <urn:b> .";
    let mut table = Table::default();
    for (guard, format, document, recovering, expected) in [
        (false, RdfFormat::TriG, trig, false, vec![false]),
        (true, RdfFormat::TriG, trig, true, vec![false, true]),
        // A line-based format goes on after a bad line anyway.
        (
            true,
            RdfFormat::NQuads,
            "<urn:s> <urn:p> <urn:o> <urn:g> .\n<urn:a> <urn:p> <urn:b> .\n",
            false,
            vec![false, true],
        ),
    ] {
        let parser = recovering_if(
            RdfParser::from_format(format).without_named_graphs(),
            recovering,
        );
        let items = drain(parser.for_slice(document.as_bytes()));
        let outcome = shaped(&items, &expected).and_then(|()| match items.get(1) {
            Some(Ok(quad)) => same(quad, &default),
            _ => Ok(()),
        });
        table.row(guard, (format, recovering), outcome);
    }
    table.done();
}

#[test]
fn n3_that_isnt_rdf_stops_the_document() {
    let items = drain(
        RdfParser::from_format(RdfFormat::N3)
            .for_slice(b"?s <urn:p> <urn:o> . <urn:a> <urn:p> <urn:b> ."),
    );
    assert_eq!(shaped(&items, &[false]), Ok(()));
}

// --- Binary RDF: a variable-length integer holds 32 bits ----------------------------------

#[test]
fn a_binary_value_id_beyond_32_bits_is_an_error() {
    let mut document = b"BRDF".to_vec();
    document.extend_from_slice(&2i32.to_be_bytes());
    document.push(5);
    document.extend_from_slice(b"UTF-8");
    // Value 0 is the IRI urn:a.
    document.extend_from_slice(&[3, 0, 1, 5]);
    document.extend_from_slice(b"urn:a");
    // A statement of value 0 thrice, the first id written in five bytes (a valid
    // encoding), then one whose subject id is 2^32: bits a 32-bit integer can't hold.
    for subject in [
        &[0x80, 0x80, 0x80, 0x80, 0x00][..],
        &[0x80, 0x80, 0x80, 0x80, 0x10],
    ] {
        document.push(1);
        document.push(6);
        document.extend_from_slice(subject);
        document.extend_from_slice(&[6, 0, 6, 0, 0]);
    }
    document.push(127);
    let a = triple("urn:a", "urn:a", iri("urn:a"));
    let mut table = Table::default();
    for reader in [false, true] {
        let parser = RdfParser::from_format(RdfFormat::BinaryRdf);
        let items = match reader {
            false => drain(parser.for_slice(&document)),
            true => drain(parser.for_reader(document.as_slice())),
        };
        let outcome =
            shaped(&items, &[true, false]).and_then(|()| same(items[0].as_ref().ok(), Some(&a)));
        table.row(false, ("reader", reader), outcome);
    }
    table.done();
}

// --- RDF/XML: references in attribute values and text -------------------------------------

/// A document whose `http://e/s` has the property attribute `e:p="{value}"`, after the
/// entity declarations `entities`.
fn property_attribute(entities: &str, value: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ {entities} ]>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/s" e:p="{value}"/>
</rdf:RDF>"#
    )
}

fn rdf_xml(document: &str) -> Vec<Result<Quad, String>> {
    drain(RdfParser::from_format(RdfFormat::RdfXml).for_slice(document.as_bytes()))
}

fn literal(value: &str) -> Quad {
    triple(
        "http://e/s",
        "http://e/p",
        Literal::new_simple_literal(value),
    )
}

#[test]
fn an_attribute_value_is_normalised_as_xml_says() {
    let mut table = Table::default();
    for (guard, entities, value, expected) in [
        // A character reference adds its character, whitespace or not.
        (false, "", "a&#xA;b", "a\nb"),
        (false, "", "a&#x9;b&#xD;c&#32;d", "a\tb\rc d"),
        // Whitespace characters written in the value become spaces; CR LF is one break.
        (true, "", "a\nb\tc", "a b c"),
        (false, "", "a\r\nb", "a b"),
        // An entity adds its replacement text, normalised in turn: the LF its declaration
        // made becomes a space, a reference its declaration made is read on inclusion.
        (true, r#"<!ENTITY nl "&#xA;">"#, "a&nl;b", "a b"),
        (false, r#"<!ENTITY ref "&#38;#xA;">"#, "a&ref;b", "a\nb"),
        // An ampersand a reference made is text, never a reference again.
        (true, "", "a&amp;#xA;b", "a&#xA;b"),
        (true, r#"<!ENTITY amp2 "&amp;#xA;">"#, "a&amp2;b", "a&#xA;b"),
        // An entity's value may name one declared after it.
        (
            false,
            r#"<!ENTITY a "x&b;y"> <!ENTITY b "z">"#,
            "&a;",
            "xzy",
        ),
    ] {
        let document = property_attribute(entities, value);
        let outcome = same(rdf_xml(&document), vec![Ok(literal(expected))]);
        table.row(guard, (entities, value), outcome);
    }
    table.done();
}

#[test]
fn a_reference_xml_does_not_allow_is_an_error() {
    let mut table = Table::default();
    for (entities, value) in [
        ("", "&nope;"),
        ("", "&#0;"),
        ("", "&#xFFFE;"),
        ("", "&#xZZ;"),
        ("", "&#X41;"),
        ("", "&#;"),
        (r#"<!ENTITY a "&a;">"#, "&a;"),
        (r#"<!ENTITY a "&b;"> <!ENTITY b "&a;">"#, "&a;"),
        (r#"<!ENTITY a "&nope;">"#, "&a;"),
    ] {
        let document = property_attribute(entities, value);
        table.row(
            false,
            (entities, value),
            shaped(&rdf_xml(&document), &[false]),
        );
    }
    table.done();
}

#[test]
fn an_entity_in_text_is_its_replacement_text() {
    let mut table = Table::default();
    let document = r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ <!ENTITY a "x&b;&#xA;y"> <!ENTITY b "z"> <!ENTITY e "http://e/"> ]>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="&e;">
  <rdf:Description rdf:about="&e;s"><e:p>&a;</e:p></rdf:Description>
</rdf:RDF>"#;
    let outcome = same(rdf_xml(document), vec![Ok(literal("xz\ny"))]);
    table.row(false, "&a; naming &b; declared after it", outcome);
    // A character reference in text names a legal XML character, as in an attribute
    // (quick-xml refused `&#0;` and `&#X41;` before already).
    for (guard, reference) in [(false, "&#xFFFE;"), (true, "&#0;"), (true, "&#X41;")] {
        let document = format!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/s"><e:p>{reference}</e:p></rdf:Description>
</rdf:RDF>"#
        );
        table.row(guard, reference, shaped(&rdf_xml(&document), &[false]));
    }
    table.done();
}

#[test]
fn a_bad_namespace_declaration_skips_its_node_element_when_recovering() {
    let document = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/bad" xmlns:x="&nope;"><e:p><rdf:Description rdf:about="http://e/inner"/></e:p></rdf:Description>
  <rdf:Description rdf:about="http://e/s" e:p="ok"/>
</rdf:RDF>"#;
    let items = drain(
        RdfParser::from_format(RdfFormat::RdfXml)
            .recovering()
            .for_slice(document.as_bytes()),
    );
    let outcome = shaped(&items, &[false, true])
        .and_then(|()| same(items[1].as_ref().ok(), Some(&literal("ok"))));
    assert_eq!(outcome, Ok(()));
}
