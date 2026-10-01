//! The four results formats, by name, file extension and media type.

use std::fmt;

/// A SPARQL query results format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryResultsFormat {
    /// [SPARQL Query Results XML Format](https://www.w3.org/TR/sparql12-results-xml/)
    Xml,
    /// [SPARQL Query Results JSON Format](https://www.w3.org/TR/sparql12-results-json/)
    Json,
    /// [SPARQL Query Results CSV Format](https://www.w3.org/TR/sparql12-results-csv-tsv/)
    Csv,
    /// [SPARQL Query Results TSV Format](https://www.w3.org/TR/sparql12-results-csv-tsv/)
    Tsv,
}

impl QueryResultsFormat {
    pub const ALL: [Self; 4] = [Self::Xml, Self::Json, Self::Csv, Self::Tsv];

    /// The format's unique IRI (from the W3C's list of formats).
    pub const fn iri(self) -> &'static str {
        match self {
            Self::Xml => "http://www.w3.org/ns/formats/SPARQL_Results_XML",
            Self::Json => "http://www.w3.org/ns/formats/SPARQL_Results_JSON",
            Self::Csv => "http://www.w3.org/ns/formats/SPARQL_Results_CSV",
            Self::Tsv => "http://www.w3.org/ns/formats/SPARQL_Results_TSV",
        }
    }

    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Xml => "application/sparql-results+xml",
            Self::Json => "application/sparql-results+json",
            Self::Csv => "text/csv; charset=utf-8",
            Self::Tsv => "text/tab-separated-values; charset=utf-8",
        }
    }

    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::Xml => "srx",
            Self::Json => "srj",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Xml => "SPARQL Results in XML",
            Self::Json => "SPARQL Results in JSON",
            Self::Csv => "SPARQL Results in CSV",
            Self::Tsv => "SPARQL Results in TSV",
        }
    }

    /// The format of a media type, parameters (`; charset=…`, `; version=1.2`) ignored, in
    /// any letter case; also the short and older names (`json`, `application/json`,
    /// `text/xml`…).
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        let base = media_type.split(';').next()?.trim().to_ascii_lowercase();
        Some(match base.as_str() {
            "application/sparql-results+xml" | "application/xml" | "text/xml" | "xml" => Self::Xml,
            "application/sparql-results+json" | "application/json" | "text/json" | "json" => {
                Self::Json
            }
            "text/csv" | "csv" => Self::Csv,
            "text/tab-separated-values" | "text/tsv" | "tsv" => Self::Tsv,
            _ => return None,
        })
    }

    /// The format of a file extension (`srj`, `json`, `srx`, `xml`, `csv`, `tsv`).
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "srx" | "xml" => Self::Xml,
            "srj" | "json" => Self::Json,
            "csv" | "txt" => Self::Csv,
            "tsv" => Self::Tsv,
            _ => return None,
        })
    }
}

impl fmt::Display for QueryResultsFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for format in QueryResultsFormat::ALL {
            assert_eq!(
                QueryResultsFormat::from_media_type(format.media_type()),
                Some(format)
            );
            assert_eq!(
                QueryResultsFormat::from_extension(format.file_extension()),
                Some(format)
            );
        }
        assert_eq!(
            QueryResultsFormat::from_media_type("application/sparql-results+json; version=1.2"),
            Some(QueryResultsFormat::Json)
        );
        assert_eq!(QueryResultsFormat::from_media_type("image/png"), None);
    }
}
