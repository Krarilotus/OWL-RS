//! RDF parsing and serialisation for the store: the only place that knows `nrese_rdf_io`.
//!
//! Blank nodes: request payloads get fresh blank nodes ([`BlankNodes::Fresh`]), so labels
//! are scoped to one payload and two documents that both use `_:b0` never merge. Within a
//! payload labels stay consistent, so a restore reproduces the dump's structure. The startup
//! ontology preload keeps the document's labels ([`BlankNodes::AsWritten`]) so that
//! re-preloading the same file on every restart of a durable store is idempotent.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use nrese_exec::{ChargedVec, Reservation};
use nrese_rdf::{
    GraphName, GraphNameRef, NamedNodeRef, NamedOrBlankNodeRef, Quad, QuadRef, TermRef, Triple,
};
use nrese_rdf_io::{QuadParser, RdfFormat, RdfParser, RdfSerializer};
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
            Self::JsonLd => RdfFormat::JsonLd,
            Self::BinaryRdf => RdfFormat::BinaryRdf,
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

// Uploads are read as they go: each statement goes to its consumer borrowed from the
// parser's buffers (a write's transaction, a validation's speculative one, a check) and is
// gone before the next. A document's statements are never kept as owned copies uncharged:
// with one long namespace and many short statements, those copies cost thousands of times
// the document (the upload sizing). A caller that must keep them gets [`ChargedQuads`].

/// Gives `each` every quad of `quads` in order, borrowed until the next; the first error,
/// the parser's or `each`'s, ends it.
fn each_of<R: Read>(
    mut quads: QuadParser<'_, R>,
    mut each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    while let Some(quad) = quads.next_ref() {
        each(quad?)?;
    }
    Ok(())
}

/// What a consumer of an upload's quads does that an error part-way can't undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consumer {
    /// It interns terms into the engine's dictionary (an insert, even into a transaction
    /// that is then discarded): the dictionary is shared and keeps them. It is given the
    /// document's quads only once the whole document has been read and found valid, by a
    /// first pass that borrows each quad and keeps nothing.
    Interning,
    /// Its effects end with the operation: lookups, a scan of its own.
    Local,
}

/// The parser's quads of `payload` to `each` as `consumer` needs them: for an interning
/// consumer, only after a first pass has read the whole payload without an error.
fn each_valid<'p>(
    quads: impl Fn() -> StoreResult<QuadParser<'p, std::io::Empty>>,
    consumer: Consumer,
    each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    if consumer == Consumer::Interning {
        each_of(quads()?, |_| Ok(()))?;
    }
    each_of(quads()?, each)
}

/// The quads of a single-graph payload, in `graph`, to `each` (as `consumer` needs them).
/// Named-graph content in the payload (possible in N-Quads, TriG and JSON-LD) is an error.
pub(crate) fn each_graph_quad(
    format: GraphResultFormat,
    base_iri: Option<&str>,
    payload: &[u8],
    graph: GraphName,
    blank_nodes: BlankNodes,
    consumer: Consumer,
    each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    let quads = || {
        Ok(parser(format.rdf_format(), base_iri, blank_nodes)?
            .without_named_graphs()
            .with_default_graph(graph.clone())
            .for_slice(payload))
    };
    each_valid(quads, consumer, each)
}

/// [`each_graph_quad`] in one pass, from a reader: for a consumer whose interned terms go
/// with its engine when it fails (the startup preload: a store that fails to open drops its
/// engine).
pub(crate) fn each_graph_quad_once(
    format: GraphResultFormat,
    base_iri: Option<&str>,
    payload: impl Read,
    graph: GraphName,
    blank_nodes: BlankNodes,
    each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    let quads = parser(format.rdf_format(), base_iri, blank_nodes)?
        .without_named_graphs()
        .with_default_graph(graph)
        .for_reader(payload);
    each_of(quads, each)
}

