//! The parser's and serialiser's interface: inputs, settings, chunks, errors.

use std::collections::BTreeSet;
use std::io::Write;

use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

fn n(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.org/{local}"))
}

/// Statements with every kind of term and the escapes a writer must produce.
fn sample() -> Vec<Quad> {
    let literals = [
        Literal::new_simple_literal("plain"),
        Literal::new_simple_literal("tab\tnew\nline \"quoted\" back\\slash \u{1} é 🦀"),
        Literal::new_language_tagged_literal("hallo", "de-AT").unwrap(),
        Literal::new_typed_literal(
            "42",
            NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"),
        ),
    ];
    let mut quads = Vec::new();
    for (i, literal) in literals.into_iter().enumerate() {
        let graph = if i % 2 == 0 {
            GraphName::DefaultGraph
        } else {
            n("g").into()
        };
        quads.push(Quad::new(n(&format!("s{i}")), n("p"), literal, graph));
    }
    quads.push(Quad::new(
        BlankNode::new_unchecked("b1"),
        n("p"),
        BlankNode::new_unchecked("b2"),
        GraphName::DefaultGraph,
    ));
    quads.push(Quad::new(
        n("s"),
        n("p"),
        n("o"),
        BlankNode::new_unchecked("g1"),
    ));
    quads
}

fn write(format: RdfFormat, quads: &[Quad]) -> Vec<u8> {
    let mut writer = RdfSerializer::from_format(format).for_writer(Vec::new());
    for quad in quads {
        writer.serialize_quad(quad).unwrap();
    }
    writer.finish().unwrap()
}

