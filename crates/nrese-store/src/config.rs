use std::path::PathBuf;

use crate::error::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreMode {
    #[default]
    InMemory,
    OnDisk,
}

/// Typed store configuration. Parsing from env/files lives in `nrese-server/src/config/`.
/// Support graph sets kept per inferred statement by default ([`StoreConfig::support_sets`]).
pub const DEFAULT_SUPPORT_SETS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub mode: StoreMode,
    pub data_dir: PathBuf,
    /// Ontology file loaded at startup. `None` means no preload; there are no implicit
    /// fallback locations.
    pub ontology_path: Option<PathBuf>,
    /// Bytes of query part results kept for repeated queries and shared sub-patterns on an
    /// unchanged store (`nrese_sparql::cache`); 0 disables the cache.
    pub query_cache_bytes: usize,
    /// SPARQL query files whose results are pinned in the result cache at startup, each
    /// under its file name without the extension.
    pub pinned_queries: Vec<PathBuf>,
    /// The graph that holds the repository's SHACL shapes.
    pub shapes_graph: String,
    /// The default graph of queries and update `WHERE` clauses that name no dataset is
    /// the merge of all graphs, not only the default graph.
    pub union_default_graph: bool,
    /// GeoSPARQL relations in triple patterns read only the statements that assert them,
    /// without the query-rewrite extension computing them from geometries.
    pub geosparql_stated_only: bool,
    /// Bytes of intermediate results all running queries may hold together; 0 is
    /// unlimited. A query that asks for more than is left fails with
    /// [`StoreError::is_server_memory_limit`](crate::StoreError::is_server_memory_limit).
    pub total_query_memory_bytes: usize,
    /// The memory the process may hold before a long operation (a materialisation, the
    /// reasoning of a commit) stops with [`StoreError::ProcessMemoryLimit`](crate::StoreError::ProcessMemoryLimit)
    /// instead of taking the machine; 0 is no limit. By default three quarters of what the
    /// process may use ([`default_process_memory_bytes`]).
    pub process_memory_bytes: u64,
    /// Whether the process's memory may be backed by transparent huge pages (Linux;
    /// `nrese_exec::memory::set_transparent_huge_pages`): faster work over large heaps
    /// (materialisation, loads), more memory held while serving many small requests.
    pub huge_pages: bool,
    /// Which remote endpoints `SERVICE` may call.
    pub federation: FederationConfig,
    /// Reasoning leaves out memberships in unnamed union classes that nothing consumes
    /// (`reasoner.unnamed_classes = "skip"`; work package W7).
    pub hide_unnamed_classes: bool,
    /// OWL 2 QL answers through existentials by tree-witness rewriting
    /// (`reasoner.ql_rewriting`, docs/design/ql-rewriting.md §4); a repository's settings
    /// may choose another.
    pub ql_rewriting: QlRewritingMode,
    /// Full materialisations with equality rules compute the closure over representatives
    /// of the `owl:sameAs` classes and expand it (`reasoner.equality = "representatives"`,
    /// the default) instead of copying every fact to every identity while computing it
    /// (`"replicate"`). The same closure either way.
    pub equality_by_representatives: bool,
    /// With [`Self::equality_by_representatives`]: the inferred stack keeps the closure
    /// over representatives, and reads expand it to every identity
    /// (`reasoner.equality = "compact"`; W4 stage B): storage and commits shrink by the
    /// replication factor, reads of facts about identities pay the expansion.
    pub equality_compact: bool,
    /// With [`Self::equality_compact`]: queries answer with one identity per class, its
    /// representative (`equality.answers = "canonical"`, stage C): for analytics, where
    /// the identities are one thing. Off (`"strict"`): every identity, as if replicated.
    pub equality_canonical_answers: bool,
    /// With [`Self::equality_compact`]: queries expand the classes at every read (stage
    /// B, `equality.expansion = "early"`) instead of after the joins where the answer
    /// can't tell (stage C, `"late"`, the default). The same answers.
    pub equality_early_expansion: bool,
    /// Support graph sets kept per inferred statement for `inferred = "supported"` in the
    /// access policy (`reasoner.support_sets`, at least 1): the smallest first. A set left
    /// out can only hide a statement from a reader who could have seen it.
    pub support_sets: usize,
    /// On disk: check the whole checkpoint when opening (see
    /// `nrese_engine::DurabilityConfig::verify_on_open`). Off: the checkpoint is used in
    /// place and opening reads only its structure.
    pub verify_on_open: bool,
    /// On disk: serve the data from each checkpoint once written, freeing its copies in
    /// memory (see `nrese_engine::DurabilityConfig::map_checkpoints`).
    pub map_checkpoints: bool,
    /// On disk: a checkpoint is written in the background once the write-ahead log has
    /// grown by this many bytes since the last (see
    /// `nrese_engine::DurabilityConfig::checkpoint_after_wal_bytes`): what a restart
    /// replays, against the work of writing checkpoints.
    pub checkpoint_after_wal_bytes: u64,
    /// On disk: memory for a bulk load's quads, in bytes (`0`: no limit); past it they are
    /// sorted in chunks spilled to the data directory (see
    /// `nrese_engine::DurabilityConfig::bulk_load_memory`).
    pub bulk_load_memory_bytes: u64,
    /// On disk: keep the WAL segments checkpoints cover in `wal-archive/` of the data
    /// directory (point-in-time restore; see `nrese_engine::DurabilityConfig::wal_archive`).
    pub wal_archive: bool,
    /// How index blocks are encoded when built (`store.index_encoding`): `Fast`, or
    /// `Compact` (palettes: a smaller store, scans up to a tenth slower). Set for the
    /// process when the store opens; see `nrese_engine::IndexEncoding`.
    pub index_encoding: nrese_engine::IndexEncoding,
    /// How checkpoints store the dictionary's keys (`store.vocabulary`): `Plain`, or `Fsst`
    /// (compressed to about half; a key costs a decode when read).
    pub vocabulary: nrese_engine::VocabularyEncoding,
    /// SHACL as a commit gate (design `docs/design/shacl.md` §7): what a commit's changes
    /// may introduce against the shapes graph.
    pub shacl_gate: ShaclGate,
    /// The `owl2-dl` mode's settings (`dl.*`), used when the reasoner runs that mode.
    pub dl: crate::dl::DlConfig,
}

