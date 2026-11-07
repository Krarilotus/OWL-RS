//! What the result-format specifications say about framing, names and text, checked on
//! documents written by hand (the expected values come from the specifications, not from
//! this crate's writer).
//!
//! - TSV: SPARQL 1.1 Query Results CSV and TSV Formats §2 (EOL is LF; readers cope with
//!   CRLF) and §4.1 (one line per solution; an unbound variable is an empty field).
//! - XML: SPARQL 1.1 Query Results XML Format §2 (`sparql` holds `head`, then `results` or
//!   `boolean`, in the results namespace), Namespaces in XML 1.0 §5–6 (names are
//!   namespace + local name), RDF 1.2's `its:dir` in the ITS namespace, and XML 1.0 §2.2
//!   (the characters a document can hold) and §2.11 (a raw CR reads as LF).
//!
//! Each table checks every row and reports all the rows that fail. A row marked `guard`
//! held before the change too: it keeps what already worked.

use std::fmt::Debug;

use nrese_rdf::{BaseDirection, Literal, NamedNode, Term, TermRef, Variable};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, ReaderQueryResultsParserOutput,
    SliceQueryResultsParserOutput, XmlUnrepresentable,
};

const NS: &str = "http://www.w3.org/2005/sparql-results#";
const ITS: &str = "http://www.w3.org/2005/11/its";

/// A table's failing rows, reported together at its end.
#[derive(Default)]
struct Table(Vec<String>);

impl Table {
    /// Row `label` (a guard if `guard`): its outcome.
    fn row(&mut self, guard: bool, label: impl Debug, outcome: Result<(), String>) {
        if let Err(why) = outcome {
            let kind = if guard { "guard " } else { "" };
            self.0.push(format!("{kind}{label:?}: {why}"));
        }
    }

    fn done(self) {
        assert!(
            self.0.is_empty(),
            "{} row(s) failed:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

fn same<T: PartialEq + Debug>(got: T, want: T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("got {got:?}, want {want:?}"))
    }
}

type Rows = Vec<Vec<Option<Term>>>;

/// The rows of a solutions document read from a slice, or the first error.
fn slice_rows(format: QueryResultsFormat, bytes: &[u8]) -> Result<Rows, String> {
    match QueryResultsParser::from_format(format).for_slice(bytes) {
        Err(error) => Err(error.to_string()),
        Ok(SliceQueryResultsParserOutput::Boolean(_)) => Err("a boolean".to_owned()),
        Ok(SliceQueryResultsParserOutput::Solutions(parser)) => parser
            .map(|row| row.map(|s| s.values().to_vec()).map_err(|e| e.to_string()))
            .collect(),
    }
}

/// The same through a reader, which takes the reader path of every format.
fn reader_rows(format: QueryResultsFormat, bytes: &[u8]) -> Result<Rows, String> {
    match QueryResultsParser::from_format(format).for_reader(bytes) {
        Err(error) => Err(error.to_string()),
        Ok(ReaderQueryResultsParserOutput::Boolean(_)) => Err("a boolean".to_owned()),
        Ok(ReaderQueryResultsParserOutput::Solutions(parser)) => parser
            .map(|row| row.map(|s| s.values().to_vec()).map_err(|e| e.to_string()))
            .collect(),
    }
}

/// What both paths read: their rows when they agree (`Ok(Err(_))` when both refuse), or why
/// they don't.
fn rows(format: QueryResultsFormat, document: &str) -> Result<Result<Rows, ()>, String> {
    match (
        slice_rows(format, document.as_bytes()),
        reader_rows(format, document.as_bytes()),
    ) {
        (Ok(slice), Ok(reader)) if slice == reader => Ok(Ok(slice)),
        (Err(_), Err(_)) => Ok(Err(())),
        (slice, reader) => Err(format!("slice {slice:?}, reader {reader:?}")),
    }
}

/// Both paths read `expected`.
fn reads(format: QueryResultsFormat, document: &str, expected: Rows) -> Result<(), String> {
    same(rows(format, document)?, Ok(expected))
}

/// Both paths refuse the document.
fn refused(format: QueryResultsFormat, document: &str) -> Result<(), String> {
    match rows(format, document)? {
        Err(()) => Ok(()),
        Ok(rows) => Err(format!("read {rows:?}")),
    }
}

fn one() -> Term {
    Literal::new_typed_literal("1", nrese_rdf::vocab::xsd::INTEGER).into()
}

#[test]
fn every_tsv_line_after_the_head_is_a_solution_and_an_empty_field_is_unbound() {
    let tsv = QueryResultsFormat::Tsv;
    let mut table = Table::default();
    // (guard, document, solutions): each line ends with LF; a last line without one counts.
    for (guard, document, expected) in [
        (true, "?x\n", vec![]),
        (false, "?x\n\n", vec![vec![None]]),
        (false, "?x\n\n\n", vec![vec![None], vec![None]]),
        (false, "?x\n\n1\n", vec![vec![None], vec![Some(one())]]),
        (false, "?x\n1\n\n", vec![vec![Some(one())], vec![None]]),
        (true, "?x\n1", vec![vec![Some(one())]]),
        (false, "?x\r\n\r\n", vec![vec![None]]),
        (true, "?x\t?y\n\t\n", vec![vec![None, None]]),
    ] {
        table.row(guard, document, reads(tsv, document, expected));
    }
    // An empty line is one field: not a solution of two variables.
    table.row(false, "?x\t?y\n\n", refused(tsv, "?x\t?y\n\n"));
    table.done();
}

#[test]
fn a_tsv_solution_with_its_one_variable_unbound_reads_back() {
    let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Tsv)
        .serialize_solutions_to_writer(Vec::new(), vec![Variable::new_unchecked("x")])
        .unwrap();
    serializer.serialize_row(&[None]).unwrap();
    let bytes = serializer.finish().unwrap();
    let document = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(
        reads(QueryResultsFormat::Tsv, document, vec![vec![None]]),
        Ok(())
    );
}

