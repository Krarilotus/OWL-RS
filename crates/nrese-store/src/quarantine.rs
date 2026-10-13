//! The quarantine: the statements a bulk load could not read, kept per repository until
//! someone resolves them (docs/design/quarantine.md).
//!
//! - **What is kept.** Each statement verbatim, as the parser skipped it (a line of
//!   N-Triples or N-Quads, a Turtle or TriG statement up to its `.`), with what reading it
//!   again needs (the `@base` and `@prefix` directives in effect, its TriG block's graph,
//!   the load's graph and base IRI), its file, the document's line, the reason and the load.
//!   RDF/XML and JSON-LD give the line and the reason; their text is not kept.
//! - **Invisible.** A quarantined statement is never in the dataset: queries, reasoning,
//!   exports and statistics don't see it, by construction.
//! - **Resolution.** Listed and exported as JSON lines; re-admitted, as kept or fixed, and
//!   with repairs only on request ([`Repair`]), through the mutation pipeline like any
//!   write ([`readmit`]), or discarded. Resolutions are recorded with what was admitted.
//! - **Kept** in `quarantine/` of the data directory: one file per load with quarantined
//!   statements (`load-<id>.jsonl`, written as the load goes and named so when it commits)
//!   and one log of resolutions (`resolutions.jsonl`). Both survive restarts; checkpoints
//!   don't touch them. Stores in memory keep them in memory.
//! - **Cost.** Nothing without errors: the directory and a load's file come with its first
//!   bad statement, and every part of this runs on the error path only.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use nrese_rdf::{GraphName, NamedNode, Term};
use nrese_rdf_io::{RdfFormat, RdfSyntaxError};
use serde::{Deserialize, Serialize};

use crate::error::{StoreError, StoreResult};
use crate::mutation::{
    MutationCommand, MutationCommitReport, MutationError, MutationKind, MutationPipeline,
    MutationTicket,
};
use crate::query::GraphResultFormat;
use crate::statements::{RdfPayload, StatementOp, StatementsRequest};

const DIR: &str = "quarantine";
const RESOLUTIONS: &str = "resolutions.jsonl";
/// A load's statements until it commits: a file with this extension at a start is from a
/// load that didn't.
const PARTIAL: &str = "partial";
/// Quarantined statements a load report names (the first in its files' order).
pub const EXAMPLES: usize = 5;
/// The longest text kept of one statement; a longer one is kept truncated, and marked so.
const MAX_TEXT: u64 = 1 << 20;
/// Quarantined statements logged one by one (the rest are only counted).
const LOGGED: u64 = 20;

/// What a bulk load does with a statement it can't read (`store.invalid_statements`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InvalidStatements {
    /// Load every valid statement, keep each invalid one in the repository's quarantine.
    #[default]
    Quarantine,
    /// Stop at the first: nothing of the load is committed.
    Strict,
}

impl InvalidStatements {
    pub fn name(self) -> &'static str {
        match self {
            Self::Quarantine => "quarantine",
            Self::Strict => "strict",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "quarantine" => Some(Self::Quarantine),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// A statement a bulk load could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantinedStatement {
    /// Unique in the repository: the load, the file's place in it and the statement's byte
    /// offset in the file.
    pub id: String,
    pub load: String,
    pub file: String,
    /// Where it starts in the document, from 1; the column counts characters.
    pub line: u64,
    pub column: u64,
    pub reason: String,
    /// The file's format, as its extension (`nt`, `ttl`, …).
    pub format: String,
    /// The statement verbatim (lossy where it isn't UTF-8, see `not_utf8`); none for
    /// RDF/XML and JSON-LD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The text is the first [`MAX_TEXT`] bytes of a longer statement.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The statement's bytes aren't UTF-8: `text` has replacement characters for them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub not_utf8: bool,
    /// The directives in effect (Turtle, TriG), as `@base` and `@prefix` lines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    /// Its TriG block's graph, `<iri>` or `_:label`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    /// The load's graph for a triple format, `<iri>`; none for the default graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The base IRI the file was read with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The prefix the load gave its blank-node labels: what the statement's labels are in
    /// the store.
    pub blank_nodes: String,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// How a quarantined statement was resolved: its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub id: String,
    #[serde(flatten)]
    pub outcome: Outcome,
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// The repairs applied on request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<Repair>,
    /// The statement admitted, where it isn't the quarantined text (fixed or repaired).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Admitted in the commit of `revision`.
    Readmitted {
        revision: u64,
    },
    Discarded,
}

/// A repair of a statement's text, applied only on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Repair {
    /// Inside IRIs (`<…>`): a `%` that starts no escape becomes `%25`, and a character
    /// IRIs can't hold (space, `"`, `{`, `}`, `|`, `^`, `` ` ``, controls) its
    /// percent-encoded UTF-8. Strings and comments stay as they are.
    PercentEncodeIris,
}

