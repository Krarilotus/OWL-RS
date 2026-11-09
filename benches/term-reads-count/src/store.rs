//! The store every op reads, built in-process from the seed, nothing fetched:
//! - 200,000 dictionary terms: 60 % IRIs, 30 % plain and language-tagged literals, 10 % typed
//!   literals the dictionary keeps (none of them inlined), plus 2,000 triple terms;
//! - 1,000,000 distinct asserted quads over them in 8 named graphs and the default graph, each
//!   term in at least one, and 100,000 inferred statements none of which is asserted.
//!
//! [`Shape::Memory`] is an in-memory engine. [`Shape::Reopened`] is the same data in a durable
//! engine in a temporary directory, checkpointed and opened again, so that reads go through
//! the persisted base. Background maintenance is off in both; the WAL is never synced.

use std::collections::HashSet;

use nrese_engine::{
    DurabilityConfig, EncodedQuad, EncodedTriple, Engine, EngineConfig, ReadModel, Snapshot,
    SyncPolicy, TermId,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};

use crate::Shape;

const IRIS: usize = 120_000;
const PLAIN: usize = 30_000;
const TAGGED: usize = 30_000;
const TYPED: usize = 20_000;
pub(crate) const DICTIONARY_TERMS: usize = IRIS + PLAIN + TAGGED + TYPED;
const TRIPLE_TERMS: usize = 2_000;
/// Every term the store holds: the dictionary terms, then the triple terms.
pub(crate) const TERMS: usize = DICTIONARY_TERMS + TRIPLE_TERMS;
pub(crate) const QUADS: usize = 1_000_000;
pub(crate) const INFERRED: usize = 100_000;
/// The first IRIs are the predicates, the next the named graphs.
const PREDICATES: usize = 200;
const GRAPHS: usize = 8;
const BASE: &str = "http://example.org/term-reads/";

/// A splitmix64 stream: one per purpose, all from the seed.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64, stream: u64) -> Self {
        Self(seed ^ stream.wrapping_mul(0xD1B5_4A32_D192_ED03))
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// `n` distinct numbers below `bound`, in the order drawn.
    pub(crate) fn distinct(&mut self, n: usize, bound: usize) -> Vec<usize> {
        let mut seen = HashSet::with_capacity(n);
        let mut drawn = Vec::with_capacity(n);
        while drawn.len() < n {
            let next = self.below(bound);
            if seen.insert(next) {
                drawn.push(next);
            }
        }
        drawn
    }
}

/// Term `i` of the store, or, `absent`, a term of the same kind and size the store lacks.
pub(crate) fn term(i: usize, absent: bool, rng: &mut Rng) -> Term {
    let (base, sign) = match absent {
        true => ("http://example.org/term-reads-absent/", "-"),
        false => (BASE, ""),
    };
    // Lengths vary as real data's do: 0 to 47 more characters.
    let pad = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKL";
    let pad = &pad[..rng.below(pad.len())];
    match i {
        _ if i < IRIS => Term::NamedNode(NamedNode::new_unchecked(format!("{base}r{i}/{pad}"))),
        _ if i < IRIS + PLAIN => {
            Literal::new_simple_literal(format!("{base} plain {i} {pad}")).into()
        }
        _ if i < IRIS + PLAIN + TAGGED => Literal::new_language_tagged_literal_unchecked(
            format!("{base} label {i} {pad}"),
            ["en", "de", "fr", "la"][i % 4],
        )
        .into(),
        _ if i.is_multiple_of(2) => {
            Literal::new_typed_literal(format!("{sign}{i}.5E{}", pad.len()), xsd::DOUBLE).into()
        }
        _ => Literal::new_typed_literal(
            format!("code{sign}{i}-{pad}"),
            NamedNode::new_unchecked(format!("{base}datatype")),
        )
        .into(),
    }
}

/// A built store and what the ops draw from.
pub(crate) struct Store {
    pub(crate) engine: Engine,
    /// The committed state after the load, which every op reads.
    pub(crate) snapshot: Snapshot,
    pub(crate) terms: Vec<Term>,
    /// The id of each of `terms`, as `snapshot` knows it.
    pub(crate) ids: Vec<TermId>,
    pub(crate) asserted: Vec<EncodedQuad>,
    /// In the default graph.
    pub(crate) inferred: Vec<EncodedQuad>,
    /// Removed once the engine is dropped (fields drop in order).
    _dir: Option<tempfile::TempDir>,
}

