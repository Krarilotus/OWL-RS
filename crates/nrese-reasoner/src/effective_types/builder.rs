use std::collections::{BTreeMap, HashMap};

use crate::dataset_index::IndexedDataset;
use crate::identity::EqualityIndex;
use crate::property_closure::PropertyClosure;
use crate::taxonomy::TaxonomyIndex;

use super::origins::{
    DirectTypeOrigin, EffectiveTypeOrigin, direct_origin_rank, effective_origin_rank,
};
use super::{EffectiveTypeSet, from_types_by_instance};

type DirectTypesByInstance = HashMap<u32, BTreeMap<u32, DirectTypeOrigin>>;
type EffectiveTypesByInstance = HashMap<u32, BTreeMap<u32, EffectiveTypeOrigin>>;

pub(super) fn build_effective_type_set(
    index: &IndexedDataset,
    class_taxonomy: &TaxonomyIndex,
    property_closure: &PropertyClosure,
    equality: &EqualityIndex,
    include_domain_range_typing: bool,
) -> EffectiveTypeSet {
    let mut direct_types =
        collect_direct_types(index, property_closure, include_domain_range_typing);
    propagate_equality_direct_types(equality, &mut direct_types);
    from_types_by_instance(promote_effective_types(&direct_types, class_taxonomy))
}

fn collect_direct_types(
    index: &IndexedDataset,
    property_closure: &PropertyClosure,
    include_domain_range_typing: bool,
) -> DirectTypesByInstance {
    let mut direct_types = DirectTypesByInstance::new();

    for (&instance_id, class_ids) in index.type_assertions() {
        for &class_id in class_ids {
            insert_direct_origin(
                &mut direct_types,
                instance_id,
                class_id,
                DirectTypeOrigin::Asserted,
            );
        }
    }

    if include_domain_range_typing {
        for &(subject_id, predicate_id, object_id) in property_closure.all_assertions() {
            if let Some(domain_classes) = index.domain_by_property().get(&predicate_id) {
                for &class_id in domain_classes {
                    insert_direct_origin(
                        &mut direct_types,
                        subject_id,
                        class_id,
                        DirectTypeOrigin::Domain {
                            via_property_id: predicate_id,
                        },
                    );
                }
            }

            if let Some(range_classes) = index.range_by_property().get(&predicate_id) {
                for &class_id in range_classes {
                    insert_direct_origin(
                        &mut direct_types,
                        object_id,
                        class_id,
                        DirectTypeOrigin::Range {
                            via_property_id: predicate_id,
                        },
                    );
                }
            }
        }
    }

    direct_types
}

fn promote_effective_types(
    direct_types: &DirectTypesByInstance,
    class_taxonomy: &TaxonomyIndex,
) -> EffectiveTypesByInstance {
    let mut types_by_instance = EffectiveTypesByInstance::new();

    for (&instance_id, class_map) in direct_types {
        let effective = types_by_instance.entry(instance_id).or_default();

        for (&class_id, origin) in class_map {
            insert_effective_origin(
                effective,
                class_id,
                EffectiveTypeOrigin::Direct(origin.clone()),
            );

            if let Some(ancestors) = class_taxonomy.ancestors_of(class_id) {
                for &ancestor_id in ancestors {
                    insert_effective_origin(
                        effective,
                        ancestor_id,
                        EffectiveTypeOrigin::Inherited {
                            from_class_id: class_id,
                            from_origin: origin.clone(),
                        },
                    );
                }
            }
        }
    }

    types_by_instance
}

fn insert_direct_origin(
    direct_types: &mut DirectTypesByInstance,
    instance_id: u32,
    class_id: u32,
    candidate: DirectTypeOrigin,
) {
    let entry = direct_types.entry(instance_id).or_default();
    match entry.get(&class_id) {
        Some(existing) if direct_origin_rank(existing) <= direct_origin_rank(&candidate) => {}
        _ => {
            entry.insert(class_id, candidate);
        }
    }
}

fn insert_effective_origin(
    effective: &mut BTreeMap<u32, EffectiveTypeOrigin>,
    class_id: u32,
    candidate: EffectiveTypeOrigin,
) {
    match effective.get(&class_id) {
        Some(existing) if effective_origin_rank(existing) <= effective_origin_rank(&candidate) => {}
        _ => {
            effective.insert(class_id, candidate);
        }
    }
}

fn propagate_equality_direct_types(
    equality: &EqualityIndex,
    direct_types: &mut DirectTypesByInstance,
) {
    let source_instance_ids = direct_types.keys().copied().collect::<Vec<_>>();

    for source_instance_id in source_instance_ids {
        let Some(equivalents) = equality.equivalents_of(source_instance_id) else {
            continue;
        };
        let Some(source_types) = direct_types.get(&source_instance_id).cloned() else {
            continue;
        };

        for &target_instance_id in equivalents {
            if target_instance_id == source_instance_id {
                continue;
            }

            for (&class_id, origin) in &source_types {
                insert_direct_origin(
                    direct_types,
                    target_instance_id,
                    class_id,
                    DirectTypeOrigin::SameAs {
                        source_instance_id,
                        source_origin: Box::new(origin.clone()),
                    },
                );
            }
        }
    }
}