impl Repair {
    pub fn name(self) -> &'static str {
        match self {
            Self::PercentEncodeIris => "percent-encode-iris",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "percent-encode-iris" => Some(Self::PercentEncodeIris),
            _ => None,
        }
    }

    fn apply(self, text: &str) -> String {
        match self {
            Self::PercentEncodeIris => percent_encode_iris(text),
        }
    }
}

/// Which quarantined statements a listing or an export shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarantineFilter {
    /// Of one load only.
    pub load: Option<String>,
    pub state: EntryState,
    /// Skipped first, then at most `limit`.
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntryState {
    /// Not resolved yet.
    #[default]
    Open,
    Resolved,
    All,
}

/// A quarantined statement and, once resolved, how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantineItem {
    #[serde(flatten)]
    pub statement: QuarantinedStatement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// What re-admitting a quarantined statement takes besides its id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readmission {
    /// The statement to admit in place of the quarantined text (a fix); its directives,
    /// graph and base still apply. Needed where no text was kept (RDF/XML, JSON-LD).
    pub text: Option<String>,
    /// Applied to the text, in order.
    pub repairs: Vec<Repair>,
    /// A statement with blank nodes gets fresh ones. The mutation pipeline's payloads
    /// can't name the load's blank nodes yet: without this, such a statement is refused.
    pub fresh_blank_nodes: bool,
    pub note: Option<String>,
}

/// A repository's quarantine.
#[derive(Debug)]
pub struct Quarantine {
    /// `quarantine/` of the data directory; `None` in memory.
    dir: Option<PathBuf>,
    state: Mutex<State>,
    /// Held by a resolution from its check to its record: one per statement.
    resolving: Mutex<()>,
}

#[derive(Debug, Default)]
struct State {
    /// The statements, in a store in memory (on disk they are read from the files).
    held: Vec<QuarantinedStatement>,
    resolutions: HashMap<String, Resolution>,
    /// Statements quarantined, resolved or not.
    total: u64,
}

