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
/// A recovering RDF/XML parser skips the rest of the outermost node element an error is
/// in, wherever in it the error is (deep inside, or at an end tag), and goes on after it;
/// XML that isn't well-formed still stops it.
#[test]
fn recovering_parsers_skip_bad_node_elements_and_go_on() {
    let document = "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
                    xmlns:e=\"http://example.org/\">\
        <rdf:Description rdf:about=\"http://example.org/a\"><e:p>1</e:p></rdf:Description>\
        <rdf:Description rdf:about=\"http://example.org/b\">\
          <e:p xml:lang=\"es/wn\">x</e:p>\
          <e:q><rdf:Description rdf:about=\"http://example.org/nested\"><e:r>2</e:r></rdf:Description></e:q>\
        </rdf:Description>\
        <rdf:Description rdf:about=\"http://example.org/c\"><e:p>3</e:p></rdf:Description>\
        <rdf:Description rdf:about=\"http://example.org/d\">\
          <e:p rdf:resource=\"http://example.org/x\">text</e:p>\
        </rdf:Description>\
        <rdf:Description rdf:about=\"http://example.org/e\"><e:p>5</e:p></rdf:Description>\
        </rdf:RDF>";
    let subjects = |results: &[Result<Quad, nrese_rdf_io::RdfParseError>]| -> Vec<String> {
        results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|q| q.subject.to_string())
            .collect()
    };
    let results: Vec<_> = RdfParser::from_format(RdfFormat::RdfXml)
        .recovering()
        .for_slice(document.as_bytes())
        .collect();
    assert_eq!(
        subjects(&results),
        [
            "<http://example.org/a>",
            "<http://example.org/c>",
            "<http://example.org/e>"
        ],
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|r| r.is_err()).count(),
        2,
        "{results:?}"
    );
    // Without recovery: nothing after the first error.
    let strict: Vec<_> = RdfParser::from_format(RdfFormat::RdfXml)
        .for_slice(document.as_bytes())
        .collect();
    assert_eq!(subjects(&strict), ["<http://example.org/a>"], "{strict:?}");
    assert_eq!(strict.iter().filter(|r| r.is_err()).count(), 1);
    // Not well-formed: the end tag doesn't match, and nothing after it is read.
    let broken = document.replacen("</e:p></rdf:Description>", "</e:q></rdf:Description>", 1);
    let results: Vec<_> = RdfParser::from_format(RdfFormat::RdfXml)
        .recovering()
        .for_slice(broken.as_bytes())
        .collect();
    assert_eq!(
        results.iter().filter(|r| r.is_err()).count(),
        1,
        "{results:?}"
    );
    assert!(subjects(&results).is_empty(), "{results:?}");
}

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
