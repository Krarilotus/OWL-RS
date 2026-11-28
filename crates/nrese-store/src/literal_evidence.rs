//! The typed object counts kept with a publication's read basis.

use std::sync::{Arc, OnceLock};

use nrese_engine::{
    AssertedIdentity, EncodedQuad, QuadPattern, ReadModel, Snapshot, TermId, TermKind, Transaction,
};
use nrese_exec::{Budget, BudgetExceeded, Reservation};
use parking_lot::Mutex;

use crate::service::ReadBasis;

type Change = [i64; 2];
type Counts = [u64; 2];

const KINDS: [(TermKind, TermKind); 3] = [
    (TermKind::Integer, TermKind::DateTime),
    (TermKind::TypedLiteral, TermKind::TypedLiteral),
    (TermKind::DerivedInteger, TermKind::DerivedInteger),
];

fn value_literal(quad: &EncodedQuad) -> bool {
    let kind = quad.object.kind();
    KINDS
        .iter()
        .any(|&(first, last)| (first..=last).contains(&kind))
}

fn key(predicate: Option<TermId>, graph: Option<TermId>) -> u128 {
    let bits = |term: Option<TermId>| term.map_or(u64::MAX, TermId::raw);
    (u128::from(bits(predicate)) << 64) | u128::from(bits(graph))
}

/// A compressed binary radix tree for the pairs and their aggregates. A successor copies
/// only the paths it writes (at most 128 branches per key). During a rebuild the nodes
/// are uniquely owned and updated in place.
#[derive(Clone, Default)]
struct Link(Option<Arc<Node>>);

struct Node {
    example: u128,
    prefix: u32,
    entry: Entry,
    _held: Reservation,
}

#[derive(Clone)]
enum Entry {
    Leaf(Counts),
    Fork([Link; 2]),
}

fn side(key: u128, prefix: u32) -> usize {
    ((key >> (127 - prefix)) & 1) as usize
}

fn apply(counts: Counts, [asserted, inferred]: Change) -> Counts {
    [
        counts[0].checked_add_signed(asserted),
        counts[1].checked_add_signed(inferred),
    ]
    .map(|count| count.expect("counts describe the transaction's base"))
}

impl Drop for Link {
    fn drop(&mut self) {
        // Take the node out of its allocation before returning its charge.
        drop(self.0.take().and_then(Arc::into_inner));
    }
}

impl Node {
    fn charged(
        example: u128,
        prefix: u32,
        entry: Entry,
        budget: &Arc<Budget>,
    ) -> Result<Arc<Self>, BudgetExceeded> {
        let alignment = align_of::<Self>().max(align_of::<usize>());
        let bytes = ((2 * size_of::<usize>()).next_multiple_of(align_of::<Self>())
            + size_of::<Self>())
        .next_multiple_of(alignment);
        let mut held = Reservation::new(Arc::clone(budget));
        held.charge(bytes)?;
        Ok(Arc::new(Self {
            example,
            prefix,
            entry,
            _held: held,
        }))
    }
}

impl Link {
    fn get(&self, key: u128) -> Counts {
        let mut link = self;
        while let Some(node) = link.0.as_deref() {
            if (node.example ^ key).leading_zeros() < node.prefix {
                break;
            }
            match &node.entry {
                Entry::Leaf(counts) => return *counts,
                Entry::Fork(children) => link = &children[side(key, node.prefix)],
            }
        }
        [0; 2]
    }

