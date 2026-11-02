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

use nrese_rdf::{BaseDirection, Literal, Term, TermRef, Variable};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, ReaderQueryResultsParserOutput,
    SliceQueryResultsParserOutput, XmlUnrepresentable,
};

const NS: &str = "http://www.w3.org/2005/sparql-results#";
const ITS: &str = "http://www.w3.org/2005/11/its";

/// The rows of a solutions document read from a slice, or the first error.
fn slice_rows(format: QueryResultsFormat, bytes: &[u8]) -> Result<Vec<Vec<Option<Term>>>, String> {
    match QueryResultsParser::from_format(format).for_slice(bytes) {
        Err(error) => Err(error.to_string()),
        Ok(SliceQueryResultsParserOutput::Boolean(_)) => Err("a boolean".to_owned()),
        Ok(SliceQueryResultsParserOutput::Solutions(parser)) => parser
            .map(|row| row.map(|s| s.values().to_vec()).map_err(|e| e.to_string()))
            .collect(),
    }
}

/// The same through a reader, which takes the reader path of every format.
fn reader_rows(format: QueryResultsFormat, bytes: &[u8]) -> Result<Vec<Vec<Option<Term>>>, String> {
    match QueryResultsParser::from_format(format).for_reader(bytes) {
        Err(error) => Err(error.to_string()),
        Ok(ReaderQueryResultsParserOutput::Boolean(_)) => Err("a boolean".to_owned()),
        Ok(ReaderQueryResultsParserOutput::Solutions(parser)) => parser
            .map(|row| row.map(|s| s.values().to_vec()).map_err(|e| e.to_string()))
            .collect(),
    }
}

/// Both paths read the same rows; returns them.
fn rows(format: QueryResultsFormat, document: &str) -> Result<Vec<Vec<Option<Term>>>, String> {
    let slice = slice_rows(format, document.as_bytes());
    let reader = reader_rows(format, document.as_bytes());
    assert_eq!(
        slice.is_ok(),
        reader.is_ok(),
        "{document:?}: slice {slice:?}, reader {reader:?}"
    );
    if let (Ok(slice), Ok(reader)) = (&slice, &reader) {
        assert_eq!(slice, reader, "{document:?}");
    }
    slice
}

fn one() -> Term {
    Literal::new_typed_literal("1", nrese_rdf::vocab::xsd::INTEGER).into()
}

#[test]
fn every_tsv_line_after_the_head_is_a_solution_and_an_empty_field_is_unbound() {
    let tsv = QueryResultsFormat::Tsv;
    // (document, solutions): each line ends with LF; a last line without one still counts.
    let cases: [(&str, Vec<Vec<Option<Term>>>); 8] = [
        ("?x\n", vec![]),
        ("?x\n\n", vec![vec![None]]),
        ("?x\n\n\n", vec![vec![None], vec![None]]),
        ("?x\n\n1\n", vec![vec![None], vec![Some(one())]]),
        ("?x\n1\n\n", vec![vec![Some(one())], vec![None]]),
        ("?x\n1", vec![vec![Some(one())]]),
        ("?x\r\n\r\n", vec![vec![None]]),
        ("?x\t?y\n\t\n", vec![vec![None, None]]),
    ];
    for (document, expected) in cases {
        assert_eq!(rows(tsv, document), Ok(expected), "{document:?}");
    }
    // An empty line is one field: not a solution of two variables.
    assert!(rows(tsv, "?x\t?y\n\n").is_err());
}