impl Quarantine {
    /// The quarantine of a store whose data directory is `dir` (`None`: in memory).
    pub(crate) fn open(dir: Option<&Path>) -> StoreResult<Self> {
        let dir = dir.map(|dir| dir.join(DIR));
        let mut state = State::default();
        if let Some(dir) = dir.as_ref().filter(|dir| dir.exists()) {
            for path in load_files(dir, PARTIAL)? {
                tracing::warn!(file = %path.display(), "dropped the quarantine of a load that didn't commit");
                fs::remove_file(&path)?;
            }
            for path in load_files(dir, "jsonl")? {
                state.total += BufReader::new(File::open(&path)?)
                    .lines()
                    .filter(|line| line.as_ref().is_ok_and(|line| !line.is_empty()))
                    .count() as u64;
            }
            let resolutions = dir.join(RESOLUTIONS);
            if resolutions.exists() {
                for line in BufReader::new(File::open(&resolutions)?).lines() {
                    let line = line?;
                    match serde_json::from_str::<Resolution>(&line) {
                        Ok(resolution) => {
                            state.resolutions.insert(resolution.id.clone(), resolution);
                        }
                        // A record torn by a crash: its resolution didn't happen.
                        Err(_) if !line.is_empty() => {
                            tracing::warn!(record = %line, "skipped an unreadable quarantine resolution");
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        Ok(Self {
            dir,
            state: Mutex::new(state),
            resolving: Mutex::new(()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Quarantined statements not resolved yet.
    pub fn open_count(&self) -> u64 {
        let state = self.state();
        state.total - state.resolutions.len() as u64
    }

    /// Calls `f` with every quarantined statement and its resolution in the order they
    /// were quarantined, until it returns `false`.
    fn each(&self, mut f: impl FnMut(QuarantineItem) -> bool) -> StoreResult<()> {
        let resolution = |id: &str| self.state().resolutions.get(id).cloned();
        let Some(dir) = self.dir.as_ref().filter(|dir| dir.exists()) else {
            let held = self.state().held.clone();
            for statement in held {
                let resolution = resolution(&statement.id);
                if !f(QuarantineItem {
                    statement,
                    resolution,
                }) {
                    break;
                }
            }
            return Ok(());
        };
        for path in load_files(dir, "jsonl")? {
            for line in BufReader::new(File::open(&path)?).lines() {
                let line = line?;
                if line.is_empty() {
                    continue;
                }
                let statement: QuarantinedStatement =
                    serde_json::from_str(&line).map_err(|error| {
                        StoreError::Configuration(format!(
                            "unreadable quarantine record in {}: {error}",
                            path.display()
                        ))
                    })?;
                let resolution = resolution(&statement.id);
                if !f(QuarantineItem {
                    statement,
                    resolution,
                }) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Calls `f` with the statements `filter` selects, in order, until it returns `false`.
    fn selected(
        &self,
        filter: &QuarantineFilter,
        mut f: impl FnMut(QuarantineItem) -> bool,
    ) -> StoreResult<()> {
        let mut skip = filter.offset;
        let mut left = filter.limit.unwrap_or(usize::MAX);
        if left == 0 {
            return Ok(());
        }
        self.each(|item| {
            let state = match item.resolution {
                None => EntryState::Open,
                Some(_) => EntryState::Resolved,
            };
            let wanted = filter
                .load
                .as_ref()
                .is_none_or(|load| *load == item.statement.load)
                && (filter.state == EntryState::All || filter.state == state);
            if !wanted {
                return true;
            }
            if skip > 0 {
                skip -= 1;
                return true;
            }
            left -= 1;
            f(item) && left > 0
        })
    }

    /// The statements `filter` selects.
    pub fn list(&self, filter: &QuarantineFilter) -> StoreResult<Vec<QuarantineItem>> {
        let mut items = Vec::new();
        self.selected(filter, |item| {
            items.push(item);
            true
        })?;
        Ok(items)
    }

    /// The statement `id`, resolved or not.
    pub fn get(&self, id: &str) -> StoreResult<Option<QuarantineItem>> {
        let mut found = None;
        self.each(|item| {
            if item.statement.id == id {
                found = Some(item);
                return false;
            }
            true
        })?;
        Ok(found)
    }

    /// The statements `filter` selects written to `out` as JSON lines, as they are read.
    /// Returns how many.
    pub fn export(&self, filter: &QuarantineFilter, mut out: impl Write) -> StoreResult<u64> {
        let mut written = 0;
        let mut failed = None;
        self.selected(filter, |item| {
            let line = serde_json::to_string(&item).expect("a quarantine item serialises");
            match writeln!(out, "{line}") {
                Ok(()) => {
                    written += 1;
                    true
                }
                Err(error) => {
                    failed = Some(error);
                    false
                }
            }
        })?;
        match failed {
            Some(error) => Err(error.into()),
            None => Ok(written),
        }
    }

    /// Discards the open statements `ids`: they stay listed as resolved, and are never
    /// admitted. All or none: an id that isn't open is an error, and nothing is discarded.
    pub fn discard(&self, ids: &[String], note: Option<String>) -> StoreResult<u64> {
        let _one = self
            .resolving
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for id in ids {
            self.open_statement(id)?;
        }
        let at = now();
        for id in ids {
            self.record(Resolution {
                id: id.clone(),
                outcome: Outcome::Discarded,
                at,
                repairs: Vec::new(),
                text: None,
                note: note.clone(),
            })?;
        }
        Ok(ids.len() as u64)
    }

    /// The open statement `id`.
    fn open_statement(&self, id: &str) -> StoreResult<QuarantinedStatement> {
        match self.get(id)? {
            Some(QuarantineItem {
                statement,
                resolution: None,
            }) => Ok(statement),
            Some(_) => Err(StoreError::Quarantine(format!(
                "quarantined statement {id} is already resolved"
            ))),
            None => Err(StoreError::Quarantine(format!(
                "no quarantined statement {id}"
            ))),
        }
    }

    /// Records `resolution`: appended to the log, then in effect.
    fn record(&self, resolution: Resolution) -> StoreResult<()> {
        if let Some(dir) = &self.dir {
            fs::create_dir_all(dir)?;
            let mut log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(RESOLUTIONS))?;
            let line = serde_json::to_string(&resolution).expect("a resolution serialises");
            log.write_all(format!("{line}\n").as_bytes())?;
            log.sync_data()?;
        }
        self.state()
            .resolutions
            .insert(resolution.id.clone(), resolution);
        Ok(())
    }

    /// The write that admits the open statement `id` as `request` asks, and the text it
    /// admits (`None`: the quarantined text). The text must read now: an error otherwise.
    fn readmission(
        &self,
        id: &str,
        request: &Readmission,
    ) -> StoreResult<(MutationCommand, Option<String>)> {
        let statement = self.open_statement(id)?;
        let format = RdfFormat::from_extension(&statement.format)
            .and_then(payload_format)
            .ok_or_else(|| {
                StoreError::Quarantine(format!(
                    "quarantined statement {id}: format {} can't be admitted",
                    statement.format
                ))
            })?;
        let kept = match (&request.text, &statement.text) {
            (Some(text), _) => text.clone(),
            (None, Some(text)) if !statement.truncated && !statement.not_utf8 => text.clone(),
            (None, _) => {
                return Err(StoreError::Quarantine(format!(
                    "quarantined statement {id}: its text wasn't kept whole; give the text to admit"
                )));
            }
        };
        let text = request
            .repairs
            .iter()
            .fold(kept, |text, repair| repair.apply(&text));
        let body = match &statement.graph {
            Some(graph) => format!("{graph} {{\n{text}\n}}\n"),
            None => format!("{text}\n"),
        };
        let document = format!("{}{body}", statement.context);
        let quads = crate::rdf_io::parse_payload_preserving_blank_nodes(
            format,
            statement.base.as_deref(),
            document.as_bytes(),
        )?;
        let has_blank_nodes = quads.iter().any(|quad| {
            has_blank_node(&quad.subject.clone().into())
                || has_blank_node(&quad.object)
                || matches!(quad.graph_name, GraphName::BlankNode(_))
        });
        if has_blank_nodes && !request.fresh_blank_nodes {
            return Err(StoreError::Quarantine(format!(
                "quarantined statement {id} has blank nodes, which a write can't name yet: \
                 admit it with fresh ones, or fix it"
            )));
        }
        let contexts = match &statement.target {
            Some(graph) => vec![GraphName::NamedNode(
                NamedNode::new(graph.trim_start_matches('<').trim_end_matches('>'))
                    .map_err(|error| StoreError::InvalidGraphIri(error.to_string()))?,
            )],
            None => Vec::new(),
        };
        let command = MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
            data: RdfPayload {
                payload: document.into_bytes(),
                format,
                base_iri: statement.base.clone(),
            },
            contexts,
        }]));
        let admitted = (statement.text.as_deref() != Some(text.as_str())).then_some(text);
        Ok((command, admitted))
    }

    /// Starts the quarantine of a load (`load`'s statements, its blank-node labels prefixed
    /// with `blank_nodes`, its triple formats read into `target`).
    pub(crate) fn begin(
        &self,
        files: Vec<LoadFile>,
        target: &GraphName,
        blank_nodes: String,
    ) -> Collector<'_> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let load = format!("{millis}-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
        Collector {
            quarantine: self,
            partial: self
                .dir
                .as_ref()
                .map(|dir| dir.join(format!("load-{load}.{PARTIAL}"))),
            load,
            files,
            target: match target {
                GraphName::NamedNode(graph) => Some(graph.to_string()),
                _ => None,
            },
            blank_nodes,
            count: AtomicU64::new(0),
            inner: Mutex::default(),
        }
    }
}

/// The files `load-*.<extension>` in `dir`, in the order their loads ran.
fn load_files(dir: &Path, extension: &str) -> StoreResult<Vec<PathBuf>> {
    let mut files: Vec<(u128, u64, PathBuf)> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.strip_prefix("load-")?;
            let (millis, sequence) = stem.split_once('-')?;
            Some((millis.parse().ok()?, sequence.parse().ok()?, path))
        })
        .collect();
    files.sort();
    Ok(files.into_iter().map(|(_, _, path)| path).collect())
}

fn has_blank_node(term: &Term) -> bool {
    match term {
        Term::BlankNode(_) => true,
        Term::Triple(triple) => {
            has_blank_node(&triple.subject.clone().into()) || has_blank_node(&triple.object)
        }
        _ => false,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The payload format a statement of `format` is admitted in.
fn payload_format(format: RdfFormat) -> Option<GraphResultFormat> {
    Some(match format {
        RdfFormat::NTriples => GraphResultFormat::NTriples,
        RdfFormat::NQuads => GraphResultFormat::NQuads,
        RdfFormat::Turtle => GraphResultFormat::Turtle,
        RdfFormat::TriG => GraphResultFormat::TriG,
        RdfFormat::RdfXml => GraphResultFormat::RdfXml,
        RdfFormat::JsonLd => GraphResultFormat::JsonLd,
        _ => return None,
    })
}

/// Admits the open quarantined statement `id` as `request` asks, through `pipeline` for
/// `requester`: its gates decide as for any write. Recorded as resolved once committed;
/// a refused or still-invalid statement stays open.
pub fn readmit(
    pipeline: &MutationPipeline,
    requester: &crate::Requester,
    ticket: &MutationTicket,
    id: &str,
    request: &Readmission,
) -> Result<Resolution, MutationError> {
    let quarantine = pipeline.store().quarantine();
    let _one = quarantine
        .resolving
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let store_error = |source| MutationError::Store {
        kind: MutationKind::Statements,
        source,
    };
    let (command, text) = quarantine.readmission(id, request).map_err(store_error)?;
    let revision = match pipeline.apply(command, requester, ticket)? {
        MutationCommitReport::Applied { revision } => revision,
        other => unreachable!("statements produced {other:?}"),
    };
    let resolution = Resolution {
        id: id.to_owned(),
        outcome: Outcome::Readmitted { revision },
        at: now(),
        repairs: request.repairs.clone(),
        text,
        note: request.note.clone(),
    };
    quarantine.record(resolution.clone()).map_err(store_error)?;
    Ok(resolution)
}

/// A file of a load, as its quarantine records it.
pub(crate) struct LoadFile {
    pub path: PathBuf,
    pub format: RdfFormat,
    pub base: Option<String>,
}

/// The quarantine of one load: its statements go to a file named for the load until it
/// commits ([`Self::commit`]); dropped before, the load leaves nothing.
pub(crate) struct Collector<'q> {
    quarantine: &'q Quarantine,
    load: String,
    /// The load's file until it commits (on disk).
    partial: Option<PathBuf>,
    files: Vec<LoadFile>,
    target: Option<String>,
    blank_nodes: String,
    count: AtomicU64,
    inner: Mutex<Collecting>,
}

#[derive(Default)]
struct Collecting {
    /// The load's file, from its first statement (on disk).
    out: Option<BufWriter<File>>,
    /// The statements (in memory).
    held: Vec<QuarantinedStatement>,
    /// The load's files, open for the text of their statements.
    sources: HashMap<usize, File>,
    /// The first statements in the files' order, by file and offset.
    examples: Vec<((usize, u64), QuarantinedStatement)>,
}

/// What a load quarantined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quarantined {
    pub load: Option<String>,
    pub count: u64,
    pub examples: Vec<QuarantinedStatement>,
}

impl Collector<'_> {
    /// Quarantines the statement of `error` in the load's file `file`.
    pub(crate) fn add(&self, file: usize, error: &RdfSyntaxError) -> StoreResult<()> {
        let source = &self.files[file];
        let skipped = error.statement();
        let start = skipped.map_or(error.location().start, |s| s.span.start);
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        if n < LOGGED {
            tracing::warn!(file = %source.path.display(), %error, "quarantined a statement");
        } else if n == LOGGED {
            tracing::warn!(file = %source.path.display(), "more statements quarantined; only counted from here");
        }
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (text, truncated, not_utf8) = match skipped {
            None => (None, false, false),
            Some(skipped) => {
                let reader = match inner.sources.entry(file) {
                    std::collections::hash_map::Entry::Occupied(open) => open.into_mut(),
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(File::open(&source.path)?)
                    }
                };
                let length = skipped.span.end.offset.saturating_sub(start.offset);
                let mut bytes = Vec::new();
                reader.seek(SeekFrom::Start(start.offset))?;
                reader.take(length.min(MAX_TEXT)).read_to_end(&mut bytes)?;
                let (text, not_utf8) = match String::from_utf8(bytes) {
                    Ok(text) => (text, false),
                    Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), true),
                };
                (Some(text), length > MAX_TEXT, not_utf8)
            }
        };
        let statement = QuarantinedStatement {
            id: format!("{}.{file}.{}", self.load, start.offset),
            load: self.load.clone(),
            file: source.path.display().to_string(),
            line: start.line + 1,
            column: start.column + 1,
            reason: error.message().to_owned(),
            format: source.format.file_extension().to_owned(),
            text,
            truncated,
            not_utf8,
            context: skipped.map(|s| s.context.clone()).unwrap_or_default(),
            graph: skipped.and_then(|s| s.graph.clone()),
            target: (!source.format.supports_datasets())
                .then(|| self.target.clone())
                .flatten(),
            base: source.base.clone(),
            blank_nodes: self.blank_nodes.clone(),
        };
        let key = (file, start.offset);
        let at = inner.examples.partition_point(|(other, _)| *other < key);
        if at < EXAMPLES {
            inner.examples.insert(at, (key, statement.clone()));
            inner.examples.truncate(EXAMPLES);
        }
        match &self.partial {
            Some(partial) => {
                if inner.out.is_none() {
                    fs::create_dir_all(partial.parent().expect("in the quarantine directory"))?;
                    inner.out = Some(BufWriter::new(File::create(partial)?));
                }
                let out = inner.out.as_mut().expect("opened");
                serde_json::to_writer(&mut *out, &statement).map_err(io::Error::from)?;
                out.write_all(b"\n")?;
            }
            None => inner.held.push(statement),
        }
        Ok(())
    }

