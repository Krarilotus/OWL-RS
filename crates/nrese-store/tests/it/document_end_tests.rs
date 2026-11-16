//! A bulk load reads each file to its end, or stops (docs/design/quarantine.md §1.1). For every
//! format the loader reads, with the document whole, a bad statement mid-file, the file cut
//! inside a statement, the last line end missing and a read error, in both modes:
//! - a load that returns Ok has read every byte, and every statement is committed or
//!   quarantined, with its text where the parser keeps it (not RDF/XML or JSON-LD);
//! - a file that ends inside a statement stops the load in every format; the error names the
//!   file, the line the statement starts on (where the format has lines) and that nothing was
//!   loaded; a last statement without a line end still loads;
//! - a quarantined statement and an error show no position the parser doesn't know;
//! - a format that goes on after a bad statement does so by default; `strict` stops;
//! - a load that stops commits nothing and quarantines nothing.

use std::fs;
use std::path::Path;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_rdf_io::RdfFormat::{self, *};
use nrese_rdf_io::RdfSerializer;
use nrese_store::{
    BulkLoadReport, BulkLoadRequest, GraphTarget, InvalidStatements, QuarantineFilter,
    QuarantinedStatement, StoreConfig, StoreError, StoreMode, StoreService,
};
use tempfile::tempdir;

const EX: &str = "http://example.com/";
/// Statements in a document, one per subject: each is its own line, statement, node element
/// or top-level node object, so a quarantined item stands for one statement.
const N: usize = 40;

const FORMATS: [RdfFormat; 8] = [
    NTriples, NQuads, Turtle, TriG, RdfXml, JsonLd, N3, BinaryRdf,
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Case {
    Whole,
    /// The whole document without its last line end (the text formats).
    NoLastLineEnd,
    BadStatement,
    /// Cut inside the statement that starts on this line.
    Cut(usize),
    /// A directory where the file should be: a read error at the first read.
    Unreadable,
}

/// Whether the parser knows the line of an error in `case`: the text formats always, JSON-LD
/// where the JSON itself is malformed (a cut), RDF/XML and Binary RDF never.
fn knows_lines(format: RdfFormat, case: Case) -> bool {
    match format {
        RdfXml | BinaryRdf => false,
        JsonLd => matches!(case, Case::Cut(_)),
        _ => true,
    }
}

/// `N` statements `ex:s{k} ex:p "v{k}"`, as the format's writer writes them.
fn document(format: RdfFormat) -> Vec<u8> {
    let mut writer = RdfSerializer::from_format(format).for_writer(Vec::new());
    for k in 0..N {
        let (s, p) = (format!("{EX}s{k}"), format!("{EX}p"));
        let (s, p) = (NamedNode::new_unchecked(s), NamedNode::new_unchecked(p));
        let o = Literal::new_simple_literal(format!("v{k}"));
        let quad = Quad::new(s, p, o, GraphName::DefaultGraph);
        writer.serialize_quad(&quad).unwrap();
    }
    writer.finish().unwrap()
}

fn find_once(bytes: &[u8], what: &[u8]) -> usize {
    let at: Vec<usize> = (0..bytes.len())
        .filter(|&i| bytes[i..].starts_with(what))
        .collect();
    assert_eq!(at.len(), 1, "{:?} once", String::from_utf8_lossy(what));
    at[0]
}

fn replace_once(bytes: &[u8], what: &[u8], with: &[u8]) -> Vec<u8> {
    let at = find_once(bytes, what);
    [&bytes[..at], with, &bytes[at + what.len()..]].concat()
}

/// The document with statement 20 invalid and the rest well-formed: a second object where
/// none may be (the text formats), an IRI with a space (RDF/XML), a number as `@id`
/// (JSON-LD), a value of an unknown type (Binary RDF).
fn with_a_bad_statement(format: RdfFormat, document: &[u8]) -> Vec<u8> {
    let subject = format!("\"{EX}s20\"");
    match format {
        RdfXml => {
            let spaced = format!("\"{EX}s 20\"");
            replace_once(document, subject.as_bytes(), spaced.as_bytes())
        }
        JsonLd => replace_once(document, subject.as_bytes(), b"20"),
        BinaryRdf => {
            // A plain literal: its type (3), its length (3), its bytes.
            let at = find_once(document, b"\x03v20") - 1;
            assert_eq!(document[at], 3, "a plain literal's type");
            [&document[..at], &[9_u8][..], &document[at + 1..]].concat()
        }
        _ => replace_once(document, br#""v20""#, br#""v20" "extra""#),
    }
}

/// The document cut inside statement 24 (60 % in), and the line (from 1) it starts on.
fn cut(document: &[u8]) -> (Vec<u8>, usize) {
    let at = find_once(document, b"v24") + 2;
    let line = document[..at].iter().filter(|&&b| b == b'\n').count() + 1;
    (document[..at].to_vec(), line)
}

struct Seen {
    result: Result<BulkLoadReport, StoreError>,
    committed: u64,
    quarantined: Vec<QuarantinedStatement>,
}

fn load(path: &Path, mode: InvalidStatements) -> Seen {
    let dir = tempdir().unwrap();
    let store = StoreService::new(StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: dir.path().join("store"),
        ..StoreConfig::in_memory()
    })
    .unwrap();
    let request = BulkLoadRequest {
        files: vec![path.to_path_buf()],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        invalid: Some(mode),
    };
    let result = store.bulk_load(&request);
    let listed = store.quarantine().list(&QuarantineFilter::default());
    Seen {
        result,
        committed: store.stats().unwrap().quad_count as u64,
        quarantined: listed.unwrap().into_iter().map(|i| i.statement).collect(),
    }
}

/// The rules one load breaks; `row` is its format, case and whether it is strict. A table, a
/// rule a line.
#[rustfmt::skip]
fn broken(row: (RdfFormat, Case, bool), file: &str, seen: &Seen) -> Vec<&'static str> {
    let (format, case, strict) = row;
    let q = seen.quarantined.len() as u64;
    let rules = match &seen.result {
        Err(error) => {
            let message = error.to_string();
            let says = |text: &str| message.contains(text);
            let cut = matches!(case, Case::Cut(_));
            let goes_on = !matches!(format, N3 | BinaryRdf);
            let line_named = match case {
                Case::Cut(line) => {
                    !knows_lines(format, Case::Whole) || says(&format!("line {line}"))
                }
                _ => true,
            };
            vec![
                (seen.committed == 0 && q == 0, "a stop commits and quarantines nothing"),
                (!matches!(case, Case::Whole | Case::NoLastLineEnd), "a well-formed file loads"),
                (strict || case != Case::BadStatement || !goes_on, "a format that goes on does"),
                (!cut || says(file), "a cut names the file"),
                (!cut || says("ends inside a statement"), "a cut says the file ends inside one"),
                (!cut || says("nothing was loaded"), "a cut says nothing was loaded"),
                (line_named, "a cut names the line its statement starts on"),
                (knows_lines(format, case) || !says("line "), "no line the parser doesn't know"),
            ]
        }
        Ok(_) => {
            let without_text = matches!(format, RdfXml | JsonLd);
            let texts = seen.quarantined.iter().all(|s| s.text.is_some());
            let unplaced = seen.quarantined.iter().all(|s| (s.line, s.column) == (0, 0));
            vec![
                (!matches!(case, Case::Cut(_) | Case::Unreadable), "Ok: every byte read, none cut"),
                (seen.committed + q == N as u64, "each statement committed or quarantined"),
                (without_text || texts, "quarantined with its text where the parser keeps it"),
                (!without_text || unplaced, "quarantined at no position the parser doesn't know"),
                (!strict || q == 0, "strict quarantines nothing"),
            ]
        }
    };
    rules
        .into_iter()
        .filter(|(holds, _)| !holds)
        .map(|(_, rule)| rule)
        .collect()
}

