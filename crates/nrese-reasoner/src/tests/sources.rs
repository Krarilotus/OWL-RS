//! Source reads outside rule evaluation count towards the stop (REAS-2, kind 2): a read that
//! repeats per item of another enumeration must not multiply after the stop. Counts of work a
//! base was asked for, never time.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::load;
use crate::batch::Schema;
use crate::delta::{Interrupted, MemoryBase, Rules, VisitsBase, update_until};
use crate::eval::{Probe, Visit};
use crate::ir::{Triple, parse_rules};
use crate::vocabulary::LocalVocabulary;

const POLL_UNITS: usize = nrese_exec::POLL_UNITS as usize;

/// A base that counts the membership probes asked of it once `armed`, and arms itself (and
/// fires the stop) at the first scan of `trigger`.
struct Probed {
    base: MemoryBase,
    trigger: [Option<u64>; 3],
    armed: AtomicBool,
    probes: AtomicUsize,
}

/// Reads are the inner base's.
impl VisitsBase for Probed {
    fn visit(&self, pattern: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        if pattern == self.trigger {
            self.armed.store(true, Ordering::Relaxed);
        }
        self.base.visit(pattern, visit);
    }

    fn size(&self, pattern: [Option<u64>; 3], probe: Probe<'_, '_>) -> usize {
        self.base.size(pattern, probe)
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool {
        if self.armed.load(Ordering::Relaxed) {
            self.probes.fetch_add(1, Ordering::Relaxed);
        }
        self.base.holds(fact, probe)
    }

    fn asserts(&self, fact: Triple, probe: Probe<'_, '_>) -> bool {
        self.base.asserts(fact, probe)
    }
}

/// M11 (sweep): inserting an edge `(x, y)` into a transitive relation where `x` has `n`
/// predecessors and `y` has `n` successors closes it by probing every predecessor ×
/// successor pair. The update's stop fires when that closure opens its first scan; the
/// probes after it are attempted work, at most `POLL_UNITS` plus the pairs of the scan in
/// progress, not n² (unstopped, it probes (n + 1)²). Each size is reported before any is
/// checked, so the log shows how the work grows with the input.
#[test]
fn an_inserted_transitive_edge_s_closure_counts_towards_the_stop() {
    let sizes = [100, 200, 400];
    let mut seen = Vec::new();
    for n in sizes {
        let mut vocabulary = LocalVocabulary::default();
        let rules = parse_rules(
            "trp: (?x rdfs:seeAlso ?y), (?y rdfs:seeAlso ?z) -> (?x rdfs:seeAlso ?z)",
            &mut vocabulary,
        )
        .unwrap();
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
        };
        let text: Vec<String> = (0..n)
            .map(|i| format!("ex:a{i} rdfs:seeAlso ex:x"))
            .chain((0..n).map(|j| format!("ex:y rdfs:seeAlso ex:b{j}")))
            .collect();
        let mut asserted = load(&mut vocabulary, &text.join("\n"));
        let edge = load(&mut vocabulary, "ex:x rdfs:seeAlso ex:y");
        asserted.extend(&edge);
        asserted.sort_unstable();
        let [x, p, _] = edge[0];
        // Closed before the edge: no pair a_i p y or x p b_j yet, nothing inferred.
        let base = Probed {
            base: MemoryBase::new(&asserted, &[]),
            trigger: [None, Some(p), Some(x)],
            armed: AtomicBool::new(false),
            probes: AtomicUsize::new(0),
        };
        let stop = || base.armed.load(Ordering::Relaxed);
        let result = update_until(&base, &edge, &[], compiled, None, &stop);
        seen.push((n, result.err(), base.probes.load(Ordering::Relaxed)));
    }
    for &(n, _, probes) in &seen {
        println!(
            "n {n}: {probes} membership probes after the stop ((n + 1)² = {})",
            (n + 1) * (n + 1)
        );
    }
    for (n, err, probes) in seen {
        assert_eq!(
            err,
            Some(Interrupted::default()),
            "n {n}: the update is interrupted"
        );
        let bound = POLL_UNITS + 2 * (n + 1);
        assert!(
            probes <= bound,
            "n {n}: {probes} probes after the stop (bound {bound}; unstopped (n + 1)² = {})",
            (n + 1) * (n + 1)
        );
    }
}
