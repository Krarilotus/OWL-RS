//! JSON-LD beyond the W3C suites: streaming, safety settings, and the writers.

use std::collections::BTreeSet;

use nrese_rdf::{Dataset, Quad};
use nrese_rdf_io::jsonld::{FromRdfOptions, JsonLdOptions, RemoteDocument};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

fn parse(text: &str) -> Result<Vec<Quad>, String> {
    RdfParser::from_format(RdfFormat::JsonLd)
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}

/// A top-level array and a top-level `@graph` are read one element at a time: the quads
/// of the elements before a bad one come out before its error.
#[test]
fn elements_stream() {
    for text in [
        r#"[{"@id": "http://e/a", "http://e/p": "1"}, {"@id": "http://e/b", "http://e/p": "2"}, {"@id": 5}]"#,
        r#"{"@context": {"p": "http://e/p"}, "@graph": [{"@id": "http://e/a", "p": "1"}, {"@id": "http://e/b", "p": "2"}, {"@id": 5}]}"#,
        r#"{"@graph": [{"@id": "http://e/a", "http://e/p": "1"}, {"@id": "http://e/b", "http://e/p": "2"}, {"@id": 5}], "@context": {}}"#,
    ] {
        let mut parser = RdfParser::from_format(RdfFormat::JsonLd).for_slice(text.as_bytes());
        assert!(parser.next().unwrap().is_ok(), "{text}");
        assert!(parser.next().unwrap().is_ok(), "{text}");
        let error = parser.next().unwrap().unwrap_err().to_string();
        assert!(error.contains("invalid @id value"), "{error}");
        assert!(parser.next().is_none());
    }
}

