//! Fuzz targets for every parser of untrusted input (the audit plan's F1): what a client
//! sends over HTTP (RDF in every format, SPARQL queries and updates) and what a federated
//! `SERVICE` answers (SPARQL results).
//!
//! Each target takes arbitrary bytes and checks two things:
//! - **It doesn't panic.** Any input either parses or is refused with an error.
//! - **What it reads, it writes back.** RDF that parses is written as N-Quads and read back
//!   to the same quads, and written in its own format (where that format can hold it) and
//!   read back to as many quads. A SPARQL query or update that parses prints as text that
//!   parses to the same text. Results that parse are written in their format and read back
//!   to the same solutions.
//!
//! The targets run in two ways:
//! - on stable Rust, from corpora and mutations of them (`tests/it`, also in
//!   `scripts/fuzz-campaign.sh`);
//! - coverage-guided with `cargo fuzz` on nightly (`fuzz/` in the repository's root).

use std::collections::BTreeSet;

use nrese_rdf::Quad;
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, SliceQueryResultsParserOutput,
};
use nrese_sparql_syntax::{DEFAULT_MAX_NESTING, SparqlParser};

/// The largest input a target reads (larger inputs are cut): fuzzing looks for wrong
/// handling, not for slowness on huge documents.
pub const MAX_INPUT: usize = 64 * 1024;

/// A fuzz target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    NTriples,
    NQuads,
    Turtle,
    TriG,
    N3,
    RdfXml,
    JsonLd,
    SparqlQuery,
    SparqlUpdate,
    ResultsJson,
    ResultsXml,
    ResultsTsv,
}

impl Target {
    pub const ALL: [Target; 12] = [
        Target::NTriples,
        Target::NQuads,
        Target::Turtle,
        Target::TriG,
        Target::N3,
        Target::RdfXml,
        Target::JsonLd,
        Target::SparqlQuery,
        Target::SparqlUpdate,
        Target::ResultsJson,
        Target::ResultsXml,
        Target::ResultsTsv,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Target::NTriples => "ntriples",
            Target::NQuads => "nquads",
            Target::Turtle => "turtle",
            Target::TriG => "trig",
            Target::N3 => "n3",
            Target::RdfXml => "rdfxml",
            Target::JsonLd => "jsonld",
            Target::SparqlQuery => "sparql-query",
            Target::SparqlUpdate => "sparql-update",
            Target::ResultsJson => "results-json",
            Target::ResultsXml => "results-xml",
            Target::ResultsTsv => "results-tsv",
        }
    }

    /// The file extensions of its corpus files.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Target::NTriples => &["nt"],
            Target::NQuads => &["nq"],
            Target::Turtle => &["ttl"],
            Target::TriG => &["trig"],
            Target::N3 => &["n3"],
            Target::RdfXml => &["rdf"],
            Target::JsonLd => &["jsonld"],
            Target::SparqlQuery => &["rq"],
            Target::SparqlUpdate => &["ru"],
            Target::ResultsJson => &["srj"],
            Target::ResultsXml => &["srx"],
            Target::ResultsTsv => &["tsv"],
        }
    }

    /// Runs the target on `data`; panics on a finding.
    pub fn run(self, data: &[u8]) {
        let data = &data[..data.len().min(MAX_INPUT)];
        match self {
            Target::NTriples => rdf(RdfFormat::NTriples, data),
            Target::NQuads => rdf(RdfFormat::NQuads, data),
            Target::Turtle => rdf(RdfFormat::Turtle, data),
            Target::TriG => rdf(RdfFormat::TriG, data),
            Target::N3 => rdf(RdfFormat::N3, data),
            Target::RdfXml => rdf(RdfFormat::RdfXml, data),
            Target::JsonLd => rdf(RdfFormat::JsonLd, data),
            Target::SparqlQuery => sparql(data, false),
            Target::SparqlUpdate => sparql(data, true),
            Target::ResultsJson => results(QueryResultsFormat::Json, data),
            Target::ResultsXml => results(QueryResultsFormat::Xml, data),
            Target::ResultsTsv => results(QueryResultsFormat::Tsv, data),
        }
    }
}

