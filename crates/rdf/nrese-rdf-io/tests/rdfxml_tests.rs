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
