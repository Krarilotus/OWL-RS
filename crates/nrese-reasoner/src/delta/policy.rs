//! The maintenance policy: how an update maintains what a change touches (B3a of
//! ADR-0011). DRed overdeletes what a deleted fact may have supported, one derivation at a
//! time, and puts back what still has one; where a change touches a part of the state
//! whose facts derive each other many times over, that costs more than recomputing the
//! part, or the whole. The policy estimates each plan, in facts touched, and takes the
//! cheapest:
//!
//! - [`Plan::Dred`]: DRed with B/F, the default.
//! - [`Plan::Recompute`]: the facts of a region (those that mention its terms) are
//!   overdeleted at once, with their consequences, and derived again from what remains,
//!   seeded by the region's asserted facts: the part recomputed inside the commit.
//! - [`Plan::Rebuild`]: the update stops before it does any work, and the caller
//!   materialises the state after the change in the commit. Only a caller that can offers
//!   it, with its estimate ([`Policy::rebuild`]), and only for a change that inserts
//!   nothing: a rebuild reports no violations, and deletions can't introduce any. A split
//!   over a stack kept over representatives always takes it ([`Policy::split_rebuilds`]).
//!
//! The one region kind so far is an `owl:sameAs` class a deletion may split: DRed runs the
//! equality rules over it, which copy every fact of a member to every other member (a
//! 500-member split took 75 s, a rematerialisation 0.15 s). Other maintenance cases (large
//! deletes, long chains) add their regions and estimates here.

use hashbrown::HashSet;

use super::Base;
use crate::ir::Triple;

/// The plan an update took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Plan {
    #[default]
    Dred,
    Recompute,
    Rebuild,
}

/// Which plans [`super::update_with`] may choose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// The plan to take wherever it applies (the differential tests of each plan);
    /// `None`: the cheapest.
    pub force: Option<Plan>,
    /// What a rebuild costs (the facts a materialisation of the whole state reads and
    /// writes), if the caller offers one.
    pub rebuild: Option<u64>,
    /// The caller rebuilds after any update that may split a class, whatever its plan (a
    /// stack kept over representatives, until the delta executor reads it through a
    /// representative view, B3b): there a split's delta only adds to the rebuild.
    pub split_rebuilds: bool,
}

impl Policy {
    /// `plan` wherever it applies.
    pub fn forcing(plan: Plan) -> Self {
        Self {
            force: Some(plan),
            ..Self::default()
        }
    }

    /// Whether the update stops at once for a rebuild: forced, and offered.
    pub(super) fn rebuilds_at_once(self) -> bool {
        self.force == Some(Plan::Rebuild) && self.rebuild.is_some()
    }
}

/// What the policy weighed for an update, and what it did (the counters of a commit's
/// report).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub plan: Plan,
    /// The estimates the policy compared, in facts touched.
    pub dred_estimate: u64,
    pub recompute_estimate: u64,
    pub rebuild_estimate: u64,
    /// The region's terms and the facts that mention them.
    pub region_terms: u64,
    pub region_facts: u64,
    /// Facts overdeleted (the region's included) and put back by rederivation.
    pub overdeleted: u64,
    pub rederived: u64,
}

/// What the policy decided for the equalities an update meets.
pub(super) enum Decision {
    /// Go on as planned.
    Keep,
    /// Recompute the facts that mention these terms.
    Recompute(HashSet<u64>),
    /// Stop: the caller rebuilds.
    Rebuild,
}

/// Facts each fact of a region costs when the region is recomputed: overdeleted, put
/// back (rederived or derived again), and expanded to its copies once more.
const RECOMPUTE_PER_FACT: u64 = 3;

/// The terms of the `owl:sameAs` classes of `equalities` (pairs of distinct terms) in
/// `base`, as one region, with the facts that mention them and what DRed would cost:
/// every such fact copied to every member of its class while overdeleting, and its
/// copies looked for while rederiving. O(the classes' members and their facts' counts).
fn equality_region<B: Base + ?Sized>(
    base: &B,
    same_as: u64,
    equalities: impl Iterator<Item = Triple>,
) -> Option<(HashSet<u64>, u64, u64)> {
    let mut terms: HashSet<u64> = HashSet::new();
    let (mut facts, mut dred) = (0u64, 0u64);
    for [a, _, b] in equalities {
        if terms.contains(&a) && terms.contains(&b) {
            continue;
        }
        // The class: everything linked to either side by `owl:sameAs` in the state, followed
        // both ways and transitively (the change's own insertions may link classes).
        let mut members: HashSet<u64> = HashSet::from_iter([a, b]);
        let mut queue = vec![a, b];
        while let Some(x) = queue.pop() {
            for pattern in [
                [Some(x), Some(same_as), None],
                [None, Some(same_as), Some(x)],
            ] {
                base.scan(pattern, &mut |t| {
                    for y in [t[0], t[2]] {
                        if members.insert(y) {
                            queue.push(y);
                        }
                    }
                });
            }
        }
        let mut class_facts = 0u64;
        for &m in &members {
            for pattern in [
                [Some(m), None, None],
                [None, Some(m), None],
                [None, None, Some(m)],
            ] {
                class_facts += base.estimate(pattern) as u64;
            }
        }
        facts += class_facts;
        dred += 2 * class_facts * members.len() as u64;
        terms.extend(members);
    }
    (!terms.is_empty()).then_some((terms, facts, dred))
}

/// The decision for an update whose overdeletion meets `equalities` (deleted or about to
/// be overdeleted), under `policy`.
pub(super) fn plan_for_equalities<B: Base + ?Sized>(
    base: &B,
    same_as: u64,
    equalities: impl Iterator<Item = Triple>,
    policy: Policy,
    counts: &mut Counts,
) -> Decision {
    let mut equalities = equalities.peekable();
    if equalities.peek().is_none() {
        return Decision::Keep;
    }
    counts.rebuild_estimate = policy.rebuild.unwrap_or(0);
    let may_rebuild = matches!(policy.force, None | Some(Plan::Rebuild));
    if may_rebuild && policy.split_rebuilds && policy.rebuild.is_some() {
        // Any delta plan ends in the rebuild: no estimate (over the expanded view, the
        // class's walk was 52 of a 500-member split's 115 ms).
        return Decision::Rebuild;
    }
    let Some((terms, facts, dred)) = equality_region(base, same_as, equalities) else {
        return Decision::Keep;
    };
    let recompute = RECOMPUTE_PER_FACT * facts;
    counts.dred_estimate += dred;
    counts.recompute_estimate += recompute;
    match policy.force {
        Some(Plan::Dred) => Decision::Keep,
        Some(Plan::Recompute) => Decision::Recompute(terms),
        None | Some(Plan::Rebuild) => {
            // The update's regions so far, this one included, against one rebuild.
            let cheapest = counts.dred_estimate.min(counts.recompute_estimate);
            if policy.rebuild.is_some_and(|rebuild| rebuild < cheapest) {
                Decision::Rebuild
            } else if dred > recompute {
                Decision::Recompute(terms)
            } else {
                Decision::Keep
            }
        }
    }
}
