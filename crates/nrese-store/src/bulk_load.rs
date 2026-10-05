//! Bulk loading of RDF files (E5): initial loads and full restores that bypass the mutation
//! pipeline.
//!
//! - **Parsing.** N-Triples and N-Quads files are split into chunks and parsed on all cores.
//!   Other formats are parsed on one thread while interning runs in parallel. Turtle is not
//!   split, because splitting it is only best effort (prefixes declared mid-file and
//!   multi-line literals can break chunking).
//! - **Blank nodes** are fresh per load, like every other write path: each label gets a
//!   prefix unique to this load, applied identically in every chunk, so labels stay
//!   consistent within the load.
//! - **Gates.** A bulk load is an operator action on an offline or otherwise idle store. It
//!   doesn't run the validation gates; SHACL and reasoning validate the result when they
//!   next run in full (M2, M3).
//! - **Bad statements.** A syntax error stops the load, unless `skip_errors`: then the
//!   statement is skipped (a line of N-Triples or N-Quads, a statement of Turtle or TriG up
//!   to its `.`, the rest of an RDF/XML node element below `rdf:RDF`, a top-level node
//!   object of JSON-LD), logged (the first [`LOGGED_ERRORS`]) and counted, and the load
//!   goes on. Other formats, malformed XML or JSON, still stop; a read error always does.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use nrese_engine::{BulkLoad, BulkMode, Engine, memory};
use nrese_rdf::{BlankNode, GraphName, NamedOrBlankNode, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser};
use rayon::prelude::*;

use crate::error::{StoreError, StoreResult};
use crate::graph_store::GraphTarget;
use crate::rdf_io::file_base_iri;

/// Quads per interning batch: large enough to amortise the dictionary lock, small enough
/// to keep all cores busy.
const BATCH: usize = 32 * 1024;
/// Batches a parsing thread adds between releases of the memory it freed ([`memory`]):
/// about 256 thousand quads. Each release takes well under a millisecond; at one per two
/// million quads per thread, freed memory reached 3 GB at the end of DBpedia's parse.
const BATCHES_PER_RELEASE: u64 = 8;
/// Skipped statements logged one by one (the rest are only counted).
const LOGGED_ERRORS: u64 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkLoadRequest {
    /// RDF files; the format is taken from each file's extension.
    pub files: Vec<PathBuf>,
    /// Replace the dataset instead of adding to it.
    pub replace: bool,
    /// Graph for triple formats (N-Triples, Turtle, RDF/XML); quad formats keep their own.
    pub graph: GraphTarget,
    /// Skip statements with syntax errors (logged and counted) instead of stopping.
    pub skip_errors: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkLoadReport {
    pub revision: u64,
    /// Quads parsed, including duplicates.
    pub parsed: u64,
    /// Asserted quads added and removed.
    pub inserted: u64,
    pub deleted: u64,
    /// Statements skipped for syntax errors (`skip_errors`).
    pub skipped: u64,
    pub elapsed: Duration,
}

/// What `skip_errors` makes of syntax errors: `None` to stop with one.
struct Skipping {
    skipped: AtomicU64,
}

impl Skipping {
    /// `Ok` (counted and maybe logged) for a syntax error to skip, the error otherwise.
    fn skip(&self, path: &Path, error: RdfParseError) -> StoreResult<()> {
        let RdfParseError::Syntax(syntax) = error else {
            return Err(error.into());
        };
        let n = self.skipped.fetch_add(1, Ordering::Relaxed);
        if n < LOGGED_ERRORS {
            tracing::warn!(file = %path.display(), error = %syntax, "skipped a statement");
        } else if n == LOGGED_ERRORS {
            tracing::warn!(file = %path.display(), "more statements skipped; only counted from here");
        }
        Ok(())
    }
}

/// How far a bulk load is, and a way to stop it: shared with whoever watches it (an import
/// job). Cancelling stops the load before its next file; nothing of it is committed.
#[derive(Debug, Default)]
pub struct LoadProgress {
    files_done: AtomicU64,
    files: AtomicU64,
    parsed: AtomicU64,
    cancelled: std::sync::atomic::AtomicBool,
}

impl LoadProgress {
    /// Files loaded, of how many.
    pub fn files(&self) -> (u64, u64) {
        (
            self.files_done.load(Ordering::Relaxed),
            self.files.load(Ordering::Relaxed),
        )
    }

    /// Statements parsed so far (whole files).
    pub fn parsed(&self) -> u64 {
        self.parsed.load(Ordering::Relaxed)
    }

    /// Asks the load to stop before its next file.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub(crate) fn bulk_load(
    engine: &Engine,
    request: &BulkLoadRequest,
    progress: &LoadProgress,
) -> StoreResult<BulkLoadReport> {
    let started = Instant::now();
    let sources = request
        .files
        .iter()
        .map(|path| Source::for_file(path))
        .collect::<StoreResult<Vec<_>>>()?;
    let graph = request.graph.graph_name()?;
    let mode = match request.replace {
        true => BulkMode::Replace,
        false => BulkMode::Append,
    };
    let load = engine.bulk_load(mode);
    let blank_nodes = BlankNodeScope::new(engine.snapshot().revision());
    let skipping = request.skip_errors.then(|| Skipping {
        skipped: AtomicU64::new(0),
    });
    let mut parsed = 0;
    progress
        .files
        .store(sources.len() as u64, Ordering::Relaxed);
    for source in &sources {
        // Dropping the load unfinished aborts it: nothing is committed.
        if progress.is_cancelled() {
            return Err(StoreError::LoadCancelled);
        }
        let quads = source.load_into(&load, &graph, &blank_nodes, skipping.as_ref())?;
        parsed += quads;
        progress.parsed.fetch_add(quads, Ordering::Relaxed);
        progress.files_done.fetch_add(1, Ordering::Relaxed);
    }
    if progress.is_cancelled() {
        return Err(StoreError::LoadCancelled);
    }
    let summary = load.finish();
    memory::release_all();
    let summary = summary?;
    Ok(BulkLoadReport {
        revision: summary.revision,
        parsed,
        inserted: summary.inserted,
        deleted: summary.deleted,
        skipped: skipping.map_or(0, |s| s.skipped.into_inner()),
        elapsed: started.elapsed(),
    })
}

struct Source<'a> {
    path: &'a Path,
    format: RdfFormat,
}

