mod builders;
mod detectors;
mod plan;
mod prepared;

pub(crate) use detectors::detect_property_characteristic_conflicts_prepared;
pub(crate) use plan::PropertyCharacteristicPlan;
pub(crate) use prepared::PreparedPropertyAssertions;

#[cfg(test)]
pub(crate) fn detect_property_characteristic_conflicts(
    index: &crate::dataset_index::IndexedDataset,
    property_closure: &crate::property_closure::PropertyClosure,
) -> Vec<crate::class_consistency::ConsistencyViolation> {
    let plan = PropertyCharacteristicPlan::build(index);
    let prepared = PreparedPropertyAssertions::build(&plan, property_closure);
    detect_property_characteristic_conflicts_prepared(index, &plan, &prepared)
}

#[cfg(test)]
#[path = "../tests/property_consistency_tests.rs"]
mod tests;
