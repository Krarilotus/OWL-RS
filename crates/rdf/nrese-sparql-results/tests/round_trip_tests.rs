//! Results written in each format read back the same: from a slice, and through a reader
//! that gives one byte per call (so every token is cut by the buffer somewhere).

use std::io::Read;

use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, BlankNode, Literal, NamedNode, Term, Triple, Variable};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, QuerySolution,
    ReaderQueryResultsParserOutput, SliceQueryResultsParserOutput,
};

struct Trickle<'a>(&'a [u8]);

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match (self.0.split_first(), buf.first_mut()) {
            (Some((b, rest)), Some(slot)) => {
                *slot = *b;
                self.0 = rest;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

fn terms() -> Vec<Term> {
    let iri = NamedNode::new_unchecked("http://example.com/a?b=c&d='e'");
    vec![
        iri.clone().into(),
        BlankNode::new_unchecked("b0").into(),
        Literal::new_simple_literal("plain").into(),
        Literal::new_simple_literal(" \t edges and \"quotes\" & <tags> ' \\ é 😀\n ").into(),
        Literal::new_simple_literal("").into(),
        Literal::new_simple_literal("   ").into(),
        Literal::new_simple_literal("control \u{1} \u{7f}").into(),
        Literal::new_language_tagged_literal_unchecked("chat", "fr").into(),
        Literal::new_directional_language_tagged_literal_unchecked("قطة", "ar", BaseDirection::Rtl)
            .into(),
        Literal::new_typed_literal("42", xsd::INTEGER).into(),
        Literal::new_typed_literal("4.2", xsd::DECIMAL).into(),
        Literal::new_typed_literal("4.2E1", xsd::DOUBLE).into(),
        Literal::new_typed_literal("true", xsd::BOOLEAN).into(),
        Literal::new_typed_literal("not a number", xsd::INTEGER).into(),
        Literal::new_typed_literal("x", NamedNode::new_unchecked("http://example.com/dt")).into(),
        Triple::new(
            BlankNode::new_unchecked("s"),
            iri.clone(),
            Triple::new(
                iri.clone(),
                iri,
                Literal::new_language_tagged_literal_unchecked("o", "en"),
            ),
        )
        .into(),
    ]
}

/// Solutions over three variables: every term in every column, and unbound ones.
fn solutions() -> (Vec<Variable>, Vec<Vec<Option<Term>>>) {
    let variables = vec![
        Variable::new_unchecked("a"),
        Variable::new_unchecked("b"),
        Variable::new_unchecked("c"),
    ];
    let terms = terms();
    let mut rows = Vec::new();
    for (i, term) in terms.iter().enumerate() {
        rows.push(vec![
            Some(term.clone()),
            None,
            Some(terms[(i + 1) % terms.len()].clone()),
        ]);
    }
    rows.push(vec![None, None, None]);
    (variables, rows)
}

fn write(
    format: QueryResultsFormat,
    variables: &[Variable],
    rows: &[Vec<Option<Term>>],
) -> Vec<u8> {
    let mut serializer = QueryResultsSerializer::from_format(format)
        .serialize_solutions_to_writer(Vec::new(), variables.to_vec())
        .unwrap();
    for row in rows {
        let row: Vec<_> = row.iter().map(|t| t.as_ref().map(Term::as_ref)).collect();
        serializer.serialize_row(&row).unwrap();
    }
    serializer.finish().unwrap()
}

fn read_slice(format: QueryResultsFormat, bytes: &[u8]) -> (Vec<Variable>, Vec<QuerySolution>) {
    let SliceQueryResultsParserOutput::Solutions(parser) = QueryResultsParser::from_format(format)
        .for_slice(bytes)
        .unwrap()
    else {
        panic!("expected solutions");
    };
    let variables = parser.variables().to_vec();
    (variables, parser.collect::<Result<_, _>>().unwrap())
}

fn read_trickling(format: QueryResultsFormat, bytes: &[u8]) -> (Vec<Variable>, Vec<QuerySolution>) {
    let ReaderQueryResultsParserOutput::Solutions(parser) = QueryResultsParser::from_format(format)
        .for_reader(Trickle(bytes))
        .unwrap()
    else {
        panic!("expected solutions");
    };
    let variables = parser.variables().to_vec();
    (variables, parser.collect::<Result<_, _>>().unwrap())
}

#[test]
fn every_readable_format_round_trips() {
    let (variables, rows) = solutions();
    for format in [
        QueryResultsFormat::Json,
        QueryResultsFormat::Xml,
        QueryResultsFormat::Tsv,
    ] {
        let bytes = write(format, &variables, &rows);
        for (read_variables, read_rows) in
            [read_slice(format, &bytes), read_trickling(format, &bytes)]
        {
            assert_eq!(read_variables, variables, "{format}");
            let read: Vec<Vec<Option<Term>>> =
                read_rows.iter().map(|s| s.values().to_vec()).collect();
            assert_eq!(read, rows, "{format}:\n{}", String::from_utf8_lossy(&bytes));
        }
    }
}

#[test]
fn booleans_round_trip() {
    for format in [
        QueryResultsFormat::Json,
        QueryResultsFormat::Xml,
        QueryResultsFormat::Tsv,
    ] {
        for value in [true, false] {
            let bytes = QueryResultsSerializer::from_format(format)
                .serialize_boolean_to_writer(Vec::new(), value)
                .unwrap();
            let parsed = QueryResultsParser::from_format(format)
                .for_slice(&bytes)
                .unwrap();
            assert!(
                matches!(parsed, SliceQueryResultsParserOutput::Boolean(v) if v == value),
                "{format}"
            );
        }
    }
}

#[test]
fn written_bytes_are_those_of_the_common_layout() {
    let variables = vec![Variable::new_unchecked("x"), Variable::new_unchecked("y")];
    let rows = vec![vec![
        Some(NamedNode::new_unchecked("http://e/s").into()),
        Some(Literal::new_language_tagged_literal_unchecked("a", "en").into()),
    ]];
    let text = |format| String::from_utf8(write(format, &variables, &rows)).unwrap();
    assert_eq!(
        text(QueryResultsFormat::Json),
        r#"{"head":{"vars":["x","y"]},"results":{"bindings":[{"x":{"type":"uri","value":"http://e/s"},"y":{"type":"literal","value":"a","xml:lang":"en"}}]}}"#
    );
    assert_eq!(
        text(QueryResultsFormat::Xml),
        r#"<?xml version="1.0"?><sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/><variable name="y"/></head><results><result><binding name="x"><uri>http://e/s</uri></binding><binding name="y"><literal xml:lang="en">a</literal></binding></result></results></sparql>"#
    );
    assert_eq!(
        text(QueryResultsFormat::Tsv),
        "?x\t?y\n<http://e/s>\t\"a\"@en\n"
    );
    assert_eq!(text(QueryResultsFormat::Csv), "x,y\r\nhttp://e/s,a\r\n");
}

#[test]
fn csv_quotes_and_writes_triple_terms_as_the_specification_says() {
    let iri = NamedNode::new_unchecked("http://e/p");
    let rows = vec![vec![
        Some(Literal::new_simple_literal("a,\"b\"").into()),
        Some(Triple::new(iri.clone(), iri.clone(), Literal::new_simple_literal("o")).into()),
    ]];
    let variables = vec![Variable::new_unchecked("x"), Variable::new_unchecked("y")];
    let text = String::from_utf8(write(QueryResultsFormat::Csv, &variables, &rows)).unwrap();
    assert_eq!(
        text,
        "x,y\r\n\"a,\"\"b\"\"\",\"<<( <http://e/p> <http://e/p> \"\"o\"\" )>>\"\r\n"
    );
    assert!(
        QueryResultsParser::from_format(QueryResultsFormat::Csv)
            .for_slice(text.as_bytes())
            .is_err()
    );
}

#[test]
fn json_members_in_any_order() {
    let document = r#"{"results": {"bindings": [{"x": {"value": "v", "type": "literal"}}]},
                       "head": {"link": ["http://e/doc"], "vars": ["x"]}}"#;
    let (variables, rows) = read_slice(QueryResultsFormat::Json, document.as_bytes());
    assert_eq!(variables, vec![Variable::new_unchecked("x")]);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("x"),
        Some(&Literal::new_simple_literal("v").into())
    );
}

