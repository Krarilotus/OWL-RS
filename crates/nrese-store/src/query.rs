use oxigraph::io::RdfFormat;
use oxigraph::sparql::results::QueryResultsFormat;

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

    pub fn into_oxigraph(self) -> QueryResultsFormat {
        match self {
            Self::Json => QueryResultsFormat::Json,
            Self::Xml => QueryResultsFormat::Xml,
            Self::Csv => QueryResultsFormat::Csv,
            Self::Tsv => QueryResultsFormat::Tsv,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphResultFormat {
    NTriples,
    Turtle,
}

impl GraphResultFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::NTriples => "application/n-triples",
            Self::Turtle => "text/turtle",
        }
    }

    pub fn into_oxigraph(self) -> RdfFormat {
        match self {
            Self::NTriples => RdfFormat::NTriples,
            Self::Turtle => RdfFormat::Turtle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlQueryRequest {
    pub query: String,
    pub solutions_format: SolutionsResultFormat,
    pub graph_format: GraphResultFormat,
}

impl SparqlQueryRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            solutions_format: SolutionsResultFormat::Json,
            graph_format: GraphResultFormat::NTriples,
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
