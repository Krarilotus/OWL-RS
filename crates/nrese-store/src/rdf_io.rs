//! RDF parsing and serialisation for the store: the only place that knows `oxrdfio`.
//!
//! Blank nodes: request payloads get fresh blank nodes ([`BlankNodes::Fresh`]), so labels
//! are scoped to one payload and two documents that both use `_:b0` never merge. Within a
//! payload labels stay consistent, so a restore reproduces the dump's structure. The startup
//! ontology preload keeps the document's labels ([`BlankNodes::AsWritten`]) so that
//! re-preloading the same file on every restart of a durable store is idempotent.

use std::io::Read;
use std::path::Path;

use oxrdf::{GraphName, Quad, Triple};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use url::Url;

use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;

impl GraphResultFormat {
    pub(crate) fn rdf_format(self) -> RdfFormat {
        match self {
            Self::NTriples => RdfFormat::NTriples,
            Self::Turtle => RdfFormat::Turtle,
            Self::RdfXml => RdfFormat::RdfXml,
            Self::NQuads => RdfFormat::NQuads,
            Self::TriG => RdfFormat::TriG,
            Self::JsonLd => RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlankNodes {
    Fresh,
    AsWritten,
}

fn parser(
    format: RdfFormat,
    base_iri: Option<&str>,
    blank_nodes: BlankNodes,
) -> StoreResult<RdfParser> {
    let parser = RdfParser::from_format(format);
    let parser = match blank_nodes {
        BlankNodes::Fresh => parser.rename_blank_nodes(),
        BlankNodes::AsWritten => parser,
    };
    match base_iri {
        None => Ok(parser),
        Some(base_iri) => parser.with_base_iri(base_iri).map_err(|error| {
            StoreError::Configuration(format!("invalid RDF parser base IRI '{base_iri}': {error}"))
        }),
    }
}

/// Parses a single-graph payload into quads of `graph`. Named-graph content in the payload
/// (possible in N-Quads, TriG and JSON-LD) is an error.
pub(crate) fn parse_graph(
    format: GraphResultFormat,
    base_iri: Option<&str>,
    payload: impl Read,
    graph: GraphName,
    blank_nodes: BlankNodes,
) -> StoreResult<Vec<Quad>> {
    parser(format.rdf_format(), base_iri, blank_nodes)?
        .without_named_graphs()
        .with_default_graph(graph)
        .for_reader(payload)
        .map(|quad| quad.map_err(StoreError::from))
        .collect()
}

/// Parses a dataset payload (N-Quads) into quads.
pub(crate) fn parse_dataset(format: RdfFormat, payload: &[u8]) -> StoreResult<Vec<Quad>> {
    parser(format, None, BlankNodes::Fresh)?
        .for_slice(payload)
        .map(|quad| quad.map_err(|error| StoreError::RdfParse(error.into())))
        .collect()
}

pub(crate) fn serialize_triples(
    format: GraphResultFormat,
    triples: impl IntoIterator<Item = Triple>,
) -> StoreResult<Vec<u8>> {
    let mut writer = RdfSerializer::from_format(format.rdf_format()).for_writer(Vec::new());
    for triple in triples {
        writer.serialize_triple(&triple)?;
    }
    Ok(writer.finish()?)
}

pub(crate) fn serialize_quads(
    format: RdfFormat,
    quads: impl IntoIterator<Item = Quad>,
) -> StoreResult<Vec<u8>> {
    let mut writer = RdfSerializer::from_format(format).for_writer(Vec::new());
    for quad in quads {
        writer.serialize_quad(&quad)?;
    }
    Ok(writer.finish()?)
}

pub(crate) fn file_base_iri(path: &Path) -> StoreResult<String> {
    let canonical = path.canonicalize()?;
    Url::from_file_path(&canonical)
        .map(|url| url.into())
        .map_err(|()| {
            StoreError::Configuration(format!(
                "cannot derive file base IRI from ontology path {}",
                canonical.display()
            ))
        })
}