impl<'a> Source<'a> {
    fn for_file(path: &'a Path) -> StoreResult<Self> {
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(RdfFormat::from_extension)
            .ok_or_else(|| {
                StoreError::Configuration(format!(
                    "cannot infer the RDF format of {} from its extension",
                    path.display()
                ))
            })?;
        Ok(Self { path, format })
    }

    /// Parses the file into `load`; returns the number of quads parsed.
    fn load_into(
        &self,
        load: &BulkLoad<'_>,
        graph: &GraphName,
        blank_nodes: &BlankNodeScope,
        skipping: Option<&Skipping>,
    ) -> StoreResult<u64> {
        let threads = rayon::current_num_threads();
        let recovering = |parser: RdfParser| match skipping {
            Some(_) => parser.recovering(),
            None => parser,
        };
        match self.format {
            // Split exactly (Turtle and TriG at statement ends, by a skim of the file) and
            // parsed on every thread.
            RdfFormat::NTriples | RdfFormat::NQuads | RdfFormat::Turtle | RdfFormat::TriG => {
                let parser = RdfParser::from_format(self.format)
                    .with_base_iri(file_base_iri(self.path)?)
                    .map_err(|error| StoreError::Configuration(error.to_string()))?;
                let parser = match self.format.supports_datasets() {
                    true => parser,
                    false => parser.with_default_graph(graph.clone()),
                };
                let chunks =
                    recovering(parser).split_file_for_parallel_parsing(self.path, threads)?;
                chunks
                    .into_par_iter()
                    .map(|chunk| feed(load, chunk, blank_nodes, skipping, self.path))
                    .sum()
            }
            format => {
                let parser = RdfParser::from_format(format)
                    .with_base_iri(file_base_iri(self.path)?)
                    .map_err(|error| StoreError::Configuration(error.to_string()))?;
                let parser = match format.supports_datasets() {
                    true => parser,
                    false => parser.with_default_graph(graph.clone()),
                };
                let quads = recovering(parser).for_reader(BufReader::new(File::open(self.path)?));
                // Parse here, intern on the pool.
                rayon::scope(|scope| {
                    let mut batch = Vec::with_capacity(BATCH);
                    let mut parsed = 0;
                    for quad in quads {
                        let quad = match (quad, skipping) {
                            (Ok(quad), _) => quad,
                            (Err(error), Some(skipping)) => {
                                skipping.skip(self.path, error)?;
                                continue;
                            }
                            (Err(error), None) => return Err(error.into()),
                        };
                        batch.push(blank_nodes.scope(quad));
                        if batch.len() == BATCH {
                            parsed += batch.len() as u64;
                            let full = std::mem::replace(&mut batch, Vec::with_capacity(BATCH));
                            scope.spawn(move |_| load.add(&full));
                        }
                    }
                    parsed += batch.len() as u64;
                    load.add(&batch);
                    Ok(parsed)
                })
            }
        }
        .map_err(|error: StoreError| match error {
            StoreError::RdfParse(source) => StoreError::FileParse {
                path: self.path.to_path_buf(),
                source,
            },
            other => other,
        })
    }
}

