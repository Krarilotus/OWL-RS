use crate::symbols::SymbolTable;
use crate::vocabulary::{
    OWL_ASYMMETRIC_PROPERTY, OWL_DIFFERENT_FROM, OWL_DISJOINT_WITH, OWL_EQUIVALENT_CLASS,
    OWL_EQUIVALENT_PROPERTY, OWL_FUNCTIONAL_PROPERTY, OWL_INVERSE_FUNCTIONAL_PROPERTY,
    OWL_INVERSE_OF, OWL_IRREFLEXIVE_PROPERTY, OWL_NOTHING, OWL_PROPERTY_DISJOINT_WITH,
    OWL_REFLEXIVE_PROPERTY, OWL_SAME_AS, OWL_SYMMETRIC_PROPERTY, OWL_TRANSITIVE_PROPERTY, RDF_TYPE,
    RDFS_DOMAIN, RDFS_RANGE, RDFS_SUBCLASS_OF, RDFS_SUBPROPERTY_OF,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexedVocabulary {
    pub(super) rdf_type_id: u32,
    pub(super) rdfs_subclass_of_id: u32,
    pub(super) rdfs_subproperty_of_id: u32,
    pub(super) rdfs_domain_id: u32,
    pub(super) rdfs_range_id: u32,
    pub(super) owl_disjoint_with_id: u32,
    pub(super) owl_equivalent_class_id: u32,
    pub(super) owl_equivalent_property_id: u32,
    pub(super) owl_nothing_id: u32,
    pub(super) owl_same_as_id: u32,
    pub(super) owl_different_from_id: u32,
    pub(super) owl_functional_property_id: u32,
    pub(super) owl_inverse_functional_property_id: u32,
    pub(super) owl_inverse_of_id: u32,
    pub(super) owl_property_disjoint_with_id: u32,
    pub(super) owl_irreflexive_property_id: u32,
    pub(super) owl_asymmetric_property_id: u32,
    pub(super) owl_reflexive_property_id: u32,
    pub(super) owl_symmetric_property_id: u32,
    pub(super) owl_transitive_property_id: u32,
}

impl IndexedVocabulary {
    pub(super) fn new(symbols: &mut SymbolTable) -> Self {
        Self {
            rdf_type_id: symbols.get_or_intern(RDF_TYPE),
            rdfs_subclass_of_id: symbols.get_or_intern(RDFS_SUBCLASS_OF),
            rdfs_subproperty_of_id: symbols.get_or_intern(RDFS_SUBPROPERTY_OF),
            rdfs_domain_id: symbols.get_or_intern(RDFS_DOMAIN),
            rdfs_range_id: symbols.get_or_intern(RDFS_RANGE),
            owl_disjoint_with_id: symbols.get_or_intern(OWL_DISJOINT_WITH),
            owl_equivalent_class_id: symbols.get_or_intern(OWL_EQUIVALENT_CLASS),
            owl_equivalent_property_id: symbols.get_or_intern(OWL_EQUIVALENT_PROPERTY),
            owl_nothing_id: symbols.get_or_intern(OWL_NOTHING),
            owl_same_as_id: symbols.get_or_intern(OWL_SAME_AS),
            owl_different_from_id: symbols.get_or_intern(OWL_DIFFERENT_FROM),
            owl_functional_property_id: symbols.get_or_intern(OWL_FUNCTIONAL_PROPERTY),
            owl_inverse_functional_property_id: symbols
                .get_or_intern(OWL_INVERSE_FUNCTIONAL_PROPERTY),
            owl_inverse_of_id: symbols.get_or_intern(OWL_INVERSE_OF),
            owl_property_disjoint_with_id: symbols.get_or_intern(OWL_PROPERTY_DISJOINT_WITH),
            owl_irreflexive_property_id: symbols.get_or_intern(OWL_IRREFLEXIVE_PROPERTY),
            owl_asymmetric_property_id: symbols.get_or_intern(OWL_ASYMMETRIC_PROPERTY),
            owl_reflexive_property_id: symbols.get_or_intern(OWL_REFLEXIVE_PROPERTY),
            owl_symmetric_property_id: symbols.get_or_intern(OWL_SYMMETRIC_PROPERTY),
            owl_transitive_property_id: symbols.get_or_intern(OWL_TRANSITIVE_PROPERTY),
        }
    }
}
