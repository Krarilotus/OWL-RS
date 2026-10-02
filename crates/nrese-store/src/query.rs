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
}

impl SparqlQueryRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            solutions_format: SolutionsResultFormat::Json,
            graph_format: GraphResultFormat::NTriples,
            default_graphs: Vec::new(),
            named_graphs: Vec::new(),
            read_model: None,
            memory_limit: None,
            as_written: false,
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
}
