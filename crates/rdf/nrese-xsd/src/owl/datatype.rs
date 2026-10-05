//! The datatypes and facets of the OWL 2 datatype map (Structural Specification §4), by
//! IRI.

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

macro_rules! datatypes {
    ($($variant:ident = $ns:ident $local:literal),* $(,)?) => {
        /// A datatype of the OWL 2 datatype map, `rdfs:Literal`, or `rdf:langString`
        /// (RDF 1.1's datatype of language-tagged literals, read as the language-tagged
        /// part of `rdf:PlainLiteral`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Datatype {
            $($variant,)*
        }

        impl Datatype {
            /// Every datatype.
            pub const ALL: &'static [Datatype] = &[$(Datatype::$variant,)*];

            /// The datatype of an IRI, if it is one of these.
            pub fn from_iri(iri: &str) -> Option<Self> {
                $(if let Some(local) = iri.strip_prefix($ns) && local == $local {
                    return Some(Self::$variant);
                })*
                None
            }

            pub fn iri(self) -> String {
                match self {
                    $(Self::$variant => format!("{}{}", $ns, $local),)*
                }
            }
        }
    };
}

datatypes! {
    Literal = RDFS "Literal",
    Real = OWL "real",
    Rational = OWL "rational",
    Decimal = XSD "decimal",
    Integer = XSD "integer",
    NonNegativeInteger = XSD "nonNegativeInteger",
    NonPositiveInteger = XSD "nonPositiveInteger",
    PositiveInteger = XSD "positiveInteger",
    NegativeInteger = XSD "negativeInteger",
    Long = XSD "long",
    Int = XSD "int",
    Short = XSD "short",
    Byte = XSD "byte",
    UnsignedLong = XSD "unsignedLong",
    UnsignedInt = XSD "unsignedInt",
    UnsignedShort = XSD "unsignedShort",
    UnsignedByte = XSD "unsignedByte",
    Float = XSD "float",
    Double = XSD "double",
    String = XSD "string",
    NormalizedString = XSD "normalizedString",
    Token = XSD "token",
    Language = XSD "language",
    Name = XSD "Name",
    NcName = XSD "NCName",
    NmToken = XSD "NMTOKEN",
    PlainLiteral = RDF "PlainLiteral",
    LangString = RDF "langString",
    Boolean = XSD "boolean",
    HexBinary = XSD "hexBinary",
    Base64Binary = XSD "base64Binary",
    AnyUri = XSD "anyURI",
    DateTime = XSD "dateTime",
    DateTimeStamp = XSD "dateTimeStamp",
    XmlLiteral = RDF "XMLLiteral",
}

impl Datatype {
    /// The bounds of an integer datatype (`None`: unbounded on that side); `None` for
    /// the others.
    pub fn integer_bounds(self) -> Option<(Option<i128>, Option<i128>)> {
        Some(match self {
            Self::Integer => (None, None),
            Self::NonNegativeInteger => (Some(0), None),
            Self::NonPositiveInteger => (None, Some(0)),
            Self::PositiveInteger => (Some(1), None),
            Self::NegativeInteger => (None, Some(-1)),
            Self::Long => (Some(i64::MIN.into()), Some(i64::MAX.into())),
            Self::Int => (Some(i32::MIN.into()), Some(i32::MAX.into())),
            Self::Short => (Some(i16::MIN.into()), Some(i16::MAX.into())),
            Self::Byte => (Some(i8::MIN.into()), Some(i8::MAX.into())),
            Self::UnsignedLong => (Some(0), Some(u64::MAX.into())),
            Self::UnsignedInt => (Some(0), Some(u32::MAX.into())),
            Self::UnsignedShort => (Some(0), Some(u16::MAX.into())),
            Self::UnsignedByte => (Some(0), Some(u8::MAX.into())),
            _ => return None,
        })
    }

    /// Whether its values are numbers of `owl:real`.
    pub fn is_real(self) -> bool {
        matches!(self, Self::Real | Self::Rational | Self::Decimal)
            || self.integer_bounds().is_some()
    }

    /// The first string region (`text::Region`) its values may be in, for the string
    /// types (each type is a union of the regions from it on).
    pub(crate) fn string_floor(self) -> Option<usize> {
        Some(match self {
            Self::String => 0,
            Self::NormalizedString => 1,
            Self::Token => 2,
            Self::NmToken => 3,
            Self::Name => 4,
            Self::NcName => 5,
            Self::Language => 6,
            _ => return None,
        })
    }
}

/// A constraining facet of the datatype map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Facet {
    MinInclusive,
    MaxInclusive,
    MinExclusive,
    MaxExclusive,
    Length,
    MinLength,
    MaxLength,
    Pattern,
    LangRange,
}

impl Facet {
    pub fn from_iri(iri: &str) -> Option<Self> {
        if iri == format!("{RDF}langRange") {
            return Some(Self::LangRange);
        }
        Some(match iri.strip_prefix(XSD)? {
            "minInclusive" => Self::MinInclusive,
            "maxInclusive" => Self::MaxInclusive,
            "minExclusive" => Self::MinExclusive,
            "maxExclusive" => Self::MaxExclusive,
            "length" => Self::Length,
            "minLength" => Self::MinLength,
            "maxLength" => Self::MaxLength,
            "pattern" => Self::Pattern,
            _ => return None,
        })
    }

    pub fn iri(self) -> String {
        let local = match self {
            Self::MinInclusive => "minInclusive",
            Self::MaxInclusive => "maxInclusive",
            Self::MinExclusive => "minExclusive",
            Self::MaxExclusive => "maxExclusive",
            Self::Length => "length",
            Self::MinLength => "minLength",
            Self::MaxLength => "maxLength",
            Self::Pattern => "pattern",
            Self::LangRange => return format!("{RDF}langRange"),
        };
        format!("{XSD}{local}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iris_round_trip() {
        for &d in Datatype::ALL {
            assert_eq!(Datatype::from_iri(&d.iri()), Some(d));
        }
        assert_eq!(Datatype::from_iri(&format!("{XSD}date")), None);
        assert_eq!(
            Facet::from_iri(&Facet::LangRange.iri()),
            Some(Facet::LangRange)
        );
        assert_eq!(
            Facet::from_iri(&Facet::MaxLength.iri()),
            Some(Facet::MaxLength)
        );
    }
}
