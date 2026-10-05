//! The RDF, RDFS, OWL and XSD terms the mapping reads, by their ids in the source (absent
//! ones never match).

pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const OWL: &str = "http://www.w3.org/2002/07/owl#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// The IRIs of the OWL 2 datatype map's datatypes and facets, `rdfs:Literal` and
/// `rdf:langString`: those a source's ids are looked up by when it can't give an IRI's
/// text.
pub fn data_iris() -> Vec<String> {
    let xsd = [
        "decimal",
        "integer",
        "nonNegativeInteger",
        "nonPositiveInteger",
        "positiveInteger",
        "negativeInteger",
        "long",
        "int",
        "short",
        "byte",
        "unsignedLong",
        "unsignedInt",
        "unsignedShort",
        "unsignedByte",
        "float",
        "double",
        "string",
        "normalizedString",
        "token",
        "language",
        "Name",
        "NCName",
        "NMTOKEN",
        "boolean",
        "hexBinary",
        "base64Binary",
        "anyURI",
        "dateTime",
        "dateTimeStamp",
        "minInclusive",
        "maxInclusive",
        "minExclusive",
        "maxExclusive",
        "length",
        "minLength",
        "maxLength",
        "pattern",
    ];
    let mut out: Vec<String> = xsd.iter().map(|l| format!("{XSD}{l}")).collect();
    out.extend(["real", "rational"].map(|l| format!("{OWL}{l}")));
    out.extend(
        ["PlainLiteral", "langString", "XMLLiteral", "langRange"].map(|l| format!("{RDF}{l}")),
    );
    out.push(format!("{RDFS}Literal"));
    out
}

macro_rules! vocabulary {
    ($($field:ident = $ns:ident $local:literal),* $(,)?) => {
        /// The ids of the terms the mapping reads.
        #[derive(Debug, Clone, Copy)]
        pub struct Vocabulary {
            $(pub $field: Option<u64>,)*
        }

        impl Vocabulary {
            /// Looks every term up through `id_of`.
            pub fn new(id_of: &dyn Fn(&str) -> Option<u64>) -> Self {
                Self {
                    $($field: id_of(&format!("{}{}", $ns, $local)),)*
                }
            }

            /// Every term with its IRI (for writing).
            pub fn iris() -> Vec<(&'static str, String)> {
                vec![$((stringify!($field), format!("{}{}", $ns, $local)),)*]
            }
        }
    };
}

vocabulary! {
    rdf_type = RDF "type",
    first = RDF "first",
    rest = RDF "rest",
    nil = RDF "nil",
    rdfs_sub_class_of = RDFS "subClassOf",
    rdfs_sub_property_of = RDFS "subPropertyOf",
    rdfs_domain = RDFS "domain",
    rdfs_range = RDFS "range",
    rdfs_datatype = RDFS "Datatype",
    rdfs_literal = RDFS "Literal",
    rdfs_class = RDFS "Class",
    rdf_property = RDF "Property",
    rdf_list = RDF "List",
    owl_data_range = OWL "DataRange",
    owl_top_object_property = OWL "topObjectProperty",
    owl_bottom_object_property = OWL "bottomObjectProperty",
    owl_top_data_property = OWL "topDataProperty",
    owl_bottom_data_property = OWL "bottomDataProperty",
    owl_class = OWL "Class",
    owl_restriction = OWL "Restriction",
    owl_thing = OWL "Thing",
    owl_nothing = OWL "Nothing",
    owl_object_property = OWL "ObjectProperty",
    owl_datatype_property = OWL "DatatypeProperty",
    owl_annotation_property = OWL "AnnotationProperty",
    owl_named_individual = OWL "NamedIndividual",
    owl_ontology = OWL "Ontology",
    owl_imports = OWL "imports",
    owl_functional = OWL "FunctionalProperty",
    owl_inverse_functional = OWL "InverseFunctionalProperty",
    owl_reflexive = OWL "ReflexiveProperty",
    owl_irreflexive = OWL "IrreflexiveProperty",
    owl_symmetric = OWL "SymmetricProperty",
    owl_asymmetric = OWL "AsymmetricProperty",
    owl_transitive = OWL "TransitiveProperty",
    owl_intersection_of = OWL "intersectionOf",
    owl_union_of = OWL "unionOf",
    owl_complement_of = OWL "complementOf",
    owl_one_of = OWL "oneOf",
    owl_on_property = OWL "onProperty",
    owl_on_properties = OWL "onProperties",
    owl_some_values_from = OWL "someValuesFrom",
    owl_all_values_from = OWL "allValuesFrom",
    owl_has_value = OWL "hasValue",
    owl_has_self = OWL "hasSelf",
    owl_min_cardinality = OWL "minCardinality",
    owl_max_cardinality = OWL "maxCardinality",
    owl_cardinality = OWL "cardinality",
    owl_min_qualified = OWL "minQualifiedCardinality",
    owl_max_qualified = OWL "maxQualifiedCardinality",
    owl_qualified = OWL "qualifiedCardinality",
    owl_on_class = OWL "onClass",
    owl_on_data_range = OWL "onDataRange",
    owl_inverse_of = OWL "inverseOf",
    owl_datatype_complement_of = OWL "datatypeComplementOf",
    owl_on_datatype = OWL "onDatatype",
    owl_with_restrictions = OWL "withRestrictions",
    owl_equivalent_class = OWL "equivalentClass",
    owl_disjoint_with = OWL "disjointWith",
    owl_all_disjoint_classes = OWL "AllDisjointClasses",
    owl_members = OWL "members",
    owl_disjoint_union_of = OWL "disjointUnionOf",
    owl_property_chain_axiom = OWL "propertyChainAxiom",
    owl_equivalent_property = OWL "equivalentProperty",
    owl_property_disjoint_with = OWL "propertyDisjointWith",
    owl_all_disjoint_properties = OWL "AllDisjointProperties",
    owl_has_key = OWL "hasKey",
    owl_same_as = OWL "sameAs",
    owl_different_from = OWL "differentFrom",
    owl_all_different = OWL "AllDifferent",
    owl_distinct_members = OWL "distinctMembers",
    owl_negative_property_assertion = OWL "NegativePropertyAssertion",
    owl_source_individual = OWL "sourceIndividual",
    owl_assertion_property = OWL "assertionProperty",
    owl_target_individual = OWL "targetIndividual",
    owl_target_value = OWL "targetValue",
    owl_axiom = OWL "Axiom",
    owl_annotation = OWL "Annotation",
    owl_annotated_source = OWL "annotatedSource",
    owl_annotated_property = OWL "annotatedProperty",
    owl_annotated_target = OWL "annotatedTarget",
    owl_deprecated_class = OWL "DeprecatedClass",
    owl_deprecated_property = OWL "DeprecatedProperty",
    owl_ontology_property = OWL "OntologyProperty",
}
