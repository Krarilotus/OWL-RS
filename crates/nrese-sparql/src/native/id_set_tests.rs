//! [`IdSet`]'s membership: an id is in the set exactly when one of the sorted, disjoint
//! ranges it was built from holds it, however many of those ranges are wide (a dictionary
//! string test's passing terms merge into thousands of runs of 64 ids or more).

use super::*;
use nrese_engine::TermKind;

/// The id of `kind` with `payload`.
fn id(kind: TermKind, payload: u64) -> TermId {
    TermId::from_raw(TermId::kind_range(kind).0.raw() + payload)
}

/// Whether one of `ranges` holds `id`: membership as defined.
fn held(ranges: &[(TermId, TermId)], id: u64) -> bool {
    ranges
        .iter()
        .any(|&(low, high)| low.raw() <= id && id <= high.raw())
}

#[test]
fn membership_is_the_ranges_with_many_wide_ranges() {
    // 3,000 wide string ranges of 100 ids with gaps of 7, single ids between some of them,
    // and a whole kind after them: sorted and disjoint, as a hint gives them.
    let mut ranges = Vec::new();
    for i in 0..3_000u64 {
        let low = 10 + i * 107;
        ranges.push((id(TermKind::String, low), id(TermKind::String, low + 99)));
        if i % 3 == 0 {
            let single = id(TermKind::String, low + 103);
            ranges.push((single, single));
        }
    }
    ranges.push(TermId::kind_range(TermKind::Integer));
    ranges.sort_unstable();
    let set = IdSet::new(&ranges);

    // Every string id from below the first range to above the last: each range's ends,
    // the ids just outside them, the gaps between two ranges.
    let last = 10 + 3_000 * 107;
    for payload in 0..=last {
        let probe = id(TermKind::String, payload).raw();
        assert_eq!(
            set.contains(probe),
            held(&ranges, probe),
            "string payload {payload}"
        );
    }
    let (low, high) = TermId::kind_range(TermKind::Integer);
    for probe in [low.raw() - 1, low.raw(), high.raw(), high.raw() + 1] {
        assert_eq!(set.contains(probe), held(&ranges, probe), "id {probe:#x}");
    }
}
