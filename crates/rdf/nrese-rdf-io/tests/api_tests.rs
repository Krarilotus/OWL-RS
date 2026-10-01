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

#[test]
fn borrowed_quads_need_no_allocation_per_term() {
    let text = write(RdfFormat::NQuads, &sample());
    let mut parser = RdfParser::from_format(RdfFormat::NQuads).for_slice(&text);
    let mut count = 0;
    while let Some(quad) = parser.next_ref() {
        let quad = quad.unwrap();
        assert!(!quad.predicate.as_str().is_empty());
        count += 1;
    }
    assert_eq!(count, sample().len());
}