/// Parses `quads` (of the file `path`) in batches into `load` on the current thread;
/// returns the count.
fn feed(
    load: &BulkLoad<'_>,
    quads: impl Iterator<Item = Result<Quad, RdfParseError>>,
    blank_nodes: &BlankNodeScope,
    skipping: Option<&Skipping>,
    path: &Path,
) -> StoreResult<u64> {
    let mut batch = Vec::with_capacity(BATCH);
    let mut parsed = 0;
    for quad in quads {
        let quad = match (quad, skipping) {
            (Ok(quad), _) => quad,
            (Err(error), Some(skipping)) => {
                skipping.skip(path, error)?;
                continue;
            }
            (Err(error), None) => return Err(error.into()),
        };
        batch.push(blank_nodes.scope(quad));
        if batch.len() == BATCH {
            load.add(&batch);
            parsed += batch.len() as u64;
            batch.clear();
            if (parsed / BATCH as u64).is_multiple_of(BATCHES_PER_RELEASE) {
                memory::release_thread();
            }
        }
    }
    load.add(&batch);
    Ok(parsed + batch.len() as u64)
}

/// Makes blank-node labels unique to one load by prefixing them. Deterministic per label,
/// so every chunk of the load maps a label the same way.
struct BlankNodeScope {
    prefix: String,
}

impl BlankNodeScope {
    fn new(revision: u64) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            prefix: format!("bulk{revision:x}x{nanos:x}x"),
        }
    }

    fn node(&self, node: &BlankNode) -> BlankNode {
        BlankNode::new_unchecked(format!("{}{}", self.prefix, node.as_str()))
    }

    /// `term` with its blank nodes, inside triple terms too, in this load's scope.
    fn term(&self, term: Term) -> Term {
        match term {
            Term::BlankNode(node) => self.node(&node).into(),
            Term::Triple(triple) => {
                let triple = *triple;
                nrese_rdf::Triple::new(
                    match triple.subject {
                        NamedOrBlankNode::BlankNode(node) => self.node(&node).into(),
                        named => named,
                    },
                    triple.predicate,
                    self.term(triple.object),
                )
                .into()
            }
            other => other,
        }
    }

    fn scope(&self, quad: Quad) -> Quad {
        let Quad {
            subject,
            predicate,
            object,
            graph_name,
        } = quad;
        Quad {
            subject: match subject {
                NamedOrBlankNode::BlankNode(node) => self.node(&node).into(),
                named => named,
            },
            predicate,
            object: self.term(object),
            graph_name: match graph_name {
                GraphName::BlankNode(node) => self.node(&node).into(),
                other => other,
            },
        }
    }
}
