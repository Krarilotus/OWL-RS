//! IRIs of the RDF, RDFS, XML Schema, OWL and GeoSPARQL vocabularies.

macro_rules! vocabulary {
    ($(#[$doc:meta])* $module:ident = $namespace:literal { $($name:ident = $local:literal),* $(,)? }) => {
        $(#[$doc])*
        pub mod $module {
            use crate::term::NamedNodeRef;

            pub const NAMESPACE: &str = $namespace;

            $(
                pub const $name: NamedNodeRef<'static> =
                    NamedNodeRef::new_unchecked(concat!($namespace, $local));
            )*
        }
    };
}

vocabulary!(
    /// RDF 1.1 (`rdf:`).
    rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#" {
        ALT = "Alt",
        BAG = "Bag",
        DIR_LANG_STRING = "dirLangString",
        FIRST = "first",
        HTML = "HTML",
        JSON = "JSON",
        LANG_STRING = "langString",
        LIST = "List",
        NIL = "nil",
        OBJECT = "object",
        PLAIN_LITERAL = "PlainLiteral",
        PREDICATE = "predicate",
        PROPERTY = "Property",
        REST = "rest",
        SEQ = "Seq",
        STATEMENT = "Statement",
        SUBJECT = "subject",
        TYPE = "type",
        VALUE = "value",
        XML_LITERAL = "XMLLiteral",
    }
);

vocabulary!(
    /// RDF Schema (`rdfs:`).
    rdfs = "http://www.w3.org/2000/01/rdf-schema#" {
        CLASS = "Class",
        COMMENT = "comment",
        CONTAINER = "Container",
        CONTAINER_MEMBERSHIP_PROPERTY = "ContainerMembershipProperty",
        DATATYPE = "Datatype",
        DOMAIN = "domain",
        IS_DEFINED_BY = "isDefinedBy",
        LABEL = "label",
        LITERAL = "Literal",
        MEMBER = "member",
        RANGE = "range",
        RESOURCE = "Resource",
        SEE_ALSO = "seeAlso",
        SUB_CLASS_OF = "subClassOf",
        SUB_PROPERTY_OF = "subPropertyOf",
    }
);

vocabulary!(
    /// XML Schema datatypes (`xsd:`).
    xsd = "http://www.w3.org/2001/XMLSchema#" {
        ANY_URI = "anyURI",
        BASE_64_BINARY = "base64Binary",
        BOOLEAN = "boolean",
        BYTE = "byte",
        DATE = "date",
        DATE_TIME = "dateTime",
        DATE_TIME_STAMP = "dateTimeStamp",
        DAY_TIME_DURATION = "dayTimeDuration",
        DECIMAL = "decimal",
        DOUBLE = "double",
        DURATION = "duration",
        FLOAT = "float",
        G_DAY = "gDay",
        G_MONTH = "gMonth",
        G_MONTH_DAY = "gMonthDay",
        G_YEAR = "gYear",
        G_YEAR_MONTH = "gYearMonth",
        HEX_BINARY = "hexBinary",
        INT = "int",
        INTEGER = "integer",
        LANGUAGE = "language",
        LONG = "long",
        NAME = "Name",
        NC_NAME = "NCName",
        NEGATIVE_INTEGER = "negativeInteger",
        NMTOKEN = "NMTOKEN",
        NON_NEGATIVE_INTEGER = "nonNegativeInteger",
        NON_POSITIVE_INTEGER = "nonPositiveInteger",
        NORMALIZED_STRING = "normalizedString",
        POSITIVE_INTEGER = "positiveInteger",
        SHORT = "short",
        STRING = "string",
        TIME = "time",
        TOKEN = "token",
        UNSIGNED_BYTE = "unsignedByte",
        UNSIGNED_INT = "unsignedInt",
        UNSIGNED_LONG = "unsignedLong",
        UNSIGNED_SHORT = "unsignedShort",
        YEAR_MONTH_DURATION = "yearMonthDuration",
    }
);

vocabulary!(
    /// OWL 2 (`owl:`): the terms the RL rules and the OWL 2 datatype map use.
    owl = "http://www.w3.org/2002/07/owl#" {
        ALL_DIFFERENT = "AllDifferent",
        ALL_DISJOINT_CLASSES = "AllDisjointClasses",
        ALL_DISJOINT_PROPERTIES = "AllDisjointProperties",
        ALL_VALUES_FROM = "allValuesFrom",
        ASYMMETRIC_PROPERTY = "AsymmetricProperty",
        CLASS = "Class",
        COMPLEMENT_OF = "complementOf",
        DATATYPE_PROPERTY = "DatatypeProperty",
        DIFFERENT_FROM = "differentFrom",
        DISJOINT_WITH = "disjointWith",
        DISTINCT_MEMBERS = "distinctMembers",
        EQUIVALENT_CLASS = "equivalentClass",
        EQUIVALENT_PROPERTY = "equivalentProperty",
        FUNCTIONAL_PROPERTY = "FunctionalProperty",
        HAS_KEY = "hasKey",
        HAS_SELF = "hasSelf",
        HAS_VALUE = "hasValue",
        INTERSECTION_OF = "intersectionOf",
        INVERSE_FUNCTIONAL_PROPERTY = "InverseFunctionalProperty",
        INVERSE_OF = "inverseOf",
        IRREFLEXIVE_PROPERTY = "IrreflexiveProperty",
        MAX_CARDINALITY = "maxCardinality",
        MAX_QUALIFIED_CARDINALITY = "maxQualifiedCardinality",
        MEMBERS = "members",
        NAMED_INDIVIDUAL = "NamedIndividual",
        NOTHING = "Nothing",
        OBJECT_PROPERTY = "ObjectProperty",
        ON_CLASS = "onClass",
        ON_PROPERTY = "onProperty",
        ONE_OF = "oneOf",
        PROPERTY_CHAIN_AXIOM = "propertyChainAxiom",
        PROPERTY_DISJOINT_WITH = "propertyDisjointWith",
        RATIONAL = "rational",
        REAL = "real",
        RESTRICTION = "Restriction",
        SAME_AS = "sameAs",
        SOME_VALUES_FROM = "someValuesFrom",
        SOURCE_INDIVIDUAL = "sourceIndividual",
        SYMMETRIC_PROPERTY = "SymmetricProperty",
        TARGET_INDIVIDUAL = "targetIndividual",
        THING = "Thing",
        TRANSITIVE_PROPERTY = "TransitiveProperty",
        UNION_OF = "unionOf",
    }
);

vocabulary!(
    /// GeoSPARQL 1.1 (`geo:`): the literal datatypes.
    geosparql = "http://www.opengis.net/ont/geosparql#" {
        GEO_JSON_LITERAL = "geoJSONLiteral",
        GML_LITERAL = "gmlLiteral",
        WKT_LITERAL = "wktLiteral",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_namespace_and_local_name() {
        assert_eq!(
            rdf::TYPE.as_str(),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
        );
        assert_eq!(
            xsd::DATE_TIME.as_str(),
            "http://www.w3.org/2001/XMLSchema#dateTime"
        );
        assert!(owl::SAME_AS.as_str().starts_with(owl::NAMESPACE));
        assert!(crate::iri::Iri::parse(geosparql::WKT_LITERAL.as_str()).is_ok());
    }
}