/// A top-level object with more than a graph is read whole, and means what it says: here
/// the graph is named by the object's `@id`, which comes after it.
#[test]
fn a_named_top_level_graph_is_read_whole() {
    let quads =
        parse(r#"{"@graph": [{"@id": "http://e/a", "http://e/p": "1"}], "@id": "http://e/g"}"#)
            .unwrap();
    assert_eq!(quads.len(), 1);
    assert_eq!(quads[0].graph_name.to_string(), "<http://e/g>");
    // And `@context` after the graph still applies to it.
    let quads =
        parse(r#"{"@graph": [{"@id": "http://e/a", "p": "1"}], "@context": {"p": "http://e/p"}}"#)
            .unwrap();
    assert_eq!(quads[0].predicate.as_str(), "http://e/p");
}

#[test]
fn remote_contexts_need_a_loader() {
    let text =
        r#"{"@context": "http://example.org/context.jsonld", "@id": "http://e/a", "p": "1"}"#;
    let error = parse(text).unwrap_err();
    assert!(error.contains("loading remote context failed"), "{error}");
    let options = JsonLdOptions::new().with_document_loader(|url| {
        assert_eq!(url, "http://example.org/context.jsonld");
        Ok(RemoteDocument {
            document_url: url.to_owned(),
            document: r#"{"@context": {"p": "http://e/p"}}"#.to_owned(),
        })
    });
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::JsonLd)
        .with_json_ld_options(options)
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(quads.len(), 1);
}

/// Nesting up to the parser's limit (128) is read on a default-sized thread stack, debug
/// builds included; deeper is an error, not a crash.
#[test]
fn deep_nesting_is_bounded() {
    let document = |depth: usize| {
        let mut text = String::new();
        for _ in 0..depth {
            text.push_str(r#"{"http://e/p": "#);
        }
        text.push_str("\"leaf\"");
        for _ in 0..depth {
            text.push('}');
        }
        text
    };
    let quads = parse(&document(127)).unwrap();
    assert_eq!(quads.len(), 127);
    assert!(
        parse(&document(1000))
            .unwrap_err()
            .contains("nested deeper")
    );
}

fn round_trip(quads: &[Quad], serializer: RdfSerializer) -> (String, BTreeSet<Quad>) {
    let mut writer = serializer.for_writer(Vec::new());
    for quad in quads {
        writer.serialize_quad(quad).unwrap();
    }
    let text = String::from_utf8(writer.finish().unwrap()).unwrap();
    let back = RdfParser::from_format(RdfFormat::JsonLd)
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    (text, back)
}

fn canonical(quads: impl IntoIterator<Item = Quad>) -> Dataset {
    let mut dataset: Dataset = quads.into_iter().collect();
    dataset.canonicalize();
    dataset
}

#[test]
fn the_streaming_writer() {
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(
            br#"<http://e/s> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://e/C> .
<http://e/s> <http://e/p> "x" .
<http://e/s> <http://e/q> "1"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://e/s> <http://e/p> "y"@en .
<http://e/s> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://e/D> .
<ex:odd> <http://e/p> <ex:other> <http://e/g> .
_:b <http://e/p> _:c <http://e/g> .
<http://e/s> <http://e/p> "back" .
"#,
        )
        .collect::<Result<_, _>>()
        .unwrap();
    let serializer = RdfSerializer::from_format(RdfFormat::JsonLd)
        .with_prefix("ex", "http://e/")
        .unwrap()
        .with_prefix("xsd", "http://www.w3.org/2001/XMLSchema#")
        .unwrap();
    let (text, back) = round_trip(&quads, serializer);
    assert_eq!(canonical(back), canonical(quads), "{text}");
    // The streaming profile's order, each key once.
    assert!(text.contains(r#"{"@id":"ex:s","@type":["ex:C","ex:D"],"ex:p":["x",{"@value":"y","@language":"en"}],"ex:q":[{"@value":"1","@type":"xsd:integer"}]}"#), "{text}");
    // An IRI that looks like a compact IRI turns its prefix off where it is.
    assert!(
        text.contains(
            r#"{"@context":{"ex":null},"@id":"ex:odd","http://e/p":[{"@id":"ex:other"}]}"#
        ),
        "{text}"
    );
}

#[test]
fn the_expanded_writer_finds_lists() {
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(br#"<http://e/s> <http://e/p> ( 1 "two" ( 3 ) ) ."#)
        .collect::<Result<_, _>>()
        .unwrap();
    let options = FromRdfOptions {
        use_native_types: true,
        ..FromRdfOptions::default()
    };
    let serializer = RdfSerializer::from_format(RdfFormat::JsonLd).with_json_ld_expanded(options);
    let (text, back) = round_trip(&quads, serializer);
    assert_eq!(
        text.trim(),
        r#"[{"@id":"http://e/s","http://e/p":[{"@list":[{"@value":1},{"@value":"two"},{"@list":[{"@value":3}]}]}]}]"#
    );
    assert_eq!(canonical(back), canonical(quads));
}

/// A recovering JSON-LD parser skips an element of a top-level array (or `@graph` array)
/// that fails to expand and goes on with the next; malformed JSON still stops it.
#[test]
fn recovering_parsers_skip_bad_elements_and_go_on() {
    let document = r#"[
        {"@id": "http://example.org/a", "http://example.org/p": "1"},
        {"@id": "http://example.org/b", "@type": 5},
        {"@id": "http://example.org/c", "http://example.org/p": "3"}
    ]"#;
    let subjects = |results: &[Result<Quad, nrese_rdf_io::RdfParseError>]| -> Vec<String> {
        results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|q| q.subject.to_string())
            .collect()
    };
    let read = |text: &str, recovering: bool| -> Vec<Result<Quad, nrese_rdf_io::RdfParseError>> {
        let parser = RdfParser::from_format(RdfFormat::JsonLd);
        let parser = if recovering {
            parser.recovering()
        } else {
            parser
        };
        parser.for_slice(text.as_bytes()).collect()
    };
    let results = read(document, true);
    assert_eq!(
        subjects(&results),
        ["<http://example.org/a>", "<http://example.org/c>"],
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|r| r.is_err()).count(),
        1,
        "{results:?}"
    );
    // Inside a top-level `@graph`, the same.
    let graph = format!(r#"{{"@graph": {document}}}"#);
    assert_eq!(subjects(&read(&graph, true)).len(), 2);
    // Without recovery: nothing after the first error.
    let strict = read(document, false);
    assert_eq!(subjects(&strict), ["<http://example.org/a>"], "{strict:?}");
    assert_eq!(strict.iter().filter(|r| r.is_err()).count(), 1);
    // Malformed JSON: no recovery.
    let broken = document.replace(r#""@type": 5}"#, r#""@type": 5"#);
    let results = read(&broken, true);
    assert_eq!(
        results.iter().filter(|r| r.is_err()).count(),
        1,
        "{results:?}"
    );
}