    /// The load committed: its statements join the quarantine. Returns what it quarantined.
    pub(crate) fn commit(self) -> StoreResult<Quarantined> {
        let count = self.count.load(Ordering::Relaxed);
        let mut inner =
            std::mem::take(&mut *self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        if count == 0 {
            return Ok(Quarantined::default());
        }
        if let (Some(out), Some(partial)) = (inner.out.take(), &self.partial) {
            let file = out.into_inner().map_err(|error| error.into_error())?;
            file.sync_data()?;
            drop(file);
            fs::rename(partial, partial.with_extension("jsonl"))?;
        }
        let mut state = self.quarantine.state();
        state.held.append(&mut inner.held);
        state.total += count;
        drop(state);
        Ok(Quarantined {
            load: Some(self.load.clone()),
            count,
            examples: inner.examples.drain(..).map(|(_, s)| s).collect(),
        })
    }
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        // Committed, the file was renamed; otherwise the load left nothing.
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        drop(inner.out.take());
        if let Some(partial) = &self.partial
            && partial.exists()
        {
            let _ = fs::remove_file(partial);
        }
    }
}

/// [`Repair::PercentEncodeIris`].
fn percent_encode_iris(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let taken = match c {
            // A comment, to the end of its line.
            '#' => rest.find('\n').unwrap_or(rest.len()),
            '"' | '\'' => string_end(rest),
            // A triple term's brackets.
            '<' if rest.starts_with("<<") => 2,
            '<' => {
                let end = rest[1..].find('>').map_or(rest.len(), |at| at + 2);
                encode_iri(&rest[..end], &mut out);
                rest = &rest[end..];
                continue;
            }
            c => c.len_utf8(),
        };
        out.push_str(&rest[..taken]);
        rest = &rest[taken..];
    }
    out
}