#[test]
fn a_load_reads_each_file_to_its_end_or_stops() {
    let dir = tempdir().unwrap();
    let mut failures = Vec::new();
    for format in FORMATS {
        let document = document(format);
        let (cut_document, line) = cut(&document);
        let mut cases = vec![
            (Case::Whole, document.clone()),
            (Case::BadStatement, with_a_bad_statement(format, &document)),
            (Case::Cut(line), cut_document),
            (Case::Unreadable, Vec::new()),
        ];
        if knows_lines(format, Case::Whole) {
            cases.push((Case::NoLastLineEnd, document.trim_ascii_end().to_vec()));
        }
        for (i, (case, bytes)) in cases.into_iter().enumerate() {
            let name = format!("case{i}.{}", format.file_extension());
            let path = dir.path().join(&name);
            match case {
                Case::Unreadable => fs::create_dir(&path).unwrap(),
                _ => fs::write(&path, &bytes).unwrap(),
            }
            for mode in [InvalidStatements::Quarantine, InvalidStatements::Strict] {
                let seen = load(&path, mode);
                let wrong = broken(
                    (format, case, mode == InvalidStatements::Strict),
                    &name,
                    &seen,
                );
                let result = seen.result.as_ref().map(|report| report.inserted);
                let listed: Vec<_> = seen
                    .quarantined
                    .iter()
                    .map(|s| (&s.text, s.line, s.column))
                    .collect();
                println!(
                    "{format:?} {case:?} {mode:?}: {result:?}, committed {}, quarantined {listed:?}, broken {wrong:?}",
                    seen.committed
                );
                if !wrong.is_empty() {
                    failures.push(format!("{format:?} {case:?} {mode:?}: {wrong:?}"));
                }
            }
        }
    }
    let (count, failures) = (failures.len(), failures.join("\n"));
    assert!(count == 0, "{count} loads break the rule:\n{failures}");
}
