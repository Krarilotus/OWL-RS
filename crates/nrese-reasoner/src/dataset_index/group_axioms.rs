use std::collections::{BTreeSet, HashMap};

use crate::symbols::SymbolTable;

use super::ids::IndexedVocabulary;
use super::rdf_list::parse_list;

pub(super) struct GroupAxiomExpansion<'a> {
    pub(super) symbols: &'a SymbolTable,
    pub(super) vocabulary: &'a IndexedVocabulary,
    pub(super) type_assertions: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) list_first_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) list_rest_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) members_heads_by_subject: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) distinct_members_heads_by_subject: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) different_from_pairs: &'a mut BTreeSet<(u32, u32)>,
    pub(super) disjoint_class_pairs: &'a mut HashMap<u32, BTreeSet<u32>>,
    pub(super) property_disjoint_pairs: &'a mut HashMap<u32, BTreeSet<u32>>,
    pub(super) diagnostics: &'a mut Vec<String>,
}

pub(super) fn expand_group_axioms(input: GroupAxiomExpansion<'_>) {
    let GroupAxiomExpansion {
        symbols,
        vocabulary,
        type_assertions,
        list_first_by_node,
        list_rest_by_node,
        members_heads_by_subject,
        distinct_members_heads_by_subject,
        different_from_pairs,
        disjoint_class_pairs,
        property_disjoint_pairs,
        diagnostics,
    } = input;

    let mut context = GroupAxiomContext {
        symbols,
        vocabulary,
        list_first_by_node,
        list_rest_by_node,
        diagnostics,
    };

    for (&subject_id, object_ids) in type_assertions {
        if object_ids.contains(&vocabulary.owl_all_different_id) {
            if let Some(heads) = distinct_members_heads_by_subject.get(&subject_id) {
                for &head_id in heads {
                    expand_all_different(&mut context, different_from_pairs, subject_id, head_id);
                }
            }
            if let Some(heads) = members_heads_by_subject.get(&subject_id) {
                for &head_id in heads {
                    expand_all_different(&mut context, different_from_pairs, subject_id, head_id);
                }
            }
        }
        if object_ids.contains(&vocabulary.owl_all_disjoint_classes_id)
            && let Some(heads) = members_heads_by_subject.get(&subject_id)
        {
            for &head_id in heads {
                expand_pairwise_group(
                    &mut context,
                    disjoint_class_pairs,
                    subject_id,
                    head_id,
                    "owl:AllDisjointClasses",
                );
            }
        }
        if object_ids.contains(&vocabulary.owl_all_disjoint_properties_id)
            && let Some(heads) = members_heads_by_subject.get(&subject_id)
        {
            for &head_id in heads {
                expand_pairwise_group(
                    &mut context,
                    property_disjoint_pairs,
                    subject_id,
                    head_id,
                    "owl:AllDisjointProperties",
                );
            }
        }
    }
}

struct GroupAxiomContext<'a> {
    symbols: &'a SymbolTable,
    vocabulary: &'a IndexedVocabulary,
    list_first_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    list_rest_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    diagnostics: &'a mut Vec<String>,
}

fn expand_all_different(
    context: &mut GroupAxiomContext<'_>,
    different_from_pairs: &mut BTreeSet<(u32, u32)>,
    subject_id: u32,
    head_id: u32,
) {
    match parse_members(context, subject_id, head_id, "owl:AllDifferent") {
        Ok(members) => {
            for left in 0..members.len() {
                for right in (left + 1)..members.len() {
                    different_from_pairs.insert(ordered_pair(members[left], members[right]));
                }
            }
        }
        Err(message) => context.diagnostics.push(message),
    }
}

fn expand_pairwise_group(
    context: &mut GroupAxiomContext<'_>,
    edges: &mut HashMap<u32, BTreeSet<u32>>,
    subject_id: u32,
    head_id: u32,
    label: &'static str,
) {
    match parse_members(context, subject_id, head_id, label) {
        Ok(members) => {
            for left in 0..members.len() {
                for right in (left + 1)..members.len() {
                    insert_symmetric_edge(edges, members[left], members[right]);
                }
            }
        }
        Err(message) => context.diagnostics.push(message),
    }
}

fn parse_members(
    context: &GroupAxiomContext<'_>,
    subject_id: u32,
    head_id: u32,
    label: &'static str,
) -> Result<Vec<u32>, String> {
    let members = parse_list(
        context.list_first_by_node,
        context.list_rest_by_node,
        context.vocabulary.rdf_nil_id,
        head_id,
    )
    .map_err(|reason| {
        format!(
            "{label} over malformed RDF lists is not implemented in rules-mvp; ignored axiom for <{}> ({reason})",
            resolve_or_unknown(context.symbols, subject_id)
        )
    })?;

    if members.len() < 2 {
        return Err(format!(
            "{label} with fewer than 2 members is not implemented in rules-mvp; ignored axiom for <{}>",
            resolve_or_unknown(context.symbols, subject_id)
        ));
    }

    Ok(members)
}

fn ordered_pair(left: u32, right: u32) -> (u32, u32) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn insert_symmetric_edge(edges: &mut HashMap<u32, BTreeSet<u32>>, left_id: u32, right_id: u32) {
    edges.entry(left_id).or_default().insert(right_id);
    edges.entry(right_id).or_default().insert(left_id);
}

fn resolve_or_unknown(symbols: &SymbolTable, term_id: u32) -> &str {
    symbols.resolve(term_id).unwrap_or("<unknown>")
}