/// The store graph `graph` names: the default graph for another store's name for it
/// (`urn:x-arq:DefaultGraph`, `urn:x-rdflib:default`: [`nrese_sparql::compat`]), which every
/// SPARQL name of it reads; otherwise `graph`. The store's one placement of a graph name: a
/// document's graphs, an explicit context or target, a restore's and a bulk load's graphs all
/// pass through here, after any protocol override and before any permission check.
pub(crate) fn stored_graph(graph: GraphName) -> GraphName {
    match &graph {
        GraphName::NamedNode(name) if nrese_sparql::compat::names_default_graph(name.as_str()) => {
            GraphName::DefaultGraph
        }
        _ => graph,
    }
}

/// [`stored_graph`] of a borrowed name, for a quad read as it goes.
pub(crate) fn stored_graph_ref(graph: GraphNameRef<'_>) -> GraphNameRef<'_> {
    match graph {
        GraphNameRef::NamedNode(name)
            if nrese_sparql::compat::names_default_graph(name.as_str()) =>
        {
            GraphNameRef::DefaultGraph
        }
        _ => graph,
    }
}

/// `quad` in the graph the store keeps it in ([`stored_graph`]).
pub(crate) fn stored(mut quad: Quad) -> Quad {
    quad.graph_name = stored_graph(quad.graph_name);
    quad
}

/// The quads of a request payload, in the graphs it names as written (the default graph for
/// triples): a protocol can still tell a graph left out from one named, an alias of the
/// default graph included; where they are placed, they pass through [`stored_graph_ref`].
/// Each to `each` (as `consumer` needs them).
pub(crate) fn each_payload_quad(
    format: RdfFormat,
    base_iri: Option<&str>,
    payload: &[u8],
    blank_nodes: BlankNodes,
    consumer: Consumer,
    each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    let quads = || Ok(parser(format, base_iri, blank_nodes)?.for_slice(payload));
    each_valid(quads, consumer, each)
}

/// The quads of a request payload that must be kept (a repository's configuration, read in
/// several passes), in the graphs it names, with fresh blank nodes: each charged to
/// `memory` before it is copied, its slot and every string and triple term it owns. A
/// refusal is a memory error, with nothing copied past it. The graphs are as written.
pub fn parse_payload(
    format: GraphResultFormat,
    base_iri: Option<&str>,
    payload: &[u8],
    memory: &nrese_exec::Envelope,
) -> StoreResult<ChargedQuads> {
    let budget = Arc::new(memory.part());
    let mut kept = ChargedQuads {
        quads: ChargedVec::new(Arc::clone(&budget)),
        terms: Reservation::new(budget),
    };
    each_payload_quad(
        format.rdf_format(),
        base_iri,
        payload,
        BlankNodes::Fresh,
        Consumer::Local,
        |quad| {
            kept.terms.charge(owned_quad_bytes(quad))?;
            kept.quads.push(quad.into_owned())?;
            Ok(())
        },
    )?;
    Ok(kept)
}

/// Quads kept with their memory charged: their slots, and the strings and triple terms they
/// own. Read them through the slice; they can't be taken out uncharged or cloned. Dropped,
/// the quads are freed before their charge is released.
#[derive(Debug)]
pub struct ChargedQuads {
    /// Before `terms`: the quads (and their strings) are freed first.
    quads: ChargedVec<Quad>,
    terms: Reservation,
}

impl std::ops::Deref for ChargedQuads {
    type Target = [Quad];

    fn deref(&self) -> &[Quad] {
        &self.quads
    }
}

/// The heap bytes an owned copy of `quad` allocates, at most: each IRI, label, literal
/// value, language and datatype as many bytes as its text (an owned copy of a `&str`
/// allocates exactly its length), each triple term its box and its own. Saturating: a sum
/// past the address space is one the charge refuses.
fn owned_quad_bytes(quad: QuadRef<'_>) -> usize {
    let graph = match quad.graph_name {
        GraphNameRef::DefaultGraph => 0,
        GraphNameRef::NamedNode(node) => node.as_str().len(),
        GraphNameRef::BlankNode(node) => node.as_str().len(),
    };
    owned_triple_bytes(quad.subject, quad.predicate, quad.object).saturating_add(graph)
}

fn owned_triple_bytes(
    subject: NamedOrBlankNodeRef<'_>,
    predicate: NamedNodeRef<'_>,
    object: TermRef<'_>,
) -> usize {
    let subject = match subject {
        NamedOrBlankNodeRef::NamedNode(node) => node.as_str().len(),
        NamedOrBlankNodeRef::BlankNode(node) => node.as_str().len(),
    };
    subject
        .saturating_add(predicate.as_str().len())
        .saturating_add(owned_term_bytes(object))
}

fn owned_term_bytes(term: TermRef<'_>) -> usize {
    match term {
        TermRef::NamedNode(node) => node.as_str().len(),
        TermRef::BlankNode(node) => node.as_str().len(),
        // An upper bound: a simple literal's datatype is no allocation of its own.
        TermRef::Literal(literal) => literal
            .value()
            .len()
            .saturating_add(literal.language().map_or(0, str::len))
            .saturating_add(literal.datatype().as_str().len()),
        TermRef::Triple(triple) => {
            std::mem::size_of::<Triple>().saturating_add(owned_triple_bytes(
                (&triple.subject).into(),
                (&triple.predicate).into(),
                (&triple.object).into(),
            ))
        }
    }
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

/// The quads of `patterns` in `view` under `model`, written to `out` in `format` as they
/// are read, one decoded at a time: a whole store in the serialiser's constant memory. With
/// `graphs` each quad keeps its graph (a format without graphs refuses a named one);
/// otherwise each is written as a triple. `cancel` stops it before any quad. Returns how
/// many, and `out`.
pub(crate) fn write_quads<W: std::io::Write>(
    view: &impl nrese_sparql::ReadView,
    model: nrese_engine::ReadModel,
    patterns: &[nrese_engine::QuadPattern],
    (format, graphs): (RdfFormat, bool),
    cancel: &nrese_sparql::CancellationToken,
    out: W,
) -> StoreResult<(u64, W)> {
    let mut writer = RdfSerializer::from_format(format).for_writer(out);
    let mut written = 0;
    for pattern in patterns {
        for quad in crate::view::decoded_quads(view, model, pattern) {
            cancel.check()?;
            let quad = quad?;
            match graphs {
                true => writer.serialize_quad(&quad)?,
                false => writer.serialize_triple(&Triple::new(
                    quad.subject,
                    quad.predicate,
                    quad.object,
                ))?,
            }
            written += 1;
        }
    }
    Ok((written, writer.finish()?))
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

/// Converts the RDF file `input` into `output`, each format taken from its file's
/// extension, statement by statement (no store involved, any size): `nrese-server convert`.
/// Blank node labels are kept; a named graph is an error for a format without graphs. The
/// output is written to `<output>.tmp` and renamed when complete. Returns the statements.
pub fn convert_file(input: &Path, output: &Path) -> StoreResult<u64> {
    let format = |path: &Path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .and_then(RdfFormat::from_extension)
            .ok_or_else(|| {
                StoreError::Configuration(format!(
                    "cannot infer the RDF format of {} from its extension",
                    path.display()
                ))
            })
    };
    let (from, to) = (format(input)?, format(output)?);
    let parser = RdfParser::from_format(from)
        .with_base_iri(file_base_iri(input)?)
        .map_err(|error| StoreError::Configuration(error.to_string()))?;
    let reader = std::io::BufReader::new(std::fs::File::open(input)?);
    let partial = output.with_extension(format!(
        "{}.tmp",
        output
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    let mut serializer =
        RdfSerializer::from_format(to).for_writer(std::fs::File::create(&partial)?);
    let mut statements = 0;
    let written = (|| -> StoreResult<()> {
        for quad in parser.for_reader(reader) {
            serializer.serialize_quad(&quad?)?;
            statements += 1;
        }
        serializer.finish()?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, output)?;
    Ok(statements)
}