/// When queries get the OWL 2 QL answers through existentials (`reasoner.ql_rewriting`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QlRewritingMode {
    /// Under `owl2-ql`, the profile that names them; not under `owl2-rl`, whose users chose
    /// its standard semantics (and answer counts equal to other systems' `owl2-rl`).
    #[default]
    Auto,
    /// Under `owl2-ql` and `owl2-rl`.
    On,
    Off,
}

impl QlRewritingMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// The mode named `name` (`auto`, `on`, `off`).
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Auto, Self::On, Self::Off]
            .into_iter()
            .find(|mode| mode.name() == name)
    }

    /// Whether queries over a closure of `ruleset` are rewritten.
    pub fn applies_to(self, ruleset: &str) -> bool {
        match (self, ruleset) {
            (Self::Off, _) => false,
            (_, "owl2-ql") => true,
            (Self::On, "owl2-rl") => true,
            _ => false,
        }
    }
}

/// The SHACL commit gate's policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShaclGate {
    /// Commits aren't validated (the default: validation is a request, as in GraphDB
    /// without a SHACL repository).
    #[default]
    Off,
    /// Commits are validated and what they introduce is logged; nothing is rejected.
    Report,
    /// A commit that introduces a result at or above this severity is rejected and
    /// changes nothing.
    Enforce(GateSeverity),
}

/// The least severity the enforcing gate rejects for. A shape's own severity IRI counts
/// as a violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GateSeverity {
    Info,
    Warning,
    Violation,
}

/// SPARQL 1.1 Federated Query: the endpoints `SERVICE` may call, and how long and how much
/// it may read from them. Off (no endpoint allowed) by default: a query could otherwise
/// make the server fetch any URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederationConfig {
    /// Endpoint IRIs, or prefixes of them, `SERVICE` may call; `*` allows any.
    pub allow: Vec<String>,
    /// Per request to an endpoint.
    pub timeout_ms: u64,
    /// Rows one request may return.
    pub max_rows: usize,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            timeout_ms: 30_000,
            max_rows: 1_000_000,
        }
    }
}

impl FederationConfig {
    pub fn enabled(&self) -> bool {
        !self.allow.is_empty()
    }

    /// Whether `endpoint` is allowed.
    pub fn allows(&self, endpoint: &str) -> bool {
        self.allow
            .iter()
            .any(|allowed| allowed == "*" || endpoint.starts_with(allowed.as_str()))
    }
}

/// Whether transparent huge pages may back the process's memory by default: yes, as
/// before the setting (LUBM 100 materialises 26 % faster with them; serving holds less
/// without them: docs/ops/server-setup.md §12.1).
pub const DEFAULT_HUGE_PAGES: bool = true;

/// The write-ahead log's growth after which a checkpoint is written: 256 MiB.
pub const DEFAULT_CHECKPOINT_AFTER_WAL_BYTES: u64 = 256 << 20;

/// Default query result cache: 64 MiB.
pub const DEFAULT_QUERY_CACHE_BYTES: usize = 64 << 20;

/// The default shapes graph: the one RDF4J and GraphDB use, so their clients and
/// documentation apply unchanged.
pub const DEFAULT_SHAPES_GRAPH: &str = "http://rdf4j.org/schema/rdf4j#SHACLShapeGraph";

