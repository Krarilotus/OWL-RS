#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReasoningStats {
    pub supported_asserted_triples: u64,
    pub unsupported_asserted_triples: u64,
    pub interned_terms: usize,
    pub subclass_edge_count: usize,
    pub subproperty_edge_count: usize,
    pub type_assertion_count: usize,
    pub property_assertion_count: usize,
    pub equality_assertion_count: usize,
    pub equality_cluster_count: usize,
    pub inferred_equality_link_count: usize,
    pub domain_assertion_count: usize,
    pub range_assertion_count: usize,
    pub taxonomy_node_count: usize,
    pub property_taxonomy_node_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectBlame {
    pub heuristic: &'static str,
    pub principal_contributor: String,
    pub principal_origin: String,
    pub contextual_contributor: String,
    pub contextual_origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectEvidence {
    pub role: &'static str,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectExplanation {
    pub summary: String,
    pub violated_constraint: String,
    pub focus_resource: String,
    pub primary_conflicting_term: String,
    pub secondary_conflicting_term: String,
    pub blame: RejectBlame,
    pub evidence: Vec<RejectEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InferenceDelta {
    pub inferred_triples: u64,
    pub consistency_violations: u64,
    pub derived_triples: Vec<(String, String, String)>,
    pub diagnostics: Vec<String>,
    pub primary_reject: Option<RejectExplanation>,
    pub stats: ReasoningStats,
}
