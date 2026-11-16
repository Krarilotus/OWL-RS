//! A bulk load reads each file to its end, or stops (docs/design/quarantine.md §1.1). For every
//! format the loader reads, with the document whole, a bad statement mid-file, the file cut
//! inside a statement, the last line end missing and a read error, in both modes:
//! - a load that returns Ok has read every byte, and every statement is committed or
//!   quarantined, with its text where the parser keeps it (not RDF/XML or JSON-LD);
//! - a file that ends inside a statement (cut off, or in Turtle and TriG a bracket that never
//!   closes) stops the load in every format, and the error names the file and that nothing
//!   was loaded; in N-Triples and N-Quads, and in Turtle and TriG by default, it also says the
//!   file ends inside a statement, with the line it starts on (strict Turtle and TriG don't
//!   recover, so don't look for its end; the other formats' errors say what is open: an
//!   element, a record, a string); a last statement without a line end still loads;
//! - a Turtle or TriG string still open at the end of the input stops the load as a cut
//!   does, whatever follows its quote; one whose closing quote isn't within 1 MiB stops,
//!   naming its file and line; a short string broken by a line end and closed within it is one
//!   record, and the load goes on;
//! - a quarantined statement and an error show no position the parser doesn't know;
//! - a format that goes on after a bad statement does so by default; `strict` stops;
//! - a load that stops commits nothing and quarantines nothing.

use std::fs;
use std::path::Path;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_rdf_io::RdfFormat::{self, *};
use nrese_rdf_io::RdfSerializer;
use nrese_store::{
    BulkLoadReport, BulkLoadRequest, GraphTarget, InvalidStatements, LoadProgress, QlRewritingMode,
    QuarantineFilter, QuarantinedStatement, StoreConfig, StoreError, StoreMode, StoreService,
};
use tempfile::tempdir;

const EX: &str = "http://example.com/";
/// Statements in a document, one per subject: each is its own line, statement, node element
/// or top-level node object, so a quarantined item stands for one statement.
const N: usize = 40;
/// Statements in `Case::StrayFar`'s document: more than 1 MiB after its stray quote.
const FAR: usize = 20_000;

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
    /// Statement 20 bad (quarantined by default), then cut inside the statement that starts
    /// on this line: the stop leaves nothing of the record either.
    BadThenCut(usize),
    /// Turtle and TriG: a bracket that never closes in the statement on this line, near the
    /// top, the rest well-formed. Recovery finds no end of it: the document ends inside it.
    Unclosed(usize),
    /// Turtle and TriG, statements with IRI objects: statement 1 (line 2) a stray quote, no
    /// quote after it. The string is open at the end of the input: the load stops.
    Stray,
    /// The same, with statement 1's string broken by a line end and closed on the next line:
    /// one record, where it starts.
    LineBreak,
    /// The same, with more than the window after the quote: the load stops.
    StrayFar,
    /// Turtle and TriG: statement 20, on this line, bad and longer than the parser's buffer
    /// (64 KiB), so it is read across a refill. Its record is where it starts.
    LongBad(usize),
    /// A directory where the file should be: a read error at the first read.
    Unreadable,
}

