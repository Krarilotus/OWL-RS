//! The RDF formats, by name, file extension and media type.

use std::fmt;

/// An RDF serialisation format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RdfFormat {
    /// [N-Triples](https://www.w3.org/TR/n-triples/)
    NTriples,
    /// [N-Quads](https://www.w3.org/TR/n-quads/)
    NQuads,
    /// [Turtle](https://www.w3.org/TR/turtle/)
    Turtle,
    /// [TriG](https://www.w3.org/TR/trig/)
    TriG,
    /// [RDF/XML](https://www.w3.org/TR/rdf-syntax-grammar/)
    RdfXml,
    /// [JSON-LD 1.1](https://www.w3.org/TR/json-ld11/)
    JsonLd,
    /// [Notation3](https://w3c.github.io/N3/spec/) (W3C Community Group). As an RDF
    /// format it reads documents RDF can hold and writes Turtle, which is N3; formulas,
    /// variables and rules go through [`crate::n3`].
    N3,
}

impl RdfFormat {
    pub const ALL: [Self; 7] = [
        Self::NTriples,
        Self::NQuads,
        Self::Turtle,
        Self::TriG,
        Self::RdfXml,
        Self::JsonLd,
        Self::N3,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::NTriples => "N-Triples",
            Self::NQuads => "N-Quads",
            Self::Turtle => "Turtle",
            Self::TriG => "TriG",
            Self::RdfXml => "RDF/XML",
            Self::JsonLd => "JSON-LD",
            Self::N3 => "N3",
        }
    }

    /// The IANA media type.
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::NTriples => "application/n-triples",
            Self::NQuads => "application/n-quads",
            Self::Turtle => "text/turtle",
            Self::TriG => "application/trig",
            Self::RdfXml => "application/rdf+xml",
            Self::JsonLd => "application/ld+json",
            Self::N3 => "text/n3",
        }
    }

    /// The usual file extension, without the dot.
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::NTriples => "nt",
            Self::NQuads => "nq",
            Self::Turtle => "ttl",
            Self::TriG => "trig",
            Self::RdfXml => "rdf",
            Self::JsonLd => "jsonld",
            Self::N3 => "n3",
        }
    }

    /// Whether the format can hold named graphs.
    pub const fn supports_datasets(self) -> bool {
        matches!(self, Self::NQuads | Self::TriG | Self::JsonLd)
    }

    /// The format of a file extension (any letter case; `owl` is RDF/XML, `json` JSON-LD).
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "nt" | "ntriples" => Self::NTriples,
            "nq" | "nquads" => Self::NQuads,
            "ttl" | "turtle" => Self::Turtle,
            "trig" => Self::TriG,
            "rdf" | "xml" | "owl" | "rdfxml" => Self::RdfXml,
            "jsonld" | "json" => Self::JsonLd,
            "n3" => Self::N3,
            _ => return None,
        })
    }

    /// The format of a media type, parameters (`; charset=utf-8`) ignored, including the
    /// usual aliases (`text/plain` for N-Triples is not one: it means any text).
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        let essence = media_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        Some(match essence.as_str() {
            "application/n-triples" => Self::NTriples,
            "application/n-quads" | "text/x-nquads" | "text/nquads" => Self::NQuads,
            "text/turtle" | "application/turtle" | "application/x-turtle" => Self::Turtle,
            "application/trig" | "application/x-trig" => Self::TriG,
            "application/rdf+xml" | "application/xml" | "text/xml" => Self::RdfXml,
            "application/ld+json" | "application/json" => Self::JsonLd,
            "text/n3" | "text/rdf+n3" | "application/n3" => Self::N3,
            _ => return None,
        })
    }
}

impl fmt::Display for RdfFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for format in RdfFormat::ALL {
            assert_eq!(
                RdfFormat::from_extension(format.file_extension()),
                Some(format)
            );
            assert_eq!(
                RdfFormat::from_media_type(format.media_type()),
                Some(format)
            );
        }
        assert_eq!(
            RdfFormat::from_media_type("text/turtle; charset=utf-8"),
            Some(RdfFormat::Turtle)
        );
        assert_eq!(RdfFormat::from_extension("OWL"), Some(RdfFormat::RdfXml));
        assert_eq!(RdfFormat::from_media_type("text/plain"), None);
    }
}