impl Default for StoreConfig {
    fn default() -> Self {
        Self::in_memory()
    }
}

/// Three quarters of the memory the process may use (the container's limit, else the
/// machine's: [`nrese_exec::memory::available_bytes`]); 0 (no limit) where unknown.
pub fn default_process_memory_bytes() -> u64 {
    nrese_exec::memory::available_bytes().map_or(0, |bytes| bytes / 4 * 3)
}

impl StoreConfig {
    pub fn in_memory() -> Self {
        Self {
            mode: StoreMode::InMemory,
            data_dir: PathBuf::from("./data"),
            ontology_path: None,
            query_cache_bytes: DEFAULT_QUERY_CACHE_BYTES,
            pinned_queries: Vec::new(),
            shapes_graph: DEFAULT_SHAPES_GRAPH.to_owned(),
            union_default_graph: false,
            geosparql_stated_only: false,
            total_query_memory_bytes: 0,
            process_memory_bytes: default_process_memory_bytes(),
            huge_pages: DEFAULT_HUGE_PAGES,
            federation: FederationConfig::default(),
            hide_unnamed_classes: false,
            ql_rewriting: QlRewritingMode::Auto,
            equality_by_representatives: true,
            equality_compact: false,
            equality_canonical_answers: false,
            equality_early_expansion: false,
            support_sets: DEFAULT_SUPPORT_SETS,
            verify_on_open: false,
            map_checkpoints: true,
            checkpoint_after_wal_bytes: DEFAULT_CHECKPOINT_AFTER_WAL_BYTES,
            bulk_load_memory_bytes: 0,
            wal_archive: false,
            index_encoding: nrese_engine::IndexEncoding::Fast,
            vocabulary: nrese_engine::VocabularyEncoding::Plain,
            shacl_gate: ShaclGate::Off,
            dl: crate::dl::DlConfig::default(),
        }
    }

    pub fn on_disk(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            mode: StoreMode::OnDisk,
            data_dir: data_dir.into(),
            ontology_path: None,
            query_cache_bytes: DEFAULT_QUERY_CACHE_BYTES,
            pinned_queries: Vec::new(),
            shapes_graph: DEFAULT_SHAPES_GRAPH.to_owned(),
            union_default_graph: false,
            geosparql_stated_only: false,
            total_query_memory_bytes: 0,
            process_memory_bytes: default_process_memory_bytes(),
            huge_pages: DEFAULT_HUGE_PAGES,
            federation: FederationConfig::default(),
            hide_unnamed_classes: false,
            ql_rewriting: QlRewritingMode::Auto,
            equality_by_representatives: true,
            equality_compact: false,
            equality_canonical_answers: false,
            equality_early_expansion: false,
            support_sets: DEFAULT_SUPPORT_SETS,
            verify_on_open: false,
            map_checkpoints: true,
            checkpoint_after_wal_bytes: DEFAULT_CHECKPOINT_AFTER_WAL_BYTES,
            bulk_load_memory_bytes: 0,
            wal_archive: false,
            index_encoding: nrese_engine::IndexEncoding::Fast,
            vocabulary: nrese_engine::VocabularyEncoding::Plain,
            shacl_gate: ShaclGate::Off,
            dl: crate::dl::DlConfig::default(),
        }
    }

    #[must_use]
    pub fn with_ontology(mut self, path: impl Into<PathBuf>) -> Self {
        self.ontology_path = Some(path.into());
        self
    }

    pub fn validate(&self) -> StoreResult<()> {
        if self.equality_compact && !self.equality_by_representatives {
            return Err(crate::StoreError::Configuration(
                "compact equality storage computes over representatives; it needs \
                 equality_by_representatives"
                    .to_owned(),
            ));
        }
        if self.dl.max_candidates == 0 || self.dl.timeout.is_zero() {
            return Err(StoreError::Configuration(
                "dl.max_candidates and dl.timeout must be above 0".to_owned(),
            ));
        }
        if self.support_sets == 0 {
            return Err(StoreError::Configuration(
                "support_sets must be at least 1".to_owned(),
            ));
        }
        if matches!(self.mode, StoreMode::OnDisk) && self.data_dir.as_os_str().is_empty() {
            return Err(StoreError::Configuration(
                "data_dir must not be empty in on-disk mode".to_owned(),
            ));
        }
        if nrese_rdf::NamedNode::new(self.shapes_graph.as_str()).is_err() {
            return Err(StoreError::Configuration(format!(
                "shapes_graph must be an IRI, not '{}'",
                self.shapes_graph
            )));
        }
        if matches!(self.ontology_path.as_ref(), Some(path) if path.as_os_str().is_empty()) {
            return Err(StoreError::Configuration(
                "ontology_path must not be empty if set".to_owned(),
            ));
        }
        Ok(())
    }
}
