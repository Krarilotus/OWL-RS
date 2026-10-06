#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolutionsResultFormat {
    Json,
    Xml,
    Csv,
    Tsv,
}

impl SolutionsResultFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Json => "application/sparql-results+json",
            Self::Xml => "application/sparql-results+xml",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
        }
    }

    /// [`Self::media_type`] announcing results that may use RDF 1.2, where the format
    /// defines how (JSON's `version` parameter; the others have none).
    pub fn media_type_rdf12(self) -> &'static str {
        match self {
            Self::Json => "application/sparql-results+json; version=1.2",
            other => other.media_type(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphResultFormat {
    NTriples,
    Turtle,
    RdfXml,
    /// One graph as N-Quads: its triples, without graph names.
    NQuads,
    /// One graph as TriG: its triples in the default graph block.
    TriG,
    JsonLd,
    /// RDF4J's Binary RDF (`application/x-binary-rdf`), with graph names where the payload
    /// has them (statements).
    BinaryRdf,
}

impl GraphResultFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::NTriples => "application/n-triples",
            Self::Turtle => "text/turtle",
            Self::RdfXml => "application/rdf+xml",
            Self::NQuads => "application/n-quads",
            Self::TriG => "application/trig",
            Self::JsonLd => "application/ld+json",
            Self::BinaryRdf => "application/x-binary-rdf",
        }
    }

    /// [`Self::media_type`] announcing an RDF 1.2 document (`version=1.2`) in the formats of
    /// the RDF 1.2 specifications.
    pub fn media_type_rdf12(self) -> &'static str {
        match self {
            Self::NTriples => "application/n-triples; version=1.2",
            Self::Turtle => "text/turtle; version=1.2",
            Self::NQuads => "application/n-quads; version=1.2",
            Self::TriG => "application/trig; version=1.2",
            other => other.media_type(),
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "ttl" => Some(Self::Turtle),
            "nt" => Some(Self::NTriples),
            "rdf" | "xml" => Some(Self::RdfXml),
            "nq" => Some(Self::NQuads),
            "trig" => Some(Self::TriG),
            "jsonld" => Some(Self::JsonLd),
            "brf" => Some(Self::BinaryRdf),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlQueryRequest {
    pub query: String,
    pub solutions_format: SolutionsResultFormat,
    pub graph_format: GraphResultFormat,
    /// Protocol `default-graph-uri` values. If this or `named_graphs` is non-empty, both
    /// replace the query's `FROM` / `FROM NAMED` clauses.
    pub default_graphs: Vec<String>,
    /// Protocol `named-graph-uri` values.
    pub named_graphs: Vec<String>,
    /// Which statements the query reads; `None` = asserted and inferred, unless the query
    /// names GraphDB's pseudo-graphs `onto:explicit` / `onto:implicit` in `FROM`.
    pub read_model: Option<nrese_engine::ReadModel>,
    /// Bytes of intermediate results the query may hold; `None` is unlimited. A query that
    /// needs more fails ([`StoreError::is_memory_limit`](crate::StoreError::is_memory_limit)).
    pub memory_limit: Option<usize>,
    /// Evaluate the operators where the query puts them (no filter pushdown, no set
    /// evaluation, paths in full): same results, for comparisons and as an escape hatch.
    pub as_written: bool,
    /// Whose read it is (graph-level access control): the query's dataset is restricted
    /// to the graphs the scope reads.
    pub scope: crate::ReadScope,
    /// Under `owl2-dl`: which answers this query asks for, over `dl.answers`.
    pub dl_answers: Option<crate::DlAnswers>,
}

impl SparqlQueryRequest {
    /// A query over every graph ([`crate::ReadScope::All`]): the server's own work, tests.
    pub fn all(query: impl Into<String>) -> Self {
        Self::new(query, crate::ReadScope::All)
    }

    pub fn new(query: impl Into<String>, scope: crate::ReadScope) -> Self {
        Self {
            query: query.into(),
            solutions_format: SolutionsResultFormat::Json,
            graph_format: GraphResultFormat::NTriples,
            default_graphs: Vec::new(),
            named_graphs: Vec::new(),
            read_model: None,
            memory_limit: None,
            as_written: false,
            scope,
            dl_answers: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryResultKind {
    Boolean,
    Solutions,
    Graph,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedQueryResult {
    pub kind: QueryResultKind,
    pub media_type: &'static str,
    pub payload: Vec<u8>,
    /// What the OWL 2 QL rewriting did and whether the answers are complete; `None` where
    /// it doesn't apply.
    pub ql: Option<nrese_sparql::ql::QlReport>,
    /// Whether the answers are complete, where a reasoning path can leave some out (the
    /// status [`crate::StoreService::run_query_reporting`] reports).
    pub completeness: Option<nrese_sparql::Completeness>,
}
