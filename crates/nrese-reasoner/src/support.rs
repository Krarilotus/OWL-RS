use std::collections::BTreeSet;

use crate::dataset_index::IndexedDataset;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::vocabulary::RDF_TYPE;

const UNSUPPORTED_OWL_PREDICATES: &[(&str, &str)] = &[
    (
        "http://www.w3.org/2002/07/owl#allValuesFrom",
        "owl:allValuesFrom is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#someValuesFrom",
        "owl:someValuesFrom is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#onProperty",
        "owl:onProperty restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#hasValue",
        "owl:hasValue restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#unionOf",
        "owl:unionOf is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#intersectionOf",
        "owl:intersectionOf is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#complementOf",
        "owl:complementOf is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#oneOf",
        "owl:oneOf is not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#minCardinality",
        "OWL cardinality restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#maxCardinality",
        "OWL cardinality restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#cardinality",
        "OWL cardinality restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#minQualifiedCardinality",
        "OWL qualified cardinality restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#maxQualifiedCardinality",
        "OWL qualified cardinality restrictions are not implemented in rules-mvp",
    ),
    (
        "http://www.w3.org/2002/07/owl#qualifiedCardinality",
        "OWL qualified cardinality restrictions are not implemented in rules-mvp",
    ),
];

const UNSUPPORTED_OWL_TYPES: &[(&str, &str)] = &[];

pub fn collect_unsupported_construct_diagnostics(
    index: &IndexedDataset,
    policy: &RulesMvpFeaturePolicy,
) -> Vec<String> {
    if !policy.unsupported_construct_diagnostics_enabled() {
        return Vec::new();
    }

    let mut diagnostics = BTreeSet::new();

    for &(subject_id, predicate_id, object_id) in index.asserted_triples() {
        let Some(predicate) = index.symbols().resolve(predicate_id) else {
            continue;
        };

        if let Some((_, message)) = UNSUPPORTED_OWL_PREDICATES
            .iter()
            .find(|(iri, _)| *iri == predicate)
        {
            diagnostics.insert(format!(
                "{message}; encountered axiom {} {} {}",
                resolve_or_unknown(index, subject_id),
                predicate,
                resolve_or_unknown(index, object_id)
            ));
        }

        if predicate == RDF_TYPE {
            let Some(object) = index.symbols().resolve(object_id) else {
                continue;
            };
            if let Some((_, message)) = UNSUPPORTED_OWL_TYPES.iter().find(|(iri, _)| *iri == object)
            {
                diagnostics.insert(format!(
                    "{message}; encountered declaration for {}",
                    resolve_or_unknown(index, subject_id)
                ));
            }
        }
    }

    diagnostics.into_iter().collect()
}

fn resolve_or_unknown(index: &IndexedDataset, term_id: u32) -> &str {
    index.symbols().resolve(term_id).unwrap_or("<unknown>")
}

#[cfg(test)]
#[path = "tests/support_tests.rs"]
mod tests;
