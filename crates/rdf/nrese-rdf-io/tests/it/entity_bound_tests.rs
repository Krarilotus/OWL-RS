//! RDF/XML entities expand within a bound: the work of including entities stays within
//! 10 times the input read so far plus 8 MiB (a fixed 20 per inclusion besides its text,
//! as libxml2 and Expat bound entity amplification), and one reference produces at most
//! 16 MiB. Past either it is a syntax error at the reference, never a truncation. Real
//! RDF/XML (namespace IRIs as entities) stays far below both.
//!
//! Each table checks every row and reports all that fail (`crate::table`); a row marked
//! `guard` held before the bound too.

use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser};

use crate::table::{Table, same};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// A document of `body` under `rdf:RDF`, after the entity declarations `entities` and
/// `padding` bytes of comment.
fn document(entities: &str, padding: usize, body: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ {entities} ]>
<!--{}-->
<rdf:RDF xmlns:rdf="{RDF}" xmlns:e="http://e/">
{body}
</rdf:RDF>"#,
        "x".repeat(padding)
    )
}

/// `levels` entities `a1`..`a{levels}`, each referencing the one below ten times, over
/// `a0` = `base`.
fn nested(levels: usize, base: &str) -> String {
    let mut entities = format!(r#"<!ENTITY a0 "{base}">"#);
    for level in 1..=levels {
        let below = format!("&a{};", level - 1).repeat(10);
        entities.push_str(&format!(r#" <!ENTITY a{level} "{below}">"#));
    }
    entities
}

/// What the slice path and the reader path read: each item's quad, or the error's message.
fn read(document: &str) -> Result<Vec<Result<Quad, String>>, String> {
    let parse = |reader: bool| -> Vec<Result<Quad, String>> {
        let parser = RdfParser::from_format(RdfFormat::RdfXml);
        let message = |item: Result<Quad, RdfParseError>| item.map_err(|e| e.to_string());
        match reader {
            false => parser
                .for_slice(document.as_bytes())
                .take(20_001)
                .map(message)
                .collect(),
            true => parser
                .for_reader(document.as_bytes())
                .take(20_001)
                .map(message)
                .collect(),
        }
    };
    let (slice, reader) = (parse(false), parse(true));
    let shape =
        |items: &[Result<Quad, String>]| -> Vec<bool> { items.iter().map(Result::is_ok).collect() };
    if shape(&slice) != shape(&reader) {
        return Err(format!(
            "slice and reader differ: {:?} vs {:?}",
            shape(&slice),
            shape(&reader)
        ));
    }
    Ok(slice)
}

/// The document is refused at its first item, with a message holding `why`.
fn refused(document: &str, why: &str) -> Result<(), String> {
    let items = read(document)?;
    match items.first() {
        Some(Err(message)) if message.contains(why) => Ok(()),
        Some(Err(message)) => Err(format!("refused, but not for {why:?}: {message}")),
        Some(Ok(quad)) => Err(format!(
            "read {} item(s), the first {}",
            items.len(),
            short(quad)
        )),
        None => Err("read nothing".to_owned()),
    }
}

/// A quad, its literal cut to a readable length.
fn short(quad: &Quad) -> String {
    let text = quad.to_string();
    match text.char_indices().nth(120) {
        Some((at, _)) => format!("{}… ({} bytes)", &text[..at], text.len()),
        None => text,
    }
}

const RATIO: &str = "10 times the input";
const CEILING: &str = "16 MiB";

#[test]
fn entities_that_amplify_are_refused() {
    let big = format!(r#"<!ENTITY big "{}">"#, "x".repeat(50_000));
    let refs = "&big;".repeat(200);
    let mut table = Table::default();
    for (what, document, why) in [
        // Nested references, 10^8 bytes from under 1 KB (billion laughs, scaled down).
        (
            "nested, in an attribute",
            document(
                &nested(7, "xxxxxxxxxx"),
                0,
                r#"<rdf:Description rdf:about="http://e/s" e:p="&a7;"/>"#,
            ),
            "",
        ),
        (
            "nested, in text",
            document(
                &nested(7, "xxxxxxxxxx"),
                0,
                r#"<rdf:Description rdf:about="http://e/s"><e:p>&a7;</e:p></rdf:Description>"#,
            ),
            "",
        ),
        // Nested empty entities: 10^7 inclusions and no text.
        (
            "nested empty entities",
            document(
                &nested(7, ""),
                0,
                r#"<rdf:Description rdf:about="http://e/s" e:p="&a7;"/>"#,
            ),
            RATIO,
        ),
        // One 50 KB entity, referenced 200 times: 10 MB from 51 KB, each reference
        // below the ceiling.
        (
            "repeated, in an attribute",
            document(
                &big,
                0,
                &format!(r#"<rdf:Description rdf:about="http://e/s" e:p="{refs}"/>"#),
            ),
            RATIO,
        ),
        (
            "repeated, in text",
            document(
                &big,
                0,
                &format!(
                    r#"<rdf:Description rdf:about="http://e/s"><e:p>{refs}</e:p></rdf:Description>"#
                ),
            ),
            RATIO,
        ),
        (
            "repeated, in a namespace declaration",
            document(
                &big,
                0,
                &format!(r#"<rdf:Description rdf:about="http://e/s" xmlns:x="{refs}" e:p="v"/>"#),
            ),
            RATIO,
        ),
        (
            "repeated, in an XML literal's attribute",
            document(
                &big,
                0,
                &format!(
                    r#"<rdf:Description rdf:about="http://e/s"><e:p rdf:parseType="Literal"><e:x a="{refs}"/></e:p></rdf:Description>"#
                ),
            ),
            RATIO,
        ),
        // 17 MiB from one reference, after 2 MB of input: the ratio allows it, the
        // ceiling doesn't.
        (
            "one reference past the ceiling",
            document(
                &format!(
                    r#"<!ENTITY a "{}"> <!ENTITY b "{}">"#,
                    "x".repeat(100 * 1024),
                    "&a;".repeat(174)
                ),
                2_000_000,
                r#"<rdf:Description rdf:about="http://e/s"><e:p>&b;</e:p></rdf:Description>"#,
            ),
            CEILING,
        ),
    ] {
        table.row(false, what, refused(&document, why));
    }
    table.done();
}

#[test]
fn rdf_xml_with_entities_for_namespaces_reads_in_full() {
    let entities = format!(
        r#"<!ENTITY rdf "{RDF}"> <!ENTITY owl "http://www.w3.org/2002/07/owl#"> <!ENTITY rdfs "http://www.w3.org/2000/01/rdf-schema#"> <!ENTITY ex "http://example.org/ontology/">"#
    );
    let mut body = String::new();
    for i in 0..5_000 {
        body.push_str(&format!(
            r#"<rdf:Description rdf:about="&ex;C{i}"><rdf:type rdf:resource="&owl;Class"/><rdfs:subClassOf xmlns:rdfs="&rdfs;" rdf:resource="&owl;Thing"/></rdf:Description>
"#
        ));
    }
    let document = document(&entities, 0, &body);
    let thing: Term = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#Thing").into();
    let last = Quad::new(
        NamedNode::new_unchecked("http://example.org/ontology/C4999"),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
        thing,
        GraphName::DefaultGraph,
    );
    let outcome = read(&document).and_then(|items| {
        let quads: Vec<Quad> = items.into_iter().collect::<Result<_, _>>()?;
        same(quads.len(), 10_000)?;
        same(quads.last(), Some(&last))
    });
    assert_eq!(outcome, Ok(()));
}

#[test]
fn declarations_alone_expand_nothing_and_a_refusal_is_recovered_from() {
    let mut table = Table::default();
    // A billion-laughs DOCTYPE nobody references: nothing is expanded.
    let unused = document(
        &nested(9, "xxxxxxxxxx"),
        0,
        r#"<rdf:Description rdf:about="http://e/s" e:p="v"/>"#,
    );
    let v = Quad::new(
        NamedNode::new_unchecked("http://e/s"),
        NamedNode::new_unchecked("http://e/p"),
        Literal::new_simple_literal("v"),
        GraphName::DefaultGraph,
    );
    let outcome = read(&unused).and_then(|items| same(items, vec![Ok(v.clone())]));
    table.row(true, "unused declarations", outcome);
    // Recovering: the node element with the refused reference is skipped, the next one read.
    let document = document(
        &nested(7, "xxxxxxxxxx"),
        0,
        r#"<rdf:Description rdf:about="http://e/bad" e:p="&a7;"/>
<rdf:Description rdf:about="http://e/s" e:p="v"/>"#,
    );
    let items: Vec<Result<Quad, String>> = RdfParser::from_format(RdfFormat::RdfXml)
        .recovering()
        .for_slice(document.as_bytes())
        .take(5)
        .map(|item| item.map_err(|e| e.to_string()))
        .collect();
    let got: Vec<Option<&Quad>> = items.iter().map(|item| item.as_ref().ok()).collect();
    table.row(
        false,
        "recovering after a refusal",
        same(got, vec![None, Some(&v)]),
    );
    table.done();
}