/// Every quad of `data` in `format`, or `None` if it doesn't parse.
fn parse(format: RdfFormat, data: &[u8]) -> Option<Vec<Quad>> {
    RdfParser::from_format(format)
        .for_slice(data)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn write(format: RdfFormat, quads: &[Quad]) -> std::io::Result<Vec<u8>> {
    let mut serializer = RdfSerializer::from_format(format).for_writer(Vec::new());
    for quad in quads {
        serializer.serialize_quad(quad)?;
    }
    serializer.finish()
}

fn rdf(format: RdfFormat, data: &[u8]) {
    let Some(quads) = parse(format, data) else {
        return;
    };
    let set: BTreeSet<&Quad> = quads.iter().collect();
    // N-Quads holds every quad exactly, blank node labels included.
    let written = write(RdfFormat::NQuads, &quads).expect("N-Quads holds every quad");
    let back = parse(RdfFormat::NQuads, &written).unwrap_or_else(|| {
        panic!(
            "{format:?}: its quads written as N-Quads don't parse:\n{}",
            String::from_utf8_lossy(&written)
        )
    });
    let back_set: BTreeSet<&Quad> = back.iter().collect();
    assert_eq!(
        set,
        back_set,
        "{format:?}: N-Quads round trip changed the quads:\n{}",
        String::from_utf8_lossy(&written)
    );
    // Its own format: as many distinct quads (blank nodes may be relabelled). RDF/XML
    // can't write every predicate (one without a local name), so a refusal is allowed
    // there. N3 writes Turtle, which has no named graphs, and its formulas are read as
    // graphs named by blank nodes: TriG holds them.
    let own = match format {
        RdfFormat::N3 => RdfFormat::TriG,
        other => other,
    };
    match write(own, &quads) {
        Ok(written) => {
            let back = parse(own, &written).unwrap_or_else(|| {
                panic!(
                    "{format:?}: its quads written in their own format don't parse:\n{}",
                    String::from_utf8_lossy(&written)
                )
            });
            let back: BTreeSet<&Quad> = back.iter().collect();
            assert_eq!(
                back.len(),
                set.len(),
                "{format:?}: own-format round trip changed the number of quads:\n{}",
                String::from_utf8_lossy(&written)
            );
        }
        Err(_) if format == RdfFormat::RdfXml => {}
        Err(error) => panic!("{format:?}: its quads can't be written back: {error}"),
    }
}

fn sparql(data: &[u8], update: bool) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // The printed form may need a few nesting levels more than the text it came from (a
    // FILTER's brackets, a binary expression's): the limit guards resources, so reading it
    // back allows a margin.
    let print = |text: &str, margin: usize| -> Option<String> {
        let parser = SparqlParser::new().with_max_nesting(DEFAULT_MAX_NESTING + margin);
        if update {
            parser.parse_update(text).ok().map(|u| u.to_string())
        } else {
            parser.parse_query(text).ok().map(|q| q.to_string())
        }
    };
    let Some(printed) = print(text, 0) else {
        return;
    };
    let again = print(&printed, 8)
        .unwrap_or_else(|| panic!("printed form doesn't parse:\n{printed}\nfrom:\n{text}"));
    assert_eq!(again, printed, "printing isn't stable; from:\n{text}");
}

fn results(format: QueryResultsFormat, data: &[u8]) {
    let read = |data: &[u8]| -> Option<Result<bool, (Vec<_>, Vec<_>)>> {
        match QueryResultsParser::from_format(format)
            .for_slice(data)
            .ok()?
        {
            SliceQueryResultsParserOutput::Boolean(value) => Some(Ok(value)),
            SliceQueryResultsParserOutput::Solutions(solutions) => {
                let variables = solutions.variables().to_vec();
                let rows: Vec<Vec<_>> = solutions
                    .map(|solution| solution.map(|s| s.values().to_vec()))
                    .collect::<Result<_, _>>()
                    .ok()?;
                Some(Err((variables, rows)))
            }
        }
    };
    let Some(first) = read(data) else {
        return;
    };
    let serializer = QueryResultsSerializer::from_format(format);
    let written = match &first {
        Ok(value) => serializer
            .serialize_boolean_to_writer(Vec::new(), *value)
            .expect("in memory"),
        Err((variables, rows)) => {
            let mut writer = serializer
                .serialize_solutions_to_writer(Vec::new(), variables.clone())
                .expect("in memory");
            for row in rows {
                let row: Vec<_> = row.iter().map(|v| v.as_ref().map(Into::into)).collect();
                writer.serialize_row(&row).expect("in memory");
            }
            writer.finish().expect("in memory")
        }
    };
    let back = read(&written).unwrap_or_else(|| {
        panic!(
            "{format:?}: written results don't parse:\n{}",
            String::from_utf8_lossy(&written)
        )
    });
    assert_eq!(
        back,
        first,
        "{format:?}: results round trip changed them:\n{}",
        String::from_utf8_lossy(&written)
    );
}