/// Whether the parser knows the line of an error in `case`: the text formats always, JSON-LD
/// where the JSON itself is malformed (a cut), RDF/XML and Binary RDF never.
fn knows_lines(format: RdfFormat, case: Case) -> bool {
    match format {
        RdfXml | BinaryRdf => false,
        JsonLd => matches!(case, Case::Cut(_) | Case::BadThenCut(_)),
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
    /// The store's quarantine directory exists after the load (a fresh store has none).
    quarantine_dir: bool,
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
        quarantine_dir: dir.path().join("store").join("quarantine").exists(),
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
            // Strict stops at the bad statement before the cut.
            let cut = matches!(case, Case::Cut(_) | Case::Unclosed(_) | Case::Stray) || matches!(case, Case::BadThenCut(_)) && !strict;
            let goes_on = !matches!(format, N3 | BinaryRdf);
            // Strict Turtle and TriG don't recover, so they don't look for the end.
            let worded = cut && (matches!(format, NTriples | NQuads) || !strict && matches!(format, Turtle | TriG));
            let line_named = match case {
                Case::Cut(line) | Case::Unclosed(line) | Case::BadThenCut(line) => !worded || says(&format!("line {line}")),
                Case::Stray => !worded || says("line 2"),
                _ => true,
            };
            vec![
                (seen.committed == 0 && q == 0, "a stop commits and quarantines nothing"),
                (!seen.quarantine_dir, "a stop leaves no quarantine directory"),
                (!matches!(case, Case::Whole | Case::NoLastLineEnd), "a well-formed file loads"),
                (strict || !matches!(case, Case::BadStatement | Case::LineBreak | Case::LongBad(_)) || !goes_on, "a format that goes on does"),
                (case != Case::StrayFar || says(file) && says("line 2, ") && !says("ends inside"), "a string not closed within the window names its file and line"),
                (!cut || says(file), "a cut names the file"),
                (!worded || says("ends inside a statement"), "a cut says the file ends inside one"),
                (!cut || says("nothing was loaded"), "a cut says nothing was loaded"),
                (line_named, "a cut names the line its statement starts on"),
                (knows_lines(format, case) || !says("line "), "no line the parser doesn't know"),
                (!strict || !says("ends inside a statement") || matches!(format, NTriples | NQuads), "strict reads no further than its error"),
            ]
        }
        Ok(_) => {
            let without_text = matches!(format, RdfXml | JsonLd);
            let texts = seen.quarantined.iter().all(|s| s.text.is_some());
            let unplaced = seen.quarantined.iter().all(|s| (s.line, s.column) == (0, 0));
            // Its reason says what the parser knows: RDF/XML the byte, JSON-LD nothing more.
            let said = |s: &QuarantinedStatement| s.reason.starts_with("byte ") == (format == RdfXml);
            let said = seen.quarantined.iter().all(|s| !s.reason.contains("line ") && said(s));
            let starts = |line| seen.quarantined.iter().all(|s| (s.line, s.column) == (line as u64, 1));
            vec![
                (!matches!(case, Case::Cut(_) | Case::BadThenCut(_) | Case::Unclosed(_) | Case::Stray | Case::Unreadable), "Ok: every byte read, none cut"),
                (case != Case::StrayFar, "a string not closed within the window stops"),
                (seen.committed + q == (if case == Case::StrayFar { FAR } else { N }) as u64, "each statement committed or quarantined"),
                (without_text || texts, "quarantined with its text where the parser keeps it"),
                (!without_text || unplaced, "quarantined at no position the parser doesn't know"),
                (!without_text || said, "a record's reason says what is known, and no line"),
                (!strict || q == 0, "strict quarantines nothing"),
                (match case { Case::LongBad(line) => starts(line), Case::LineBreak => starts(2), _ => true }, "a record is where its statement starts"),
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
            (
                Case::BadThenCut(line),
                cut(&with_a_bad_statement(format, &document)).0,
            ),
            (Case::Unreadable, Vec::new()),
        ];
        if knows_lines(format, Case::Whole) {
            cases.push((Case::NoLastLineEnd, document.trim_ascii_end().to_vec()));
        }
        if matches!(format, Turtle | TriG) {
            // Statement 1's object opens a `[` that never closes; 38 statements follow it.
            let at = find_once(&document, br#""v1""#);
            let line = document[..at].iter().filter(|&&b| b == b'\n').count() + 1;
            let unclosed = replace_once(&document, br#""v1""#, br#"[ <http://example.com/q> "v1""#);
            cases.push((Case::Unclosed(line), unclosed));
            let at = find_once(&document, br#""v20""#);
            let line = document[..at].iter().filter(|&&b| b == b'\n').count() + 1;
            let long = format!("\"v20{}\" \"extra\"", "x".repeat(70_000));
            let long = replace_once(&document, br#""v20""#, long.as_bytes());
            cases.push((Case::LongBad(line), long));
            for (case, n) in [
                (Case::Stray, N),
                (Case::LineBreak, N),
                (Case::StrayFar, FAR),
            ] {
                let statement = |k| match k {
                    1 if case == Case::LineBreak => {
                        format!("<{EX}s1> <{EX}p> \"broken\nstring\" .\n")
                    }
                    1 => format!("<{EX}s1> <{EX}p> \"stray .\n"),
                    _ => format!("<{EX}s{k}> <{EX}p> <{EX}o{k}> .\n"),
                };
                cases.push((case, (0..n).map(statement).collect::<String>().into_bytes()));
            }
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

/// What a load of one row's file must do.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// Stop: an error that says nothing was loaded; nothing committed or quarantined, and no
    /// quarantine directory left.
    Stops,
    /// Load this many statements, none quarantined.
    Loads(u64),
    /// Load this many and quarantine one statement, its record at this line, column 1.
    Records(u64, u64),
}

/// A file cut anywhere in its structure stops the load, in every format: inside an IRI, a
/// string of any form (whatever the string holds after its quote: an open string at the end
/// of the input is never taken for a typo), a collection, a directive or declaration, between
/// a statement's terms, inside an XML tag or between elements, inside a JSON string or
/// between members, inside each field of a Binary RDF record. A file that ends where a
/// document may end loads: empty, comments, whitespace, directives only, a byte order mark,
/// CRLF line ends, a closed TriG block. A bad statement whose first token can't be read is
/// recorded where that token is.
#[test]
fn a_file_cut_anywhere_stops_and_one_that_ends_whole_loads() {
    use Expect::*;
    let s = "<urn:s> <urn:p> <urn:o> .\n";
    let t = "<urn:t> <urn:p> <urn:o> .";
    let x = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="urn:e#"><rdf:Description rdf:about="urn:s"><e:p>1</e:p></rdf:Description>"#;
    let binary = document(BinaryRdf);
    let field = find_once(&binary, b"\x03v24") - 1;
    let text = |t: &str| t.as_bytes().to_vec();
    let mut rows: Vec<(RdfFormat, Vec<u8>, Expect)> = Vec::new();
    for format in [NTriples, NQuads, Turtle, TriG, N3] {
        let cuts = [
            "<urn:s> <urn:p> <http://ex/o",
            "<urn:s> ",
            "<urn:s> <urn:p> ",
            "<urn:lost> <urn:p> \"cut .",
        ];
        for tail in cuts {
            rows.push((format, text(&format!("{s}{tail}")), Stops));
        }
        rows.push((format, Vec::new(), Loads(0)));
        rows.push((
            format,
            text(&format!("{s}  \n# a comment, no line end")),
            Loads(1),
        ));
        rows.push((
            format,
            text(&format!("{s}{t}\n").replace('\n', "\r\n")),
            Loads(2),
        ));
        rows.push((
            format,
            text(&format!("{s}{t}").replace('\n', "\r\n")),
            Loads(2),
        ));
    }
    for format in [NTriples, NQuads] {
        rows.push((format, text(&format!("{s}VERSION \"1.2")), Stops));
        rows.push((format, text(&format!("{s}VERSION \"1.2\"")), Loads(1)));
    }
    for format in [Turtle, TriG] {
        let mut stops = vec![
            "<urn:lost> <urn:p> 'cut .",
            "<urn:lost> <urn:p> \"\"\"cut .",
            "<urn:lost> <urn:p> '''cut .",
            "<urn:lost> <urn:p> \"cut . <urn:x> <urn:p> <urn:o> .",
            "<urn:s> <urn:p> \"\"\"a long\nstring",
            "<urn:s> <urn:p> ( <urn:a> <urn:b>",
            "@prefix ex: <http://ex",
            "@prefix ex: <http://ex/>",
            "PREFIX ex: <http://ex",
            "@base <http://ex",
            "BASE <http://ex",
            "@version \"1.2",
            "VERSION \"1.2",
        ];
        if format == TriG {
            stops.extend([
                "GRAPH <urn:g> {",
                "<urn:g> { <urn:s> <urn:p> <urn:o> .",
                "<urn:g> { <urn:lost> <urn:p> \"cut .",
            ]);
        }
        for tail in stops {
            rows.push((format, text(&format!("{s}{tail}")), Stops));
        }
        for whole in [
            "PREFIX ex: <http://ex/>",
            "VERSION \"1.2\"",
            "@prefix ex: <http://ex/> .\n",
        ] {
            rows.push((format, text(&format!("{s}{whole}")), Loads(1)));
        }
        let directives = "@prefix ex: <http://ex/> .\n@base <http://ex/> .\n";
        rows.push((format, text(directives), Loads(0)));
        rows.push((format, text(&format!("\u{FEFF}{s}")), Loads(1)));
        let unlexed = format!("{s}# a comment\n\n% <urn:p> <urn:o> .\n{t}\n");
        rows.push((format, text(&unlexed), Records(2, 4)));
    }
    rows.push((
        TriG,
        text("<urn:g> { <urn:s> <urn:p> <urn:o> . }"),
        Loads(1),
    ));
    rows.push((
        TriG,
        text("<urn:g> { <urn:s> <urn:p> <urn:o> }\n"),
        Loads(1),
    ));
    for tail in [
        "@prefix ex: <http://ex",
        "@keywords a",
        "@forAll <http://ex/x",
    ] {
        rows.push((N3, text(&format!("{s}{tail}")), Stops));
    }
    rows.push((
        N3,
        text(&format!("{s}@prefix ex: <http://ex/> .")),
        Loads(1),
    ));
    let xml_cuts = [
        "<rdf:Descr",
        r#"<rdf:Description rdf:about="urn:t"><e:p>2</e"#,
        "",
        r#"<rdf:Description rdf:about="urn:t"><e:p>tex"#,
    ];
    for tail in xml_cuts {
        rows.push((RdfXml, text(&format!("{x}{tail}")), Stops));
    }
    rows.push((RdfXml, text(&format!("{x}</rdf:RDF>\n \n")), Loads(1)));
    let json_cuts = [
        r#"[{"@id": "urn:s", "urn:p": "v"#,
        r#"[{"@id": "urn:s", "urn:p": "v"},"#,
        r#"[{"@id": "urn:s","#,
        r#"[{"@id": "urn:s", "urn:p""#,
    ];
    for cut in json_cuts {
        rows.push((JsonLd, text(cut), Stops));
    }
    let json = "[{\"@id\": \"urn:s\", \"urn:p\": \"v\"}]\n \n";
    rows.push((JsonLd, text(json), Loads(1)));
    // A record's type, its length, its bytes.
    for at in [field + 1, field + 2, field + 3] {
        rows.push((BinaryRdf, binary[..at].to_vec(), Stops));
    }
    let dir = tempdir().unwrap();
    let mut failures = Vec::new();
    for (i, (format, bytes, expect)) in rows.iter().enumerate() {
        let path = dir
            .path()
            .join(format!("row{i}.{}", format.file_extension()));
        fs::write(&path, bytes).unwrap();
        let seen = load(&path, InvalidStatements::Quarantine);
        let q = seen.quarantined.len() as u64;
        let at = |line| {
            seen.quarantined
                .iter()
                .all(|r| (r.line, r.column) == (line, 1))
        };
        let holds = match (&seen.result, *expect) {
            (Err(error), Stops) => {
                error.to_string().ends_with("nothing was loaded")
                    && seen.committed == 0
                    && q == 0
                    && !seen.quarantine_dir
            }
            (Ok(_), Loads(n)) => seen.committed == n && q == 0,
            (Ok(_), Records(n, line)) => seen.committed == n && q == 1 && at(line),
            _ => false,
        };
        if !holds {
            let end = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(60)..]);
            let result = seen.result.as_ref().map(|r| r.inserted);
            let records: Vec<_> = seen
                .quarantined
                .iter()
                .map(|r| (r.line, r.column))
                .collect();
            failures.push(format!(
                "{format:?} row {i} …{end:?}: expected {expect:?}, got {:?}, committed {}, records {records:?}, quarantine dir {}",
                result.map_err(|e| e.to_string()),
                seen.committed,
                seen.quarantine_dir,
            ));
        }
    }
    let (count, failures) = (failures.len(), failures.join("\n"));
    assert!(count == 0, "{count} row(s) failed:\n{failures}");
}

/// A load that stops changes nothing a query sees: the answers that rest on the reasoning
/// certificate stay, and so do the certificate, the revision and the marker on disk, in the
/// process and after a reopen. Stops: a cut file, by default and replacing; a bad statement
/// under `strict`; a read error; a cancel; a whole file, then a cut one; a quarantined
/// statement, then a cut. A load that adds nothing keeps them too; one that commits clears
/// the certificate, and the answer that rests on it goes.
#[test]
fn a_load_that_stops_keeps_the_answers_and_the_certificate() {
    let dir = tempdir().unwrap();
    let file = |name: &str, text: &str| {
        let path = dir.path().join(name);
        fs::write(&path, text).unwrap();
        path
    };
    let prefixes = format!(
        "@prefix : <{EX}> .\n@prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n"
    );
    // `:a :p ?o` holds only through the rewriting of the existential, which the certificate
    // switches on: no statement in the data has `:a` as its subject and `:p`.
    let ontology = format!(
        "{prefixes}:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
         owl:someValuesFrom owl:Thing ] .\n:a a :A .\n"
    );
    let ontology = file("ontology.ttl", &ontology);
    let whole = file("whole.ttl", &format!("{prefixes}:b a :A .\n"));
    let cut = file("cut.ttl", &format!("{prefixes}:c a :A .\n:d a :"));
    let bad = file(
        "bad.ttl",
        &format!("{prefixes}:c a :A .\n:d a :A :B .\n:e a :A .\n"),
    );
    let bad_then_cut = file(
        "bad-then-cut.ttl",
        &format!("{prefixes}:c a :A :B .\n:d a :"),
    );
    let unreadable = dir.path().join("unreadable.ttl");
    fs::create_dir(&unreadable).unwrap();
    let store_dir = dir.path().join("store");
    let config = || StoreConfig {
        ql_rewriting: QlRewritingMode::On,
        ..StoreConfig::on_disk(&store_dir)
    };
    let request = |files: &[&Path], replace, invalid| BulkLoadRequest {
        files: files.iter().map(|f| f.to_path_buf()).collect(),
        replace,
        graph: GraphTarget::DefaultGraph,
        invalid: Some(invalid),
    };
    let ask = |store: &StoreService| {
        let query = format!("ASK {{ <{EX}a> <{EX}p> ?o }}");
        let result = store.execute_query_str(&query).expect("query");
        let json: serde_json::Value = serde_json::from_slice(&result.payload).unwrap();
        json["boolean"].as_bool().expect("a boolean")
    };
    let marker = || fs::read(store_dir.join("reasoning.state")).ok();
    let store = StoreService::new(config()).unwrap();
    let quarantine = InvalidStatements::Quarantine;
    store
        .bulk_load(&request(&[&ontology], false, quarantine))
        .unwrap();
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Ql)
        .unwrap();
    assert!(
        ask(&store),
        "the certificate's rewriting answers the existential"
    );
    let certified = (store.reasoning_state(), store.consistency());
    let (revision, written) = (store.current_revision(), marker());
    assert!(
        certified.0.is_some() && written.is_some(),
        "the store is certified"
    );
    let mut wrong = Vec::new();
    let unchanged = |store: &StoreService, after: &str, wrong: &mut Vec<String>| {
        let seen = [
            (ask(store), "the answer"),
            (
                (store.reasoning_state(), store.consistency()) == certified,
                "the certificate",
            ),
            (store.current_revision() == revision, "the revision"),
            (marker() == written, "the marker"),
        ];
        for (holds, what) in seen {
            if !holds {
                wrong.push(format!("{after}: {what} changed"));
            }
        }
    };
    let strict = InvalidStatements::Strict;
    // Each stop's files, whether it replaces, its mode, and whether it is cancelled.
    let stops: [(&str, &[&Path], bool, InvalidStatements, bool); 7] = [
        ("a cut file", &[&cut], false, quarantine, false),
        ("a cut file, replacing", &[&cut], true, quarantine, false),
        ("a strict bad statement", &[&bad], false, strict, false),
        ("a read error", &[&unreadable], false, quarantine, false),
        ("a cancelled load", &[&whole], false, quarantine, true),
        (
            "a whole file, then a cut one",
            &[&whole, &cut],
            false,
            quarantine,
            false,
        ),
        (
            "a quarantined statement, then a cut",
            &[&bad_then_cut],
            false,
            quarantine,
            false,
        ),
    ];
    for (stop, files, replace, invalid, cancel) in stops {
        let progress = LoadProgress::default();
        if cancel {
            progress.cancel();
        }
        let result = store.bulk_load_with(&request(files, replace, invalid), &progress);
        if result.is_ok() {
            wrong.push(format!("{stop}: the load didn't stop"));
        }
        unchanged(&store, stop, &mut wrong);
    }
    // A statement the store holds: no blank node, so nothing new.
    let known = file("known.ttl", &format!("{prefixes}:a a :A .\n"));
    let again = store.bulk_load(&request(&[&known], false, quarantine));
    if again.as_ref().map(|report| report.inserted).ok() != Some(0) {
        wrong.push(format!(
            "a load of what is there added something: {again:?}"
        ));
    }
    unchanged(&store, "a load that adds nothing", &mut wrong);
    drop(store);
    let store = StoreService::new(config()).unwrap();
    unchanged(&store, "after a reopen", &mut wrong);
    store
        .bulk_load(&request(&[&whole], false, quarantine))
        .unwrap();
    if ask(&store) || store.reasoning_state().is_some() || marker().is_some() {
        wrong.push("a load that commits left the certificate or its answer".to_owned());
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
