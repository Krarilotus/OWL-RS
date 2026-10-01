//! Turtle and TriG beyond the W3C suites: input from readers, limits, errors, settings, and
//! the writer's output.

use std::collections::BTreeSet;
use std::io::Read;

use nrese_rdf::{Dataset, Quad};
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser, RdfSerializer};

fn parse(format: RdfFormat, text: &str) -> Result<Vec<Quad>, RdfParseError> {
    RdfParser::from_format(format)
        .for_slice(text.as_bytes())
        .collect()
}

/// A reader that gives one byte per call.
struct Trickle<'a>(&'a [u8]);

impl Read for Trickle<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.0.split_first() {
            Some((&b, rest)) if !out.is_empty() => {
                out[0] = b;
                self.0 = rest;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

#[test]
fn a_byte_order_mark_is_skipped_from_any_input() {
    let text = "\u{FEFF}<http://e/s> <http://e/p> <http://e/o> .";
    assert_eq!(parse(RdfFormat::Turtle, text).unwrap().len(), 1);
    let read: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(Trickle(text.as_bytes()))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(read.len(), 1);
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let depth = 100_000;
    let text = format!(
        "<http://e/s> <http://e/p> {}<http://e/o>{} .",
        "( ".repeat(depth),
        " )".repeat(depth)
    );
    let error = parse(RdfFormat::Turtle, &text).unwrap_err().to_string();
    assert!(error.contains("nesting deeper than 128"), "{error}");
    let brackets = format!(
        "<http://e/s> <http://e/p> {}<http://e/o>{} .",
        "[ <http://e/q> ".repeat(depth),
        " ]".repeat(depth)
    );
    assert!(parse(RdfFormat::Turtle, &brackets).is_err());
    // Within the limit, and with a higher one.
    let fine = format!(
        "<http://e/s> <http://e/p> {}<http://e/o>{} .",
        "( ".repeat(100),
        " )".repeat(100)
    );
    assert!(parse(RdfFormat::Turtle, &fine).is_ok());
    // A higher limit, on a thread with a stack to match.
    let deeper = format!(
        "<http://e/s> <http://e/p> {}<http://e/o>{} .",
        "( ".repeat(1000),
        " )".repeat(1000)
    );
    let parsed = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            RdfParser::from_format(RdfFormat::Turtle)
                .with_max_nesting(2000)
                .for_slice(deeper.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map(|quads| quads.len())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(parsed.unwrap(), 1 + 2 * 1000);
}

#[test]
fn errors_say_what_and_where() {
    let text = "@prefix ex: <http://e/> .\n\nex:s ex:p\n  other:o .\n";
    let RdfParseError::Syntax(error) = parse(RdfFormat::Turtle, text).unwrap_err() else {
        panic!("a syntax error")
    };
    assert!(error.message().contains("'other:'"), "{error}");
    let at = error.location().start;
    assert_eq!((at.line, at.column), (3, 2), "{error}");
    assert_eq!(at.offset as usize, text.find("other").unwrap());
    // The statements before the error come out first.
    let mut parser = RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(b"<http://e/s> <http://e/p> 1 . <http://e/s> <http://e/p> ? .");
    assert!(parser.next().unwrap().is_ok());
    assert!(parser.next().unwrap().is_err());
    assert!(parser.next().is_none());
}

#[test]
fn relative_iris_need_a_base() {
    let text = "<s> <p> <../o> .";
    assert!(parse(RdfFormat::Turtle, text).is_err());
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri("http://e/a/b")
        .unwrap()
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        quads[0].to_string(),
        "<http://e/a/s> <http://e/a/p> <http://e/o>"
    );
    // @base changes it midway, relative to the one before.
    let quads = parse(
        RdfFormat::Turtle,
        "@base <http://e/x/> . <s> <p> <o> . @base <../y/> . <s> <p> <o> .",
    )
    .unwrap();
    assert_eq!(quads[1].subject.to_string(), "<http://e/y/s>");
    // Unchecked keeps it as written.
    let unchecked = RdfParser::from_format(RdfFormat::Turtle)
        .unchecked()
        .for_slice(text.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(unchecked.object.to_string(), "<../o>");
}

#[test]
fn renamed_blank_nodes_stay_consistent() {
    let text = "_:a <http://e/p> _:b . _:b <http://e/p> _:a . [] <http://e/p> _:a .";
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .rename_blank_nodes()
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    // _:a is the subject of the first, the object of the second and third.
    assert_eq!(
        nrese_rdf::Term::from(quads[0].subject.clone()),
        quads[1].object
    );
    assert_eq!(quads[1].object, quads[2].object);
    assert!(!quads[0].subject.to_string().contains("_:a"));
}

#[test]
fn trig_graphs_and_the_default_graph() {
    let text = r#"
        @prefix ex: <http://e/> .
        ex:s ex:p ex:o .
        ex:g { ex:s ex:p ex:o1 . ex:s ex:p ex:o2 }
        GRAPH _:b { ex:s ex:p "x" . }
        { ex:s ex:p ex:o3 }
        [] { ex:s ex:p ex:o4 }
    "#;
    let quads = parse(RdfFormat::TriG, text).unwrap();
    let graphs: Vec<String> = quads.iter().map(|q| q.graph_name.to_string()).collect();
    assert_eq!(graphs[0], "DEFAULT");
    assert_eq!(graphs[1], "<http://e/g>");
    assert_eq!(graphs[2], "<http://e/g>");
    assert!(graphs[3].starts_with("_:"));
    assert_eq!(graphs[4], "DEFAULT");
    assert!(graphs[5].starts_with("_:") && graphs[5] != graphs[3]);
    // Graphs are TriG, not Turtle.
    assert!(
        parse(
            RdfFormat::Turtle,
            "<http://e/g> { <http://e/s> <http://e/p> <http://e/o> }"
        )
        .is_err()
    );
}

#[test]
fn the_writer_groups_and_abbreviates() {
    let text = r#"@prefix ex: <http://e/> .
        ex:s a ex:C ; ex:p 1, 2.5, 1e3, true, "x"@en, "y"^^ex:dt, "01"^^<http://www.w3.org/2001/XMLSchema#integer> ; ex:q ex:o .
        ex:t ex:p "z" .
    "#;
    let quads = parse(RdfFormat::Turtle, text).unwrap();
    let mut writer = RdfSerializer::from_format(RdfFormat::Turtle)
        .with_prefix("ex", "http://e/")
        .unwrap()
        .for_writer(Vec::new());
    for quad in &quads {
        writer.serialize_quad(quad).unwrap();
    }
    let written = String::from_utf8(writer.finish().unwrap()).unwrap();
    let expected = "@prefix ex: <http://e/> .\n\nex:s a ex:C ;\n\tex:p 1 ,\n\t\t2.5 ,\n\t\t1e3 ,\n\t\ttrue ,\n\t\t\"x\"@en ,\n\t\t\"y\"^^ex:dt ,\n\t\t01 ;\n\tex:q ex:o .\nex:t ex:p \"z\" .\n";
    assert_eq!(written, expected);
    // And it reads back the same.
    let back: BTreeSet<Quad> = parse(RdfFormat::Turtle, &written)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(back, quads.iter().cloned().collect());
    // Named graphs: TriG yes, Turtle no.
    let trig = parse(
        RdfFormat::TriG,
        "<http://e/g> { <http://e/s> <http://e/p> <http://e/o> }",
    )
    .unwrap();
    let mut turtle = RdfSerializer::from_format(RdfFormat::Turtle).for_writer(Vec::new());
    assert!(turtle.serialize_quad(&trig[0]).is_err());
    let mut writer = RdfSerializer::from_format(RdfFormat::TriG).for_writer(Vec::new());
    writer.serialize_quad(&trig[0]).unwrap();
    let written = String::from_utf8(writer.finish().unwrap()).unwrap();
    assert_eq!(
        written,
        "<http://e/g> {\n\t<http://e/s> <http://e/p> <http://e/o> .\n}\n"
    );
    assert!(
        RdfSerializer::from_format(RdfFormat::Turtle)
            .with_prefix("bad name", "http://e/")
            .is_err()
    );
    assert!(
        RdfSerializer::from_format(RdfFormat::Turtle)
            .with_prefix("ex", "relative")
            .is_err()
    );
}

#[test]
fn large_documents_through_a_reader_equal_the_slice() {
    // Long strings and many statements across many buffer refills.
    let mut text = String::from("@prefix ex: <http://e/> .\n");
    for i in 0..5000 {
        text.push_str(&format!(
            "ex:s{i} ex:p \"\"\"{}\"\"\" ; ex:q [ ex:r ( {i} {} ) ] .\n",
            "long text ".repeat(i % 50),
            i + 1
        ));
    }
    let from_slice: Dataset = parse(RdfFormat::Turtle, &text)
        .unwrap()
        .into_iter()
        .collect();
    let from_reader: Dataset = RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let (mut a, mut b) = (from_slice, from_reader);
    a.canonicalize();
    b.canonicalize();
    assert_eq!(a, b);
    assert_eq!(a.len(), 5000 * 7);
}

/// Turtle and TriG cut for parallel parsing give what one parser gives, whatever the
/// number of parts: directives in the middle (a prefix redefined), strings that look like
/// Turtle, comments, lists, nested blank nodes, graph blocks.
#[test]
fn parallel_parsing_is_exact() {
    let mut turtle = String::from("@prefix ex: <http://a/> .\n");
    let mut trig = String::from("PREFIX ex: <http://a/>\n");
    for i in 0..400 {
        if i == 200 {
            // From here on, ex: is another namespace.
            turtle.push_str("@prefix ex: <http://b/> .\n# ex:fake ex:p ex:o .\n");
            trig.push_str("PREFIX ex:<http://b/>\n");
        }
        turtle.push_str(&format!(
            "ex:s{i} ex:p \"a . b\" , '''x .\nex:not ex:a ex:triple .''' ; ex:q ( 1 2.5 [ ex:r ex:t{i} ] ) .\n"
        ));
        trig.push_str(&format!(
            "ex:g{i} {{ ex:s{i} ex:p \"}} . {{\" . _:b{i} ex:q ex:o{i} . }}\n"
        ));
    }
    for (format, text) in [(RdfFormat::Turtle, &turtle), (RdfFormat::TriG, &trig)] {
        let sequential: BTreeSet<Quad> = parse(format, text).unwrap().into_iter().collect();
        assert!(
            sequential
                .iter()
                .any(|q| q.subject.to_string() == "<http://b/s399>")
        );
        let canonical = |quads: BTreeSet<Quad>| {
            let mut dataset: Dataset = quads.into_iter().collect();
            dataset.canonicalize();
            dataset
        };
        let expected = canonical(sequential);
        for parts in 2..30 {
            let parsers = RdfParser::from_format(format)
                .split_slice_for_parallel_parsing(text.as_bytes(), parts)
                .unwrap();
            assert!(parsers.len() > 1, "{format} wasn't cut into {parts}");
            let quads: BTreeSet<Quad> = parsers.into_iter().flatten().map(|q| q.unwrap()).collect();
            assert_eq!(canonical(quads), expected, "{format} in {parts} parts");
        }
        // The same through a file.
        let path = std::env::temp_dir().join(format!("nrese-rdf-io-split-{}.{}", std::process::id(), format.file_extension()));
        std::fs::write(&path, text).unwrap();
        for parts in [2, 5, 17] {
            let parsers = RdfParser::from_format(format).split_file_for_parallel_parsing(&path, parts).unwrap();
            let quads: BTreeSet<Quad> = parsers.into_iter().flatten().map(|q| q.unwrap()).collect();
            assert_eq!(canonical(quads), expected, "{format} file in {parts} parts");
        }
        std::fs::remove_file(&path).unwrap();
    }
}
