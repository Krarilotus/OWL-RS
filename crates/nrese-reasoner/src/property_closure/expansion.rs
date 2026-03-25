use crate::identity::EqualityIndex;

pub(super) fn for_each_equality_expansion(
    equality: &EqualityIndex,
    subject_id: u32,
    object_id: u32,
    enabled: bool,
    mut visit: impl FnMut(u32, u32),
) {
    if !enabled {
        visit(subject_id, object_id);
        return;
    }

    match (
        equality.equivalents_of(subject_id),
        equality.equivalents_of(object_id),
    ) {
        (Some(subjects), Some(objects)) => {
            for &expanded_subject in subjects {
                for &expanded_object in objects {
                    visit(expanded_subject, expanded_object);
                }
            }
        }
        (Some(subjects), None) => {
            for &expanded_subject in subjects {
                visit(expanded_subject, object_id);
            }
        }
        (None, Some(objects)) => {
            for &expanded_object in objects {
                visit(subject_id, expanded_object);
            }
        }
        (None, None) => visit(subject_id, object_id),
    }
}
