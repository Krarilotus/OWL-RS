use std::collections::{BTreeMap, BTreeSet};

use crate::property_closure::PropertyClosure;

use super::plan::PropertyCharacteristicPlan;

type PropertySubjectKey = (u32, u32);
type PropertyObjectKey = (u32, u32);
type SubjectObjectKey = (u32, u32);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PreparedPropertyAssertions {
    self_loops: Vec<(u32, u32)>,
    outgoing_objects_by_property_subject: BTreeMap<PropertySubjectKey, BTreeSet<u32>>,
    incoming_subjects_by_property_object: BTreeMap<PropertyObjectKey, BTreeSet<u32>>,
    predicates_by_subject_object: BTreeMap<SubjectObjectKey, BTreeSet<u32>>,
}

impl PreparedPropertyAssertions {
    pub(crate) fn build(
        plan: &PropertyCharacteristicPlan,
        property_closure: &PropertyClosure,
    ) -> Self {
        let mut prepared = Self::default();

        for &(subject_id, predicate_id, object_id) in property_closure.all_assertions() {
            if !plan.is_constrained_predicate(predicate_id) {
                continue;
            }

            if subject_id == object_id {
                prepared.self_loops.push((subject_id, predicate_id));
            }

            prepared
                .outgoing_objects_by_property_subject
                .entry((predicate_id, subject_id))
                .or_default()
                .insert(object_id);
            prepared
                .incoming_subjects_by_property_object
                .entry((predicate_id, object_id))
                .or_default()
                .insert(subject_id);
            prepared
                .predicates_by_subject_object
                .entry((subject_id, object_id))
                .or_default()
                .insert(predicate_id);
        }

        prepared
    }

    pub(crate) fn self_loops(&self) -> &[(u32, u32)] {
        &self.self_loops
    }

    pub(crate) fn outgoing_objects_by_property_subject(
        &self,
    ) -> &BTreeMap<PropertySubjectKey, BTreeSet<u32>> {
        &self.outgoing_objects_by_property_subject
    }

    pub(crate) fn incoming_subjects_by_property_object(
        &self,
    ) -> &BTreeMap<PropertyObjectKey, BTreeSet<u32>> {
        &self.incoming_subjects_by_property_object
    }

    pub(crate) fn predicates_by_subject_object(
        &self,
    ) -> &BTreeMap<SubjectObjectKey, BTreeSet<u32>> {
        &self.predicates_by_subject_object
    }

    pub(crate) fn objects_for(&self, predicate_id: u32, subject_id: u32) -> Option<&BTreeSet<u32>> {
        self.outgoing_objects_by_property_subject
            .get(&(predicate_id, subject_id))
    }
}
