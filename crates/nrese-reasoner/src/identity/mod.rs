mod consistency;
mod equality;
mod inference;

pub(crate) use consistency::detect_different_from_conflicts;
pub(crate) use equality::EqualityIndex;
pub(crate) use inference::prepare_identity;