fn xml(body: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><sparql xmlns="{NS}"><head><variable name="x"/></head>{body}</sparql>"#
    )
}

fn literal(attributes: &str) -> String {
    xml(&format!(
        r#"<results><result><binding name="x"><literal xml:lang="ar" {attributes}>x</literal></binding></result></results>"#
    ))
}

#[test]
fn xml_elements_count_only_in_the_results_namespace() {
    let format = QueryResultsFormat::Xml;
    let mut table = Table::default();
    for document in [
        r#"<sparql xmlns="urn:wrong"><head/><results/></sparql>"#.to_owned(),
        r#"<sparql><head/><results/></sparql>"#.to_owned(),
        format!(r#"<r:sparql xmlns:r="{NS}"><head/><results/></r:sparql>"#),
        format!(r#"<sparql xmlns="{NS}"><r:head xmlns:r="urn:wrong"/><results/></sparql>"#),
        format!(r#"<u:sparql xmlns:r="{NS}"><r:head/><r:results/></u:sparql>"#),
    ] {
        let outcome = refused(format, &document);
        table.row(false, document, outcome);
    }
    // A prefix bound to the results namespace names its elements.
    let prefixed = format!(
        r#"<r:sparql xmlns:r="{NS}"><r:head><r:variable name="x"/></r:head><r:results><r:result><r:binding name="x"><r:uri>urn:a</r:uri></r:binding></r:result></r:results></r:sparql>"#
    );
    let urn_a: Term = NamedNode::new_unchecked("urn:a").into();
    let outcome = reads(format, &prefixed, vec![vec![Some(urn_a)]]);
    table.row(true, prefixed, outcome);
    // An element of another namespace that happens to be called `result` is no solution.
    let foreign =
        xml(r#"<results><f:result xmlns:f="urn:f"><f:binding name="x"/></f:result></results>"#);
    let outcome = reads(format, &foreign, vec![]);
    table.row(false, foreign, outcome);
    table.done();
}

#[test]
fn only_its_dir_in_the_its_namespace_gives_a_literal_its_direction() {
    let format = QueryResultsFormat::Xml;
    let plain: Term = Literal::new_language_tagged_literal_unchecked("x", "ar").into();
    let rtl: Term =
        Literal::new_directional_language_tagged_literal_unchecked("x", "ar", BaseDirection::Rtl)
            .into();
    let mut table = Table::default();
    for (guard, attributes, expected) in [
        (false, r#"xmlns:q="urn:q" q:dir="rtl""#.to_owned(), &plain),
        (true, r#"dir="rtl""#.to_owned(), &plain),
        (true, format!(r#"xmlns:its="{ITS}" its:dir="rtl""#), &rtl),
        (true, format!(r#"xmlns:d="{ITS}" d:dir="rtl""#), &rtl),
        (
            false,
            format!(r#"xmlns:its="{ITS}" xmlns:q="urn:q" q:dir="ltr" its:dir="rtl""#),
            &rtl,
        ),
    ] {
        let outcome = reads(
            format,
            &literal(&attributes),
            vec![vec![Some(expected.clone())]],
        );
        table.row(guard, attributes, outcome);
    }
    // Two attributes with the same expanded name, and an undeclared prefix, aren't XML
    // with namespaces.
    for attributes in [
        format!(r#"xmlns:a="{ITS}" xmlns:b="{ITS}" a:dir="rtl" b:dir="ltr""#),
        r#"q:dir="rtl""#.to_owned(),
    ] {
        let outcome = refused(format, &literal(&attributes));
        table.row(false, attributes, outcome);
    }
    table.done();
}

#[test]
fn an_xml_document_has_one_head_then_one_results_or_boolean() {
    let solution = r#"<result><binding name="x"><uri>urn:a</uri></binding></result>"#;
    let mut table = Table::default();
    for document in [
        // No head.
        format!(r#"<sparql xmlns="{NS}"><results/></sparql>"#),
        // A second results section, or anything after the first.
        xml(&format!("<results/><results>{solution}</results>")),
        xml("<results/><boolean>true</boolean>"),
        xml("<results/><link href=\"urn:l\"/>"),
        xml("<boolean>true</boolean><results/>"),
        xml("<boolean>true</boolean><boolean>false</boolean>"),
        // A second head.
        xml("<head/><results/>"),
    ] {
        let outcome = document_refused(&document);
        table.row(false, document, outcome);
    }
    table.done();
}

/// Both paths refuse an XML document, boolean or solutions: at its start or at a row.
fn document_refused(document: &str) -> Result<(), String> {
    let parser = QueryResultsParser::from_format(QueryResultsFormat::Xml);
    let slice = match parser.for_slice(document.as_bytes()) {
        Err(_) => true,
        Ok(SliceQueryResultsParserOutput::Boolean(_)) => false,
        Ok(SliceQueryResultsParserOutput::Solutions(mut rows)) => rows.any(|row| row.is_err()),
    };
    let reader = match parser.for_reader(document.as_bytes()) {
        Err(_) => true,
        Ok(ReaderQueryResultsParserOutput::Boolean(_)) => false,
        Ok(ReaderQueryResultsParserOutput::Solutions(mut rows)) => rows.any(|row| row.is_err()),
    };
    match (slice, reader) {
        (true, true) => Ok(()),
        _ => Err(format!(
            "read: slice refused {slice}, reader refused {reader}"
        )),
    }
}

/// An XML document of one solution binding `x` to `term`, as the serializer writes it.
fn written_xml(term: TermRef<'_>) -> std::io::Result<Vec<u8>> {
    let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Xml)
        .serialize_solutions_to_writer(Vec::new(), vec![Variable::new_unchecked("x")])?;
    serializer.serialize_row(&[Some(term)])?;
    serializer.finish()
}

#[test]
fn a_carriage_return_in_xml_text_is_written_as_a_reference() {
    let mut table = Table::default();
    // A CR at a value's ends was a reference already; inside it, it wasn't.
    for (guard, value) in [
        (false, "a\rb"),
        (false, "a\r\nb"),
        (true, "\r"),
        (true, "a\r"),
    ] {
        let term: Term = Literal::new_simple_literal(value).into();
        let outcome = written_xml(term.as_ref())
            .map_err(|e| e.to_string())
            .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
            .and_then(|text| {
                if text.contains('\r') {
                    return Err(format!("a raw CR in {text:?}"));
                }
                reads(QueryResultsFormat::Xml, &text, vec![vec![Some(term)]])
            });
        table.row(guard, value, outcome);
    }
    table.done();
}

/// Writes the row `good`, then the row `bad`: the refusal of `bad`, and the document
/// `finish` writes after it.
fn refusal(good: &Term, bad: &Term) -> Result<(std::io::Error, Vec<u8>), String> {
    let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Xml)
        .serialize_solutions_to_writer(Vec::new(), vec![Variable::new_unchecked("x")])
        .map_err(|e| e.to_string())?;
    serializer
        .serialize_row(&[Some(good.as_ref())])
        .map_err(|e| e.to_string())?;
    let Err(error) = serializer.serialize_row(&[Some(bad.as_ref())]) else {
        return Err("written".to_owned());
    };
    let bytes = serializer.finish().map_err(|e| e.to_string())?;
    Ok((error, bytes))
}

#[test]
fn a_character_xml_cannot_hold_is_refused_and_the_rows_before_it_stand() {
    let mut table = Table::default();
    // Representable: TAB, LF, DEL, the last characters before the gaps.
    for value in ["\t\n\u{7f}", "\u{d7ff}\u{e000}\u{fffd}\u{10ffff}"] {
        let term: Term = Literal::new_simple_literal(value).into();
        let outcome = written_xml(term.as_ref())
            .map(drop)
            .map_err(|e| e.to_string());
        table.row(true, value, outcome);
    }
    let good: Term = Literal::new_simple_literal("good").into();
    for (value, character) in [
        ("a\u{0}b", '\u{0}'),
        ("a\u{1}b", '\u{1}'),
        ("\u{b}", '\u{b}'),
        ("\u{1f} ", '\u{1f}'),
        ("a\u{fffe}", '\u{fffe}'),
        ("\u{ffff}", '\u{ffff}'),
    ] {
        let bad: Term = Literal::new_simple_literal(value).into();
        let outcome = refusal(&good, &bad).and_then(|(error, bytes)| {
            same(error.kind(), std::io::ErrorKind::InvalidData)?;
            let typed = error
                .get_ref()
                .and_then(|e| e.downcast_ref::<XmlUnrepresentable>())
                .ok_or_else(|| format!("not XmlUnrepresentable: {error}"))?;
            same(typed.character(), character)?;
            // The refused row left nothing behind: the document holds the row before it.
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            reads(
                QueryResultsFormat::Xml,
                &text,
                vec![vec![Some(good.clone())]],
            )
        });
        table.row(false, value, outcome);
    }
    table.done();
}