#[test]
fn xml_with_a_prefixed_namespace() {
    let document = r#"<?xml version="1.0"?>
        <r:sparql xmlns:r="http://www.w3.org/2005/sparql-results#">
          <r:head><r:variable name="x"/></r:head>
          <r:results>
            <r:result><r:binding name="x"><r:literal datatype="http://www.w3.org/2001/XMLSchema#integer">1</r:literal></r:binding></r:result>
          </r:results>
        </r:sparql>"#;
    let (_, rows) = read_slice(QueryResultsFormat::Xml, document.as_bytes());
    assert_eq!(
        rows[0].get("x"),
        Some(&Literal::new_typed_literal("1", xsd::INTEGER).into())
    );
}

#[test]
fn errors_say_what_is_wrong() {
    for (format, document) in [
        (
            QueryResultsFormat::Json,
            r#"{"head": {"vars": ["x"]}, "results": {"bindings": [{"y": {"type": "uri", "value": "http://e/"}}]}}"#,
        ),
        (
            QueryResultsFormat::Json,
            r#"{"head": {"vars": ["x"]}, "results": {"bindings": [{"x": {"type": "uri", "value": "not an iri"}}]}}"#,
        ),
        (
            QueryResultsFormat::Json,
            r#"{"head": {"vars": ["x"]}, "results": {"bindings": [{"x": {"type": "literal", "value": "a", "xml:lang": "en", "its:dir": "up"}}]}}"#,
        ),
        (
            QueryResultsFormat::Xml,
            r#"<sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/></head><results><result><binding name="x"><unknown/></binding></result></results></sparql>"#,
        ),
        (QueryResultsFormat::Tsv, "?x\n<http://e/>\textra\n"),
    ] {
        let failed = match QueryResultsParser::from_format(format).for_slice(document.as_bytes()) {
            Err(_) => true,
            Ok(SliceQueryResultsParserOutput::Solutions(parser)) => {
                parser.into_iter().any(|r| r.is_err())
            }
            Ok(SliceQueryResultsParserOutput::Boolean(_)) => false,
        };
        assert!(failed, "{format}: {document}");
    }
}
