//! Throughput of nrese-rdf-io against oxttl, oxrdfxml and oxjsonld: parsing and writing
//! every format both read or write, on the same generated documents.
//!
//! The data: `QUADS` statements from a fixed seed (IRIs from a few namespaces, strings with
//! escapes and non-ASCII, language tags, typed literals, blank nodes; a share in named
//! graphs for the dataset formats). nrese writes each document once; both parse the same
//! bytes. Ours is measured twice where it can be: owned quads, as Oxigraph hands them
//! out, and borrowed (`next_ref`, no allocation per term).
//!
//! `cargo run --release --bin io [filter]`

use std::hint::black_box;
use std::io::{Cursor, Read};
use std::time::Instant;

use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use nrese_rdf_io::n3::N3Parser;
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph_comparison::rng;
use rand::Rng;

const QUADS: usize = 200_000;
const ROUNDS: usize = 7;

const PREFIXES: [(&str, &str); 4] = [
    ("ex", "http://example.org/resource/"),
    ("voc", "http://example.org/vocabulary#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
];

/// The generated statements; `graphs`: a share of them in named graphs.
fn dataset(graphs: bool) -> Vec<Quad> {
    let mut r = rng(42);
    let words = [
        "alpha",
        "beta",
        "gamma",
        "delta",
        "Übergröße",
        "naïve",
        "日本語",
        "quote \" here",
        "tab\there",
        "line\nbreak",
    ];
    let predicates: Vec<NamedNode> = (0..24)
        .map(|i| NamedNode::new_unchecked(format!("http://example.org/vocabulary#property{i}")))
        .collect();
    let mut quads = Vec::with_capacity(QUADS);
    let mut subject = 0_usize;
    while quads.len() < QUADS {
        subject += 1;
        let s: NamedOrBlankNode = if r.random_bool(0.1) {
            BlankNode::new_unchecked(format!("b{subject}")).into()
        } else {
            NamedNode::new_unchecked(format!("http://example.org/resource/item{subject}")).into()
        };
        let graph = if graphs && r.random_bool(0.3) {
            GraphName::NamedNode(NamedNode::new_unchecked(format!(
                "http://example.org/graph/{}",
                subject % 50
            )))
        } else {
            GraphName::DefaultGraph
        };
        for _ in 0..r.random_range(3..9) {
            let p = predicates[r.random_range(0..predicates.len())].clone();
            let o: Term = match r.random_range(0..7) {
                0 | 1 => NamedNode::new_unchecked(format!(
                    "http://example.org/resource/item{}",
                    r.random_range(0..QUADS)
                ))
                .into(),
                2 => Literal::new_simple_literal(format!(
                    "{} {}",
                    words[r.random_range(0..words.len())],
                    r.random_range(0..1000)
                ))
                .into(),
                3 => Literal::new_language_tagged_literal_unchecked(
                    words[r.random_range(0..4)],
                    ["en", "de", "fr-be"][r.random_range(0..3)],
                )
                .into(),
                4 => Literal::new_typed_literal(
                    r.random_range(-100_000..100_000_i64).to_string(),
                    nrese_rdf::vocab::xsd::INTEGER,
                )
                .into(),
                5 => Literal::new_typed_literal(
                    format!(
                        "2026-{:02}-{:02}",
                        r.random_range(1..13),
                        r.random_range(1..29)
                    ),
                    nrese_rdf::vocab::xsd::DATE,
                )
                .into(),
                _ => BlankNode::new_unchecked(format!("b{}", r.random_range(0..subject.max(1))))
                    .into(),
            };
            quads.push(Quad {
                subject: s.clone(),
                predicate: p,
                object: o,
                graph_name: graph.clone(),
            });
        }
    }
    quads.truncate(QUADS);
    quads
}

fn write(format: RdfFormat, quads: &[Quad]) -> Vec<u8> {
    let mut serializer = RdfSerializer::from_format(format);
    for (name, iri) in PREFIXES {
        serializer = serializer.with_prefix(name, iri).unwrap();
    }
    let mut writer = serializer.for_writer(Vec::new());
    for quad in quads {
        writer.serialize_quad(quad).unwrap();
    }
    writer.finish().unwrap()
}

/// The median seconds of `work` over `ROUNDS` runs.
fn seconds(mut work: impl FnMut() -> usize) -> f64 {
    let mut times: Vec<f64> = (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            black_box(work());
            start.elapsed().as_secs_f64()
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[ROUNDS / 2]
}

struct Report {
    filter: Option<String>,
}

impl Report {
    /// One case: ours and theirs over `bytes` bytes and `items` statements.
    fn case(
        &self,
        name: &str,
        bytes: usize,
        items: usize,
        ours: impl FnMut() -> usize,
        theirs: impl FnMut() -> usize,
    ) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        let (a, b) = (seconds(ours), seconds(theirs));
        let mb = bytes as f64 / 1e6;
        println!(
            "{name:<40} nrese {:>7.1} MB/s {:>6.2} Mq/s   oxigraph {:>7.1} MB/s {:>6.2} Mq/s   time ratio {:>5.2}",
            mb / a,
            items as f64 / a / 1e6,
            mb / b,
            items as f64 / b / 1e6,
            a / b
        );
    }
}

/// A reader that hands out `chunk` bytes per call, as a socket or file would.
struct Chunks<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl Read for Chunks<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.chunk.min(out.len()).min(self.data.len());
        out[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

/// Counts the items of a parse, failing on the first error.
trait CountOk {
    fn count_ok(self) -> usize;
}

impl<T, E: std::fmt::Debug, I: Iterator<Item = Result<T, E>>> CountOk for I {
    fn count_ok(self) -> usize {
        let mut count = 0;
        for item in self {
            item.unwrap();
            count += 1;
        }
        count
    }
}

fn ours(format: RdfFormat, bytes: &[u8]) -> usize {
    RdfParser::from_format(format).for_slice(bytes).count_ok()
}

fn ours_borrowed(format: RdfFormat, bytes: &[u8]) -> usize {
    let mut parser = RdfParser::from_format(format).for_slice(bytes);
    let mut n = 0;
    while let Some(quad) = parser.next_ref() {
        black_box(quad.unwrap());
        n += 1;
    }
    n
}

fn ours_reader(format: RdfFormat, bytes: &[u8]) -> usize {
    let mut parser = RdfParser::from_format(format).for_reader(Chunks {
        data: bytes,
        chunk: 64 * 1024,
    });
    let mut n = 0;
    while let Some(quad) = parser.next_ref() {
        black_box(quad.unwrap());
        n += 1;
    }
    n
}

fn main() {
    let report = Report {
        filter: std::env::args().nth(1),
    };
    let triples = dataset(false);
    let quads = dataset(true);
    let documents: Vec<(RdfFormat, &[Quad])> = vec![
        (RdfFormat::NTriples, &triples),
        (RdfFormat::NQuads, &quads),
        (RdfFormat::Turtle, &triples),
        (RdfFormat::TriG, &quads),
        (RdfFormat::RdfXml, &triples),
        (RdfFormat::JsonLd, &quads),
        (RdfFormat::N3, &triples),
    ];
    // The same statements as Oxigraph's types, for its writers.
    let to_ox = |quads: &[Quad]| -> Vec<oxrdf::Quad> {
        let text = write(RdfFormat::NQuads, quads);
        oxttl::NQuadsParser::new()
            .for_slice(&text)
            .map(|q| q.unwrap())
            .collect()
    };
    let (ox_triples, ox_quads) = (to_ox(&triples), to_ox(&quads));

    println!("Parsing ({QUADS} statements; time ratio < 1: nrese faster)");
    for (format, data) in &documents {
        let bytes = write(*format, data);
        let len = bytes.len();
        let theirs = |bytes: &[u8]| -> usize {
            match format {
                RdfFormat::NTriples => oxttl::NTriplesParser::new().for_slice(bytes).count_ok(),
                RdfFormat::NQuads => oxttl::NQuadsParser::new().for_slice(bytes).count_ok(),
                RdfFormat::Turtle => oxttl::TurtleParser::new().for_slice(bytes).count_ok(),
                RdfFormat::TriG => oxttl::TriGParser::new().for_slice(bytes).count_ok(),
                RdfFormat::RdfXml => oxrdfxml::RdfXmlParser::new().for_slice(bytes).count_ok(),
                RdfFormat::JsonLd => oxjsonld::JsonLdParser::new().for_slice(bytes).count_ok(),
                RdfFormat::N3 => oxttl::N3Parser::new().for_slice(bytes).count_ok(),
            }
        };
        let theirs_reader = |bytes: &[u8]| -> usize {
            let reader = Chunks {
                data: bytes,
                chunk: 64 * 1024,
            };
            match format {
                RdfFormat::NTriples => oxttl::NTriplesParser::new().for_reader(reader).count_ok(),
                RdfFormat::NQuads => oxttl::NQuadsParser::new().for_reader(reader).count_ok(),
                RdfFormat::Turtle => oxttl::TurtleParser::new().for_reader(reader).count_ok(),
                RdfFormat::TriG => oxttl::TriGParser::new().for_reader(reader).count_ok(),
                RdfFormat::RdfXml => oxrdfxml::RdfXmlParser::new().for_reader(reader).count_ok(),
                RdfFormat::JsonLd => oxjsonld::JsonLdParser::new().for_reader(reader).count_ok(),
                RdfFormat::N3 => oxttl::N3Parser::new().for_reader(reader).count_ok(),
            }
        };
        let name = format.name();
        if *format == RdfFormat::N3 {
            let n3 = |bytes: &[u8]| N3Parser::new().for_slice(bytes).count_ok();
            report.case(
                &format!("{name} slice, owned"),
                len,
                data.len(),
                || n3(&bytes),
                || theirs(&bytes),
            );
            continue;
        }
        report.case(
            &format!("{name} slice, owned"),
            len,
            data.len(),
            || ours(*format, &bytes),
            || theirs(&bytes),
        );
        report.case(
            &format!("{name} slice, nrese borrowed"),
            len,
            data.len(),
            || ours_borrowed(*format, &bytes),
            || theirs(&bytes),
        );
        report.case(
            &format!("{name} reader, nrese borrowed"),
            len,
            data.len(),
            || ours_reader(*format, &bytes),
            || theirs_reader(&bytes),
        );
    }

    // N-Triples on several threads: each splits the slice and parses the parts at once.
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let nt = write(RdfFormat::NTriples, &triples);
    report.case(
        &format!("N-Triples parallel ({threads} threads)"),
        nt.len(),
        triples.len(),
        || {
            let parts = RdfParser::from_format(RdfFormat::NTriples)
                .split_slice_for_parallel_parsing(&nt, threads)
                .unwrap();
            std::thread::scope(|scope| {
                let handles: Vec<_> = parts
                    .into_iter()
                    .map(|mut part| {
                        scope.spawn(move || {
                            let mut n = 0;
                            while let Some(q) = part.next_ref() {
                                black_box(q.unwrap());
                                n += 1;
                            }
                            n
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            })
        },
        || {
            let parts = oxttl::NTriplesParser::new().split_slice_for_parallel_parsing(&nt, threads);
            std::thread::scope(|scope| {
                let handles: Vec<_> = parts
                    .into_iter()
                    .map(|part| scope.spawn(move || part.count_ok()))
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            })
        },
    );
    let ttl = write(RdfFormat::Turtle, &triples);
    report.case(
        &format!("Turtle parallel ({threads} threads)"),
        ttl.len(),
        triples.len(),
        || {
            let parts = RdfParser::from_format(RdfFormat::Turtle)
                .split_slice_for_parallel_parsing(&ttl, threads)
                .unwrap();
            std::thread::scope(|scope| {
                let handles: Vec<_> = parts
                    .into_iter()
                    .map(|mut part| {
                        scope.spawn(move || {
                            let mut n = 0;
                            while let Some(q) = part.next_ref() {
                                black_box(q.unwrap());
                                n += 1;
                            }
                            n
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            })
        },
        || {
            let parts = oxttl::TurtleParser::new().split_slice_for_parallel_parsing(&ttl, threads);
            std::thread::scope(|scope| {
                let handles: Vec<_> = parts
                    .into_iter()
                    .map(|part| scope.spawn(move || part.count_ok()))
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            })
        },
    );

    println!("\nWriting ({QUADS} statements)");
    let prefixes = |mut ttl: oxttl::TurtleSerializer| {
        for (name, iri) in PREFIXES {
            ttl = ttl.with_prefix(name, iri).unwrap();
        }
        ttl
    };
    let sizes = |format: RdfFormat, data: &[Quad]| write(format, data).len();
    report.case(
        "N-Triples write",
        sizes(RdfFormat::NTriples, &triples),
        triples.len(),
        || write(RdfFormat::NTriples, &triples).len(),
        || {
            let mut w = oxttl::NTriplesSerializer::new().for_writer(Vec::new());
            for q in &ox_triples {
                w.serialize_triple(oxrdf::TripleRef::from(q.as_ref()))
                    .unwrap();
            }
            w.finish().len()
        },
    );
    report.case(
        "N-Quads write",
        sizes(RdfFormat::NQuads, &quads),
        quads.len(),
        || write(RdfFormat::NQuads, &quads).len(),
        || {
            let mut w = oxttl::NQuadsSerializer::new().for_writer(Vec::new());
            for q in &ox_quads {
                w.serialize_quad(q).unwrap();
            }
            w.finish().len()
        },
    );
    report.case(
        "Turtle write",
        sizes(RdfFormat::Turtle, &triples),
        triples.len(),
        || write(RdfFormat::Turtle, &triples).len(),
        || {
            let mut w = prefixes(oxttl::TurtleSerializer::new()).for_writer(Vec::new());
            for q in &ox_triples {
                w.serialize_triple(oxrdf::TripleRef::from(q.as_ref()))
                    .unwrap();
            }
            w.finish().unwrap().len()
        },
    );
    report.case(
        "TriG write",
        sizes(RdfFormat::TriG, &quads),
        quads.len(),
        || write(RdfFormat::TriG, &quads).len(),
        || {
            let mut ser = oxttl::TriGSerializer::new();
            for (name, iri) in PREFIXES {
                ser = ser.with_prefix(name, iri).unwrap();
            }
            let mut w = ser.for_writer(Vec::new());
            for q in &ox_quads {
                w.serialize_quad(q).unwrap();
            }
            w.finish().unwrap().len()
        },
    );
    report.case(
        "RDF/XML write",
        sizes(RdfFormat::RdfXml, &triples),
        triples.len(),
        || write(RdfFormat::RdfXml, &triples).len(),
        || {
            let mut ser = oxrdfxml::RdfXmlSerializer::new();
            for (name, iri) in PREFIXES {
                ser = ser.with_prefix(name, iri).unwrap();
            }
            let mut w = ser.for_writer(Vec::new());
            for q in &ox_triples {
                w.serialize_triple(oxrdf::TripleRef::from(q.as_ref()))
                    .unwrap();
            }
            w.finish().unwrap().len()
        },
    );
    report.case(
        "JSON-LD write (streaming)",
        sizes(RdfFormat::JsonLd, &quads),
        quads.len(),
        || write(RdfFormat::JsonLd, &quads).len(),
        || {
            let mut ser = oxjsonld::JsonLdSerializer::new();
            for (name, iri) in PREFIXES {
                ser = ser.with_prefix(name, iri).unwrap();
            }
            let mut w = ser.for_writer(Vec::new());
            for q in &ox_quads {
                w.serialize_quad(q).unwrap();
            }
            w.finish().unwrap().len()
        },
    );
    let _ = Cursor::new(Vec::<u8>::new());
}