#[test]
fn n_quads_round_trip() {
    let quads = sample();
    let text = write(RdfFormat::NQuads, &quads);
    let read: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(read, quads);
    // The same through a reader.
    let read: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_reader(text.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(read, quads);
}

#[test]
fn n_triples_has_no_graphs() {
    let triples: Vec<Quad> = sample()
        .into_iter()
        .filter(|q| q.graph_name.is_default_graph())
        .collect();
    let text = write(RdfFormat::NTriples, &triples);
    let read: Vec<Quad> = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(read, triples);
    let mut writer = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(Vec::new());
    assert!(writer.serialize_quad(&sample()[1]).is_err());
    // An N-Quads graph label is an error in N-Triples.
    let error = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(b"<http://e/s> <http://e/p> <http://e/o> <http://e/g> .")
        .next()
        .unwrap();
    assert!(error.is_err());
}

#[test]
fn settings_place_statements() {
    let text = b"<http://e/s> <http://e/p> <http://e/o> .\n<http://e/s> <http://e/p> <http://e/o> <http://e/g> .\n";
    let placed: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .with_default_graph(n("target"))
        .for_slice(text)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(placed[0].graph_name, n("target").into());
    assert_eq!(
        placed[1].graph_name,
        NamedNode::new_unchecked("http://e/g").into()
    );
    let mut strict = RdfParser::from_format(RdfFormat::NQuads)
        .without_named_graphs()
        .for_slice(text);
    assert!(strict.next().unwrap().is_ok());
    let error = strict.next().unwrap().unwrap_err().to_string();
    assert!(error.starts_with("line 2"), "{error}");
}

#[test]
fn renamed_blank_nodes_agree_across_chunks() {
    let mut text = Vec::new();
    for i in 0..2000 {
        writeln!(text, "_:x{} <http://e/p> _:x{} .", i % 7, (i + 1) % 7).unwrap();
    }
    let whole: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    let parser = RdfParser::from_format(RdfFormat::NTriples).rename_blank_nodes();
    let chunks = parser.split_slice_for_parallel_parsing(&text, 5).unwrap();
    assert!(chunks.len() > 1);
    let renamed: Vec<Quad> = chunks
        .into_iter()
        .flat_map(|chunk| chunk.map(Result::unwrap))
        .collect();
    assert_eq!(renamed.len(), 2000);
    // Seven labels, seven fresh names, used consistently.
    let names: BTreeSet<String> = renamed
        .iter()
        .flat_map(|q| [q.subject.to_string(), q.object.to_string()])
        .collect();
    assert_eq!(names.len(), 7);
    assert!(!names.contains("_:x0"));
    // Up to the names, the same statements.
    let (mut a, mut b): (nrese_rdf::Dataset, nrese_rdf::Dataset) =
        (whole.into_iter().collect(), renamed.into_iter().collect());
    a.canonicalize();
    b.canonicalize();
    assert_eq!(a, b);
}

#[test]
fn chunks_of_any_size_cover_the_document_once() {
    let quads = sample();
    let text = write(RdfFormat::NQuads, &quads).repeat(50);
    let expected: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    for parts in [1, 2, 3, 7, 64, 10_000] {
        let read: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
            .split_slice_for_parallel_parsing(&text, parts)
            .unwrap()
            .into_iter()
            .flat_map(|chunk| chunk.map(Result::unwrap))
            .collect();
        assert_eq!(read, expected, "{parts} parts");
    }
    // The same through files.
    let path = std::env::temp_dir().join(format!("nrese-rdf-io-chunks-{}.nq", std::process::id()));
    std::fs::write(&path, &text).unwrap();
    for parts in [1, 4, 13] {
        let read: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
            .split_file_for_parallel_parsing(&path, parts)
            .unwrap()
            .into_iter()
            .flat_map(|chunk| chunk.map(Result::unwrap))
            .collect();
        assert_eq!(read, expected, "{parts} file parts");
    }
    std::fs::remove_file(&path).unwrap();
    assert!(
        RdfParser::from_format(RdfFormat::RdfXml)
            .split_slice_for_parallel_parsing(&text, 2)
            .is_err()
    );
}

/// A parse in chunks names an error's line in the document, not in its chunk (Claros, 13 Oct
/// 2026: an invalid IRI on line 17,794,519 was reported at line 3,751,763 of its chunk).
#[test]
fn chunks_name_the_documents_line() {
    let good = "<http://e/s> <http://e/p> <http://e/o> .\n";
    let mut text = good.repeat(40);
    // \r and \n each end a line, as the reader counts them: three lines here.
    text.push_str("# a comment\r\n\n");
    text.push_str(&good.repeat(40));
    text.push_str("<http://e/s> <http://e/p> <http://e/a%&b> .\n");
    text.push_str(&good.repeat(5));
    // 40 lines, three, 40 more: the bad one is line 84, from 1.
    fn error_line<R: std::io::Read>(chunks: Vec<nrese_rdf_io::QuadParser<'_, R>>) -> u64 {
        let error = chunks
            .into_iter()
            .flat_map(|chunk| chunk.filter_map(Result::err))
            .next()
            .expect("the invalid IRI is refused");
        match error {
            nrese_rdf_io::RdfParseError::Syntax(syntax) => syntax.location().start.line + 1,
            other => panic!("a syntax error, not {other}"),
        }
    }
    for parts in [1, 2, 3, 7, 64] {
        let chunks = RdfParser::from_format(RdfFormat::NTriples)
            .split_slice_for_parallel_parsing(text.as_bytes(), parts)
            .unwrap();
        assert_eq!(error_line(chunks), 84, "{parts} parts");
    }
    let path = std::env::temp_dir().join(format!("nrese-rdf-io-lines-{}.nt", std::process::id()));
    std::fs::write(&path, &text).unwrap();
    for parts in [1, 4, 13] {
        let chunks = RdfParser::from_format(RdfFormat::NTriples)
            .split_file_for_parallel_parsing(&path, parts)
            .unwrap();
        assert_eq!(error_line(chunks), 84, "{parts} file parts");
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn errors_say_where() {
    let text = "<http://e/s> <http://e/p> <http://e/o> .\n# a comment\n<http://e/s> <http://e/p> \"open .\n";
    let mut parser = RdfParser::from_format(RdfFormat::NTriples).for_slice(text.as_bytes());
    assert!(parser.next().unwrap().is_ok());
    let error = parser.next().unwrap().unwrap_err();
    let nrese_rdf_io::RdfParseError::Syntax(syntax) = error else {
        panic!("a syntax error")
    };
    let at = syntax.location().start;
    assert_eq!((at.line, at.column), (2, 26), "{syntax}");
    assert_eq!(at.offset as usize, text.find("\"open").unwrap());
    // Relative IRIs are not N-Triples; unchecked takes them.
    let relative = b"<s> <p> <o> .";
    assert!(
        RdfParser::from_format(RdfFormat::NTriples)
            .for_slice(relative)
            .next()
            .unwrap()
            .is_err()
    );
    let quad = RdfParser::from_format(RdfFormat::NTriples)
        .unchecked()
        .for_slice(relative)
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(quad.object, Term::from(NamedNode::new_unchecked("o")));
}

/// `next_ref` gives the document's quads, borrowed, in order: each one the quad written.
#[test]
fn borrowed_quads_are_the_documents() {
    let quads = sample();
    let text = write(RdfFormat::NQuads, &quads);
    let mut parser = RdfParser::from_format(RdfFormat::NQuads).for_slice(&text);
    let mut expected = quads.iter();
    while let Some(quad) = parser.next_ref() {
        let written = expected.next().expect("no more quads than written");
        assert_eq!(quad.unwrap(), written.as_ref());
    }
    assert_eq!(expected.next(), None, "as many quads as written");
}

/// RDF 1.2's `VERSION` directive: written first when asked for, and read back in every
/// line-based and Turtle-family format (a reader used to take the directive line of
/// N-Triples for a statement and panic).
#[test]
fn version_directives_are_written_and_read() {
    let quad = Quad::new(
        NamedNode::new_unchecked("urn:a"),
        NamedNode::new_unchecked("urn:p"),
        NamedNode::new_unchecked("urn:b"),
        GraphName::DefaultGraph,
    );
    for format in [
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::Turtle,
        RdfFormat::TriG,
    ] {
        let mut writer = RdfSerializer::from_format(format)
            .with_version("1.2")
            .for_writer(Vec::new());
        writer.serialize_quad(&quad).unwrap();
        let document = writer.finish().unwrap();
        assert!(document.starts_with(b"VERSION \"1.2\"\n"), "{format:?}");
        let read: Vec<Quad> = RdfParser::from_format(format)
            .for_slice(&document)
            .collect::<Result<_, _>>()
            .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        assert_eq!(read, std::slice::from_ref(&quad), "{format:?}");
    }
    // A malformed directive is an error, not a statement.
    let bad = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(b"VERSION 1.2\n<urn:a> <urn:p> <urn:b> .\n")
        .collect::<Result<Vec<_>, _>>();
    assert!(bad.is_err());
}