/// The length of the string literal `rest` starts with (to the end of the line if it
/// isn't closed there, or of the text if a long one isn't).
fn string_end(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let quote = bytes[0];
    let long = bytes.len() >= 3 && bytes[1] == quote && bytes[2] == quote;
    let close: &[u8] = match long {
        true => &bytes[..3],
        false => &bytes[..1],
    };
    let mut at = close.len();
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            at += 2;
            continue;
        }
        if bytes[at..].starts_with(close) {
            return at + close.len();
        }
        if !long && bytes[at] == b'\n' {
            return at;
        }
        at += 1;
    }
    bytes.len()
}

/// `iri` (`<…>`, or `<…` unclosed) to `out` with what IRIs can't hold percent-encoded.
fn encode_iri(iri: &str, out: &mut String) {
    let bytes = iri.as_bytes();
    for (at, c) in iri.char_indices() {
        let first = at == 0;
        let last = at + 1 == bytes.len() && c == '>';
        match c {
            '<' if first => out.push(c),
            '>' if last => out.push(c),
            '%' if bytes.len() > at + 2
                && bytes[at + 1].is_ascii_hexdigit()
                && bytes[at + 2].is_ascii_hexdigit() =>
            {
                out.push(c)
            }
            '%' | ' ' | '"' | '{' | '}' | '|' | '^' | '`' | '<' | '>' => percent_encode(c, out),
            c if c.is_control() => percent_encode(c, out),
            c => out.push(c),
        }
    }
}

