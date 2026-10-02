//! RDF/XML beyond the W3C suite: real documents, and the writer's output.

use std::collections::BTreeSet;
use std::path::Path;

use nrese_rdf::{Dataset, Quad};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

/// The OWL 2 test cases (one large RDF/XML file with entities, fetched by
/// `scripts/fetch-w3c-tests.sh`): read without error, written back and read again alike.
#[test]
fn the_owl_2_test_cases_read_and_round_trip() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache/owl-test/all.rdf");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!(
            "skipped: {} not found (run scripts/fetch-w3c-tests.sh)",
            path.display()
        );
        return;
    };
    let quads: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2009/11/owl-test/all.rdf")
        .unwrap()
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(quads.len() > 10_000, "{}", quads.len());
    // Through a reader too.
    let from_reader: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2009/11/owl-test/all.rdf")
        .unwrap()
        .for_reader(bytes.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    let canonical = |quads: BTreeSet<Quad>| {
        let mut dataset: Dataset = quads.into_iter().collect();
        dataset.canonicalize();
        dataset
    };
    let expected = canonical(quads.clone());
    assert_eq!(canonical(from_reader), expected);
    // Written as RDF/XML and read back.
    let mut writer = RdfSerializer::from_format(RdfFormat::RdfXml)
        .with_prefix("owl", "http://www.w3.org/2002/07/owl#")
        .unwrap()
        .for_writer(Vec::new());
    for quad in &quads {
        writer.serialize_quad(quad).unwrap();
    }
    let text = writer.finish().unwrap();
    let back: BTreeSet<Quad> = RdfParser::from_format(RdfFormat::RdfXml)
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(canonical(back), expected);
}

#[test]
fn what_rdf_xml_cant_hold_is_an_error() {
    let parse = |text: &str| -> Vec<Quad> {
        RdfParser::from_format(RdfFormat::NQuads)
            .for_slice(text.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let write = |quad: &Quad| {
        let mut writer = RdfSerializer::from_format(RdfFormat::RdfXml).for_writer(Vec::new());
        writer.serialize_quad(quad)
    };
    assert!(write(&parse("<http://e/s> <http://e/p> <http://e/o> <http://e/g> .")[0]).is_err());
    assert!(write(&parse("<http://e/s> <http://e/123> <http://e/o> .")[0]).is_err());
    assert!(write(&parse("<http://e/s> <http://e/p> \"\\u0001\" .")[0]).is_err());
    assert!(write(&parse("<http://e/s> <http://e/p> \"fine\" .")[0]).is_ok());
}

/// `xml:lang` takes any text in XML, but a literal's language must be a BCP 47 tag: found by
/// fuzzing (`nrese-fuzz`), the RDF/XML reader took `"bar"@es/wn` and wrote N-Quads that
/// didn't read back. Now it refuses the document, as the other readers do; unchecked, it
/// still takes it.
#[test]
fn xml_lang_must_be_a_language_tag() {
    let document = |lang: &str| {
        format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:e=\"http://example.org/\">\
             <rdf:Description rdf:about=\"http://example.org/joe\" xml:lang=\"{lang}\"><e:name>bar</e:name></rdf:Description></rdf:RDF>"
        )
    };
    let read = |parser: RdfParser, lang: &str| {
        parser
            .for_slice(document(lang).as_bytes())
            .collect::<Result<Vec<_>, _>>()
    };
    for lang in ["es/wn", "f@r", "e all rights", "-en", "en-"] {
        assert!(
            read(RdfParser::from_format(RdfFormat::RdfXml), lang).is_err(),
            "{lang}"
        );
        assert!(read(RdfParser::from_format(RdfFormat::RdfXml).unchecked(), lang).is_ok());
    }
    for lang in ["en", "EN-gb", "zh-Hant-TW", ""] {
        assert!(
            read(RdfParser::from_format(RdfFormat::RdfXml), lang).is_ok(),
            "{lang}"
        );
    }
}