#[test]
fn a_tsv_solution_with_its_one_variable_unbound_reads_back() {
    let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Tsv)
        .serialize_solutions_to_writer(Vec::new(), vec![Variable::new_unchecked("x")])
        .unwrap();
    serializer.serialize_row(&[None]).unwrap();
    let bytes = serializer.finish().unwrap();
    assert_eq!(
        rows(
            QueryResultsFormat::Tsv,
            std::str::from_utf8(&bytes).unwrap()
        ),
        Ok(vec![vec![None]])
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
    for document in [
        r#"<sparql xmlns="urn:wrong"><head/><results/></sparql>"#.to_owned(),
        r#"<sparql><head/><results/></sparql>"#.to_owned(),
        format!(r#"<r:sparql xmlns:r="{NS}"><head/><results/></r:sparql>"#),
        format!(r#"<sparql xmlns="{NS}"><r:head xmlns:r="urn:wrong"/><results/></sparql>"#),
        format!(r#"<u:sparql xmlns:r="{NS}"><r:head/><r:results/></u:sparql>"#),
    ] {
        assert!(rows(format, &document).is_err(), "{document}");
    }
    // A prefix bound to the results namespace names its elements.
    let prefixed = format!(
        r#"<r:sparql xmlns:r="{NS}"><r:head><r:variable name="x"/></r:head><r:results><r:result><r:binding name="x"><r:uri>urn:a</r:uri></r:binding></r:result></r:results></r:sparql>"#
    );
    assert_eq!(
        rows(format, &prefixed),
        Ok(vec![vec![Some(
            nrese_rdf::NamedNode::new_unchecked("urn:a").into()
        )]])
    );
    // An element of another namespace that happens to be called `result` is no solution.
    let foreign =
        xml(r#"<results><f:result xmlns:f="urn:f"><f:binding name="x"/></f:result></results>"#);
    assert_eq!(rows(format, &foreign), Ok(vec![]));
}

#[test]
fn only_its_dir_in_the_its_namespace_gives_a_literal_its_direction() {
    let format = QueryResultsFormat::Xml;
    let plain: Term = Literal::new_language_tagged_literal_unchecked("x", "ar").into();
    let rtl: Term =
        Literal::new_directional_language_tagged_literal_unchecked("x", "ar", BaseDirection::Rtl)
            .into();
    for (attributes, expected) in [
        (r#"xmlns:q="urn:q" q:dir="rtl""#.to_owned(), &plain),
        (r#"dir="rtl""#.to_owned(), &plain),
        (format!(r#"xmlns:its="{ITS}" its:dir="rtl""#), &rtl),
        (format!(r#"xmlns:d="{ITS}" d:dir="rtl""#), &rtl),
        (
            format!(r#"xmlns:its="{ITS}" xmlns:q="urn:q" q:dir="ltr" its:dir="rtl""#),
            &rtl,
        ),
    ] {
        let document = literal(&attributes);
        assert_eq!(
            rows(format, &document),
            Ok(vec![vec![Some(expected.clone())]]),
            "{document}"
        );
    }
    // Two attributes with the same expanded name, and an undeclared prefix, aren't XML
    // with namespaces.
    for attributes in [
        format!(r#"xmlns:a="{ITS}" xmlns:b="{ITS}" a:dir="rtl" b:dir="ltr""#),
        r#"q:dir="rtl""#.to_owned(),
    ] {
        let document = literal(&attributes);
        assert!(rows(format, &document).is_err(), "{document}");
    }
}

#[test]
fn an_xml_document_has_one_head_then_one_results_or_boolean() {
    let format = QueryResultsFormat::Xml;
    let solution = r#"<result><binding name="x"><uri>urn:a</uri></binding></result>"#;
    for document in [
        // No head.
        format!(r#"<sparql xmlns="{NS}"><results/></sparql>"#),
        // A second results section, or anything after the first.
        xml(&format!("<results/><results>{solution}</results>")),
        xml("<results/><boolean>true</boolean>"),
        xml("<results/><link href=\"urn:l\"/>"),
        xml("<boolean>true</boolean><results/>"),
        // A second head.
        xml("<head/><results/>"),
    ] {
        assert!(rows(format, &document).is_err(), "{document}");
    }
    let document = xml("<boolean>true</boolean><boolean>false</boolean>");
    assert!(
        QueryResultsParser::from_format(format)
            .for_slice(document.as_bytes())
            .is_err()
    );
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
    for value in ["a\rb", "a\r\nb", "\r", "a\r"] {
        let term: Term = Literal::new_simple_literal(value).into();
        let bytes = written_xml(term.as_ref()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains('\r'), "{text:?}");
        assert_eq!(
            rows(QueryResultsFormat::Xml, text),
            Ok(vec![vec![Some(term)]]),
            "{text:?}"
        );
    }
}

#[test]
fn a_character_xml_cannot_hold_is_refused_and_the_rows_before_it_stand() {
    // Representable: TAB, LF, DEL, the last characters before the gaps.
    for value in ["\t\n\u{7f}", "\u{d7ff}\u{e000}\u{fffd}\u{10ffff}"] {
        let term: Term = Literal::new_simple_literal(value).into();
        assert!(written_xml(term.as_ref()).is_ok(), "{value:?}");
    }
    for (value, refused) in [
        ("a\u{0}b", '\u{0}'),
        ("a\u{1}b", '\u{1}'),
        ("\u{b}", '\u{b}'),
        ("\u{1f} ", '\u{1f}'),
        ("a\u{fffe}", '\u{fffe}'),
        ("\u{ffff}", '\u{ffff}'),
    ] {
        let good: Term = Literal::new_simple_literal("good").into();
        let bad: Term = Literal::new_simple_literal(value).into();
        let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Xml)
            .serialize_solutions_to_writer(Vec::new(), vec![Variable::new_unchecked("x")])
            .unwrap();
        serializer.serialize_row(&[Some(good.as_ref())]).unwrap();
        let error = serializer.serialize_row(&[Some(bad.as_ref())]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{value:?}");
        let typed = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<XmlUnrepresentable>())
            .unwrap_or_else(|| panic!("{value:?}: {error}"));
        assert_eq!(typed.character(), refused);
        // The refused row left nothing behind: the document holds the row before it.
        let bytes = serializer.finish().unwrap();
        assert_eq!(
            rows(
                QueryResultsFormat::Xml,
                std::str::from_utf8(&bytes).unwrap()
            ),
            Ok(vec![vec![Some(good)]]),
            "{value:?}"
        );
    }
}