    fn adjust(
        &mut self,
        key: u128,
        change: Change,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        let leaf = || Node::charged(key, 128, Entry::Leaf(apply([0; 2], change)), budget);
        let Some(root) = self.0.as_mut() else {
            self.0 = Some(leaf()?);
            return Ok(());
        };
        let prefix = (root.example ^ key).leading_zeros();
        if prefix < root.prefix {
            let mut children = [Link(Some(Arc::clone(root))), Link(Some(leaf()?))];
            if side(key, prefix) == 0 {
                children.swap(0, 1);
            }
            *self = Link(Some(Node::charged(
                key,
                prefix,
                Entry::Fork(children),
                budget,
            )?));
            return Ok(());
        }
        if Arc::get_mut(root).is_none() {
            let next = Node::charged(root.example, root.prefix, root.entry.clone(), budget)?;
            // Drop the old holder through Link so the last release frees the Arc first.
            drop(Link(Some(std::mem::replace(root, next))));
        }
        let root = Arc::get_mut(root).expect("unique node");
        match &mut root.entry {
            Entry::Leaf(counts) => {
                *counts = apply(*counts, change);
                if *counts == [0; 2] {
                    *self = Link::default();
                }
            }
            Entry::Fork(children) => {
                let index = side(key, root.prefix);
                children[index].adjust(key, change, budget)?;
                if children[index].0.is_none() {
                    *self = std::mem::take(&mut children[1 - index]);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct LiteralEvidence {
    asserted: AssertedIdentity,
    counts: Link,
}

impl LiteralEvidence {
    fn new(asserted: AssertedIdentity) -> Self {
        Self {
            asserted,
            counts: Link::default(),
        }
    }

    fn adjust(
        &mut self,
        quad: EncodedQuad,
        change: Change,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetExceeded> {
        if change != [0; 2] {
            for predicate in [Some(quad.predicate), None] {
                for graph in [Some(quad.graph), None] {
                    self.counts.adjust(key(predicate, graph), change, budget)?;
                }
            }
        }
        Ok(())
    }

    fn build(snapshot: &Snapshot, budget: &Arc<Budget>) -> Result<Self, BudgetExceeded> {
        use nrese_engine::quad::Permutation;
        let stored = snapshot.stored();
        let mut evidence = Self::new(stored.asserted_identity());
        for (model, change) in [(ReadModel::Asserted, [1, 0]), (ReadModel::Inferred, [0, 1])] {
            let mut scan = Reservation::new(budget.as_ref());
            scan.charge(stored.scan_bytes(model))?;
            for (first, last) in KINDS {
                let range = stored.scan_range_in(
                    model,
                    &QuadPattern::all(),
                    Permutation::Ospg,
                    TermId::kind_range(first).0,
                    TermId::kind_range(last).1,
                );
                for quad in range.expect("every stored layout has an object-first order") {
                    evidence.adjust(quad, change, budget)?;
                }
            }
        }
        Ok(evidence)
    }

    pub(crate) fn after(
        &self,
        tx: &Transaction<'_>,
        budget: &Arc<Budget>,
    ) -> Result<Self, BudgetExceeded> {
        let base = tx.base().stored();
        let mut next = self.clone();
        next.asserted = tx.asserted_identity_after();
        let change = |model, quad: &EncodedQuad| {
            i64::from(tx.contains_in(model, quad)) - i64::from(base.contains_in(model, quad))
        };
        for quad in tx.inserted().chain(tx.deleted()).filter(value_literal) {
            // An assertion can take a quad out of the inferred stack even before the engine
            // finalises disjointness. Transaction membership already accounts for that.
            next.adjust(
                quad,
                [
                    change(ReadModel::Asserted, &quad),
                    change(ReadModel::Inferred, &quad),
                ],
                budget,
            )?;
        }
        for quad in tx
            .inferred_inserted()
            .chain(tx.inferred_deleted())
            .map(|triple| triple.in_default_graph())
            .filter(value_literal)
        {
            // The asserted pass accounted for both stacks of every quad it touched.
            if change(ReadModel::Asserted, &quad) == 0 {
                next.adjust(quad, [0, change(ReadModel::Inferred, &quad)], budget)?;
            }
        }
        Ok(next)
    }

    pub(crate) fn holds(
        snapshot: &Snapshot,
        predicate: Option<&str>,
        access: Option<&nrese_sparql::GraphAccess>,
    ) -> Option<bool> {
        let evidence = ReadBasis::of(snapshot)?.literal_evidence.get()?.as_ref()?;
        if evidence.asserted != snapshot.asserted_identity() {
            return None;
        }
        let predicate = match predicate {
            Some(iri) => {
                match snapshot.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into()) {
                    Some(id) => Some(id),
                    None => return Some(false),
                }
            }
            None => None,
        };
        let count = |p, g| evidence.counts.get(key(p, g));
        let inferred = access.is_none_or(|a| a.inferred);
        let default = access.is_none_or(|a| a.default_graph);
        let classes = (inferred && default)
            .then(|| snapshot.equality_classes())
            .flatten();
        // Stored ranges can miss the literal aliases that these classes introduce.
        if classes
            .as_ref()
            .is_some_and(|classes| classes.relate_literals())
        {
            return None;
        }
        let canonical = predicate.map(|p| classes.as_ref().map_or(p, |c| c.representative(p)));
        let g = Some(TermId::DEFAULT_GRAPH);
        let asserted = match access {
            None => count(predicate, None)[0] != 0,
            Some(access) => {
                (default && count(predicate, g)[0] != 0)
                    || access
                        .graphs
                        .iter()
                        .filter(|iri| access.allows(iri))
                        .any(|iri| {
                            snapshot
                                .lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into())
                                .is_some_and(|id| count(predicate, Some(id))[0] != 0)
                        })
            }
        };
        if asserted
            || (inferred && default && predicate != canonical && count(canonical, g)[0] != 0)
        {
            return Some(true);
        }
        // A support-scoped or pending view can have different inferences. Published counts
        // only speak for its unchanged assertions. A hidden default graph cannot supply aliases.
        if inferred {
            if snapshot.shared_state().is_none()
                || (!default && predicate.is_some() && snapshot.identity().equality.is_some())
            {
                return None;
            }
            if count(canonical, g)[1] != 0 {
                return Some(true);
            }
        }
        access
            .is_none_or(|a| a.prefixes.is_empty())
            .then_some(false)
    }
}

pub(crate) type Record = OnceLock<Option<LiteralEvidence>>;

#[cfg(test)]
mod tests;

/// One rebuild at a time for a store. A newer request replaces a queued snapshot, while
/// the running build can finish its own basis even if readers still hold that old snapshot.
#[derive(Clone)]
pub(crate) struct Builder {
    pub(crate) budget: Arc<Budget>,
    work: Arc<Mutex<Work>>,
}

#[derive(Default)]
struct Work {
    active: bool,
    next: Option<Snapshot>,
}

impl Builder {
    pub(crate) fn new(budget: Arc<Budget>) -> Self {
        Self {
            budget,
            work: Arc::default(),
        }
    }

    pub(crate) fn queue(&self, snapshot: Snapshot) {
        let Some(basis) = ReadBasis::of(&snapshot) else {
            return;
        };
        if basis.literal_evidence.get().is_some() {
            return;
        }
        if snapshot.len() == 0 {
            let _ = basis
                .literal_evidence
                .set(Some(LiteralEvidence::new(snapshot.asserted_identity())));
            return;
        }
        let mut work = self.work.lock();
        if work
            .next
            .as_ref()
            .and_then(ReadBasis::of)
            .is_some_and(|next| next.id > basis.id)
        {
            return;
        }
        work.next = Some(snapshot);
        if !work.active {
            work.active = true;
            let builder = self.clone();
            rayon::spawn(move || builder.run());
        }
    }

    fn run(&self) {
        loop {
            let snapshot = {
                let mut work = self.work.lock();
                let Some(snapshot) = work.next.take() else {
                    work.active = false;
                    return;
                };
                snapshot
            };
            self.fill(&snapshot);
        }
    }

    pub(crate) fn fill(&self, snapshot: &Snapshot) {
        let basis = ReadBasis::of(snapshot).expect("a queued publication has a basis");
        basis
            .literal_evidence
            .get_or_init(|| LiteralEvidence::build(snapshot, &self.budget).ok());
    }
}
