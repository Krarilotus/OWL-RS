//! REAS-2 fix check r1 (R2-F1, held counts): a count tells its visitor about a run's
//! tombstone words as it reads them, so a caller's stop ends the read within a run of words.

use super::run::Run;
use super::{IndexVersion, Layout};
use crate::quad::{AccessPlan, EncodedQuad, QuadPattern};
use crate::term::{TermId, TermKind};
use crate::witness::{self, Site};

fn term(n: u64) -> TermId {
    TermId::new(TermKind::Iri, n + 1)
}

/// Spec (`IndexVersion::count_plan_visiting`, `Snapshot::count_in_visiting`'s contract):
/// `visit(n)` is called before the work it counts, `n` at most `POLL_UNITS`, and nothing more
/// is read once it returns false. A version of `n` quads `s_i p o`, then a run deleting all
/// but the first: counting `? p ?` reads that run's tombstone bitmap, `n / 64` words, twice
/// `POLL_UNITS` of them. A visitor that answers "stop" at its first call after any bitmap
/// word was read: the count answers nothing, and at most `POLL_UNITS` words plus the range's
/// two edge words were read. Each call reports at most `POLL_UNITS` units.
#[test]
fn a_held_count_reads_its_tombstones_within_a_run_of_its_visitor() {
    let interval = u64::from(nrese_exec::POLL_UNITS);
    let n = 2 * interval * 64 + 1;
    let (p, o) = (term(1), term(2));
    let quads: Vec<EncodedQuad> = (0..n)
        .map(|i| EncodedQuad::new(term(10 + i), p, o, TermId::DEFAULT_GRAPH))
        .collect();
    let version = IndexVersion::empty(Layout::DefaultGraph)
        .with_run(Run::from_delta(Layout::DefaultGraph, &quads, &[]))
        .with_run(Run::from_delta(Layout::DefaultGraph, &[], &quads[1..]));
    let plan = AccessPlan::for_pattern(&QuadPattern {
        predicate: Some(p),
        ..QuadPattern::all()
    });
    let before = witness::get(Site::TombstoneWords);
    let largest = std::cell::Cell::new(0);
    let counted = version.count_plan_visiting(&plan, &|units| {
        largest.set(largest.get().max(units));
        witness::get(Site::TombstoneWords) == before
    });
    let largest = largest.get();
    let read = witness::get(Site::TombstoneWords) - before;
    assert!(
        read <= interval + 2,
        "{read} bitmap words read: more than {interval} plus the two edge words before the \
         visitor was asked"
    );
    assert_eq!(counted, None, "a count its visitor stopped answers nothing");
    assert!(
        largest <= interval,
        "a visit of {largest} units: at most {interval} at once"
    );
}