impl Store {
    pub(crate) fn build(shape: Shape, seed: u64) -> Result<Self, String> {
        let mut rng = Rng::new(seed, 1);
        let mut terms: Vec<Term> = (0..DICTIONARY_TERMS)
            .map(|i| term(i, false, &mut rng))
            .collect();
        for i in 0..TRIPLE_TERMS {
            let triple = Triple::new(
                iri(&terms, i),
                iri(&terms, i % PREDICATES),
                terms[IRIS + i].clone(),
            );
            terms.push(Term::Triple(Box::new(triple)));
        }
        // Asserted quads as term numbers [subject, predicate, object, graph] (graph 0: the
        // default graph, g: the IRI after the predicates' g-th). The first quads name every
        // term as their object.
        let mut seen: HashSet<[u32; 4]> = HashSet::with_capacity(QUADS);
        let mut asserted = Vec::with_capacity(QUADS);
        while asserted.len() < QUADS {
            let object = match asserted.len() < TERMS {
                true => asserted.len(),
                false => rng.below(TERMS),
            };
            let quad = [
                rng.below(IRIS),
                rng.below(PREDICATES),
                object,
                rng.below(GRAPHS + 1),
            ]
            .map(|n| n as u32);
            if seen.insert(quad) {
                asserted.push(quad);
            }
        }
        let mut inferred = Vec::with_capacity(INFERRED);
        while inferred.len() < INFERRED {
            let quad =
                [rng.below(IRIS), rng.below(PREDICATES), rng.below(TERMS), 0].map(|n| n as u32);
            if seen.insert(quad) {
                inferred.push(quad);
            }
        }
        let config = EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                sync: SyncPolicy::OsBuffered,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        };
        let dir = match shape {
            Shape::Memory => None,
            Shape::Reopened => {
                Some(tempfile::tempdir().map_err(|error| format!("temporary directory: {error}"))?)
            }
        };
        let engine = match &dir {
            None => Engine::new(config.clone()).map_err(failed("engine"))?,
            Some(dir) => Engine::open(dir.path(), config.clone()).map_err(failed("open"))?,
        };
        let load = engine.bulk_load(nrese_engine::BulkMode::Append);
        for batch in asserted.chunks(8192) {
            let quads: Vec<Quad> = batch.iter().map(|&quad| rdf_quad(&terms, quad)).collect();
            load.add(&quads);
        }
        load.finish().map_err(failed("bulk load"))?;
        let ids = lookup_all(&engine.snapshot(), &terms)?;
        let triples = inferred.iter().map(|&[s, p, o, _]| {
            EncodedTriple::new(ids[s as usize], ids[p as usize], ids[o as usize])
        });
        let summary = engine
            .rematerialisation()
            .finish(triples.collect())
            .map_err(failed("inferred stack"))?;
        if summary.inferred_inserted != INFERRED as u64 {
            return Err(format!(
                "{} of {INFERRED} inferred statements kept",
                summary.inferred_inserted
            ));
        }
        let engine = match &dir {
            None => engine,
            Some(dir) => {
                engine.checkpoint().map_err(failed("checkpoint"))?;
                drop(engine);
                Engine::open(dir.path(), config).map_err(failed("reopen"))?
            }
        };
        let snapshot = engine.snapshot();
        // Looked up again: a reopened store reads them from its base.
        let ids = lookup_all(&snapshot, &terms)?;
        let encode = |&[s, p, o, g]: &[u32; 4]| EncodedQuad {
            subject: ids[s as usize],
            predicate: ids[p as usize],
            object: ids[o as usize],
            graph: match g {
                0 => TermId::DEFAULT_GRAPH,
                g => ids[PREDICATES + g as usize - 1],
            },
        };
        let asserted: Vec<EncodedQuad> = asserted.iter().map(encode).collect();
        let inferred: Vec<EncodedQuad> = inferred.iter().map(encode).collect();
        let counts = [ReadModel::Asserted, ReadModel::Inferred].map(|model| snapshot.len_in(model));
        if counts != [QUADS as u64, INFERRED as u64] {
            return Err(format!(
                "the store holds {counts:?} asserted and inferred quads"
            ));
        }
        Ok(Self {
            engine,
            snapshot,
            terms,
            ids,
            asserted,
            inferred,
            _dir: dir,
        })
    }

    /// `n` quads none of `model` holds nor `taken` names, drawn over the store's terms (all
    /// in the default graph if `default_graph`), added to `taken`.
    pub(crate) fn fresh(
        &self,
        n: usize,
        default_graph: bool,
        taken: &mut HashSet<EncodedQuad>,
        rng: &mut Rng,
    ) -> Vec<EncodedQuad> {
        let mut drawn = Vec::with_capacity(n);
        while drawn.len() < n {
            let graph = match default_graph {
                true => 0,
                false => rng.below(GRAPHS + 1),
            };
            let quad = EncodedQuad {
                subject: self.ids[rng.below(IRIS)],
                predicate: self.ids[rng.below(PREDICATES)],
                object: self.ids[rng.below(TERMS)],
                graph: match graph {
                    0 => TermId::DEFAULT_GRAPH,
                    g => self.ids[PREDICATES + g - 1],
                },
            };
            if !self.snapshot.contains_in(ReadModel::Materialised, &quad) && taken.insert(quad) {
                drawn.push(quad);
            }
        }
        drawn
    }
}

fn failed(what: &'static str) -> impl Fn(nrese_engine::EngineError) -> String {
    move |error| format!("{what}: {error}")
}

fn iri(terms: &[Term], i: usize) -> NamedNode {
    match &terms[i] {
        Term::NamedNode(iri) => iri.clone(),
        _ => unreachable!("the first {IRIS} terms are IRIs"),
    }
}

fn rdf_quad(terms: &[Term], [s, p, o, g]: [u32; 4]) -> Quad {
    Quad {
        subject: NamedOrBlankNode::NamedNode(iri(terms, s as usize)),
        predicate: iri(terms, p as usize),
        object: terms[o as usize].clone(),
        graph_name: match g {
            0 => GraphName::DefaultGraph,
            g => GraphName::NamedNode(iri(terms, PREDICATES + g as usize - 1)),
        },
    }
}

/// Every term's id; each must be a dictionary term, which the ops read.
fn lookup_all(snapshot: &Snapshot, terms: &[Term]) -> Result<Vec<TermId>, String> {
    terms
        .iter()
        .map(|term| match snapshot.lookup(term.as_ref()) {
            Some(id) if id.kind().is_dictionary() => Ok(id),
            Some(_) => Err(format!("{term} is inlined, not a dictionary term")),
            None => Err(format!("{term} isn't in the store")),
        })
        .collect()
}