fn percent_encode(c: char, out: &mut String) {
    let mut buffer = [0; 4];
    for byte in c.encode_utf8(&mut buffer).bytes() {
        out.push_str(&format!("%{byte:02X}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_encode_inside_iris_only() {
        let repaired = percent_encode_iris(
            "<http://e.com/x?g=%&a=%41 b> <http://e.com/p> \"100% <not an iri>\" . # 50% <c>\n",
        );
        assert_eq!(
            repaired,
            "<http://e.com/x?g=%25&a=%41%20b> <http://e.com/p> \"100% <not an iri>\" . # 50% <c>\n"
        );
        // Triple terms, long strings with quotes, escapes.
        let repaired =
            percent_encode_iris("<< <a%> <p> <o> >> <q> \"\"\"a \"b\" <%>\"\"\" , 'it\\'s <%>' .");
        assert_eq!(
            repaired,
            "<< <a%25> <p> <o> >> <q> \"\"\"a \"b\" <%>\"\"\" , 'it\\'s <%>' ."
        );
    }

    #[test]
    fn settings_and_repairs_read_their_names() {
        for mode in [InvalidStatements::Quarantine, InvalidStatements::Strict] {
            assert_eq!(InvalidStatements::from_name(mode.name()), Some(mode));
        }
        assert_eq!(InvalidStatements::default(), InvalidStatements::Quarantine);
        let repair = Repair::PercentEncodeIris;
        assert_eq!(Repair::from_name(repair.name()), Some(repair));
        assert_eq!(
            serde_json::to_string(&repair).unwrap(),
            format!("\"{}\"", repair.name())
        );
    }
}
