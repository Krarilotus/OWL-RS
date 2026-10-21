//! Narrow representations, refused typed at their exact limit (the admission design's §3).
//!
//! Each helper takes a value to store and returns it as a `u32`, or the limit it passes. A
//! limit caps the largest value stored, not a count: where `u32::MAX` is a sentinel (END, an
//! unmatched row, an unseen node), indices `0..u32::MAX` are valid, so `u32::MAX` items fit.

/// A value past the largest its representation stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepresentationLimit {
    /// What was to be stored.
    pub what: &'static str,
    /// The largest value that can be.
    pub limit: u64,
}

impl std::fmt::Display for RepresentationLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} past the largest that fits, {}",
            self.what, self.limit
        )
    }
}

impl std::error::Error for RepresentationLimit {}

fn narrow(value: usize, what: &'static str, limit: u32) -> Result<u32, RepresentationLimit> {
    u32::try_from(value)
        .ok()
        .filter(|&value| value <= limit)
        .ok_or(RepresentationLimit {
            what,
            limit: u64::from(limit),
        })
}

/// A row index of a hash chain, a merge pair or a table sort: `u32::MAX` is the sentinel (no
/// row, unmatched) or a range's end, so the largest index is one below it.
pub fn row_index(index: usize) -> Result<u32, RepresentationLimit> {
    narrow(index, "a row index", u32::MAX - 1)
}

/// A group id as it is issued: every value up to `u32::MAX`, no sentinel, so up to
/// `u32::MAX + 1` groups fit.
pub fn group_id(id: usize) -> Result<u32, RepresentationLimit> {
    narrow(id, "a group id", u32::MAX)
}

/// A CSR offset: edge positions `0..=m`, so up to `u32::MAX` retained edges.
pub fn edge_offset(offset: usize) -> Result<u32, RepresentationLimit> {
    narrow(offset, "an edge offset", u32::MAX)
}

/// A dense node id or a Tarjan order: `u32::MAX` is UNSEEN, so the largest id is one below it.
pub fn node_id(id: usize) -> Result<u32, RepresentationLimit> {
    narrow(id, "a node id", u32::MAX - 1)
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    /// Each helper at its limit−1, limit and limit+1: the limit is the largest value stored,
    /// so a count of `u32::MAX` rows fits where the largest index is `u32::MAX - 1`.
    #[test]
    fn each_helper_refuses_just_past_its_largest_value() {
        type Helper = fn(usize) -> Result<u32, RepresentationLimit>;
        let helpers: [(Helper, u32); 4] = [
            (row_index, u32::MAX - 1),
            (group_id, u32::MAX),
            (edge_offset, u32::MAX),
            (node_id, u32::MAX - 1),
        ];
        for (helper, limit) in helpers {
            let at = limit as usize;
            assert_eq!(helper(at - 1), Ok(limit - 1));
            assert_eq!(helper(at), Ok(limit));
            let refused = helper(at + 1).unwrap_err();
            assert_eq!(refused.limit, u64::from(limit), "{}", refused.what);
        }
    }
}
