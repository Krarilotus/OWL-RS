//! Equality by representatives, read side (work package W4, stage B).
//!
//! With equality on ([`crate::Engine::set_equality`]), the default graph of the inferred
//! stack holds the closure over one representative per `owl:sameAs` class (its smallest
//! id) instead of every fact for every identity: a class of k identities saves up to k³
//! copies of a fact. Each identity other than the representative is stored as
//! `identity owl:sameAs representative`, which is how a version finds its classes.
//!
//! Reads of the default graph in the models with inferred statements see the closure as if
//! every copy were stored: the stored facts over representatives, each expanded to every
//! identity of its terms ([`Expand`]). Constants of a pattern are looked up by their
//! representative and kept in the answer. Stored facts that are not over representatives
//! (the `sameAs` facts that place an identity in its class, and asserted facts about other
//! identities, which their rewritten copy stands for) are skipped. Named graphs and the
//! asserted model read the statements as stored. As without equality by representatives,
//! a statement asserted only in a named graph is not in the default graph
//! ([`Visibility`]); the copies for its other identities are.
//!
//! Expansion keeps the scan order: a representative is its class's smallest id, so every
//! copy of a fact sorts at or after the fact itself, and a heap holds the copies until the
//! scan has passed them.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use hashbrown::HashMap;
use parking_lot::Mutex;

use super::{ReadModel, Stack, Version};
use crate::quad::{EncodedQuad, GraphSelector, Key, Permutation, QuadPattern};
use crate::term::TermId;

/// The classes of one version, computed at the first read that needs them and passed on
/// to the next version while no commit changes a `sameAs` statement.
#[derive(Default)]
pub(crate) struct EqualityCell(Mutex<Option<(TermId, Arc<Classes>)>>);

impl std::fmt::Debug for EqualityCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EqualityCell").finish_non_exhaustive()
    }
}

impl EqualityCell {
    /// The classes of `version` under `same_as`.
    pub(crate) fn classes(&self, version: &Version, same_as: TermId) -> Arc<Classes> {
        let mut cell = self.0.lock();
        if let Some((id, classes)) = cell.as_ref()
            && *id == same_as
        {
            return Arc::clone(classes);
        }
        let classes = Arc::new(Classes::of(version, same_as));
        *cell = Some((same_as, Arc::clone(&classes)));
        classes
    }
}

/// `owl:sameAs` classes of two or more identities.
#[derive(Debug, Default)]
pub struct Classes {
    /// Every identity of a class other than its representative, with the representative.
    aliases: HashMap<u64, u64>,
    /// Each representative's identities, sorted (the representative first).
    members: HashMap<u64, Box<[u64]>>,
}

impl Classes {
    /// The classes the `sameAs` statements of `version`'s default graph make.
    fn of(version: &Version, same_as: TermId) -> Self {
        let pattern = QuadPattern {
            subject: None,
            predicate: Some(same_as),
            object: None,
            graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        };
        let mut parent: HashMap<u64, u64> = HashMap::new();
        fn find(parent: &mut HashMap<u64, u64>, x: u64) -> u64 {
            let mut x = x;
            loop {
                let p = *parent.get(&x).unwrap_or(&x);
                if p == x {
                    return x;
                }
                let grand = *parent.get(&p).unwrap_or(&p);
                parent.insert(x, grand);
                x = grand;
            }
        }
        for stack in Stack::ALL {
            for quad in version.stack(stack).scan(&pattern) {
                let (a, b) = (quad.subject.raw(), quad.object.raw());
                if a == b {
                    continue;
                }
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent.insert(ra.max(rb), ra.min(rb));
                    parent.entry(ra.min(rb)).or_insert(ra.min(rb));
                }
            }
        }
        let nodes: Vec<u64> = parent.keys().copied().collect();
        let mut members: HashMap<u64, Vec<u64>> = HashMap::new();
        for node in nodes {
            let root = find(&mut parent, node);
            members.entry(root).or_default().push(node);
        }
        let mut classes = Self::default();
        for (_, mut identities) in members {
            if identities.len() < 2 {
                continue;
            }
            identities.sort_unstable();
            // The smallest id represents the class, as the reasoner chooses.
            let representative = identities[0];
            for &identity in &identities[1..] {
                classes.aliases.insert(identity, representative);
            }
            classes
                .members
                .insert(representative, identities.into_boxed_slice());
        }
        classes
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The number of classes of two or more identities.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Each class of two or more identities: its representative and its identities,
    /// sorted (the representative first).
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u64])> {
        self.members.iter().map(|(&r, m)| (r, &m[..]))
    }

    /// The representative of `id` (`id` itself outside every class).
    pub fn representative(&self, id: TermId) -> TermId {
        self.aliases
            .get(&id.raw())
            .map_or(id, |&r| TermId::from_raw(r))
    }

    /// The identities of `id`'s class, sorted, if it has two or more.
    pub fn class_of(&self, id: TermId) -> Option<&[u64]> {
        self.members
            .get(&self.representative(id).raw())
            .map(|m| &m[..])
    }

    /// Whether `id` is an identity of a class other than its representative.
    fn is_alias(&self, id: u64) -> bool {
        self.aliases.contains_key(&id)
    }

    /// The identities a stored representative `id` stands for (`None`: itself only).
    fn expansion(&self, id: u64) -> Option<&[u64]> {
        self.members.get(&id).map(|m| &m[..])
    }

    /// `pattern` with its constants replaced by their representatives.
    pub(crate) fn normalised(&self, pattern: &QuadPattern) -> QuadPattern {
        QuadPattern {
            subject: pattern.subject.map(|t| self.representative(t)),
            predicate: pattern.predicate.map(|t| self.representative(t)),
            object: pattern.object.map(|t| self.representative(t)),
            graph: pattern.graph,
        }
    }

    /// Whether some term of `quad` has other identities.
    pub(crate) fn touches(&self, quad: &EncodedQuad) -> bool {
        [quad.subject, quad.predicate, quad.object]
            .iter()
            .any(|term| self.members.contains_key(&term.raw()))
    }

    /// Whether a stored default-graph quad is over representatives (else a copy of it is
    /// stored over representatives, or it places an identity in its class).
    pub(crate) fn is_canonical(&self, quad: &EncodedQuad) -> bool {
        !(self.is_alias(quad.subject.raw())
            || self.is_alias(quad.predicate.raw())
            || self.is_alias(quad.object.raw()))
    }

    /// How many quads a canonical stored quad stands for, given the pattern's constants
    /// (a constant stands for itself).
    pub(crate) fn multiplicity(&self, quad: &EncodedQuad, pattern: &QuadPattern) -> u64 {
        let size = |bound: Option<TermId>, id: TermId| match bound {
            Some(_) => 1,
            None => self.expansion(id.raw()).map_or(1, |m| m.len() as u64),
        };
        size(pattern.subject, quad.subject)
            * size(pattern.predicate, quad.predicate)
            * size(pattern.object, quad.object)
    }

    /// The quads a canonical stored quad stands for, the pattern's constants in place.
    pub(crate) fn expand(
        &self,
        quad: &EncodedQuad,
        pattern: &QuadPattern,
        out: &mut impl FnMut(EncodedQuad),
    ) {
        let values = |bound: Option<TermId>, id: TermId| -> Values<'_> {
            match bound {
                Some(constant) => Values::One(constant.raw()),
                None => match self.expansion(id.raw()) {
                    Some(members) => Values::Many(members),
                    None => Values::One(id.raw()),
                },
            }
        };
        let (s, p, o) = (
            values(pattern.subject, quad.subject),
            values(pattern.predicate, quad.predicate),
            values(pattern.object, quad.object),
        );
        let graph = quad.graph.raw();
        for &s in s.as_slice() {
            for &p in p.as_slice() {
                for &o in o.as_slice() {
                    out(EncodedQuad::from_components([s, p, o, graph]));
                }
            }
        }
    }
}

enum Values<'a> {
    One(u64),
    Many(&'a [u64]),
}

impl Values<'_> {
    fn as_slice(&self) -> &[u64] {
        match self {
            Values::One(value) => std::slice::from_ref(value),
            Values::Many(values) => values,
        }
    }
}

/// Which copies of a stored default-graph quad a read shows: in the inferred model none
/// that is asserted; in either model none asserted only in named graphs (a statement
/// asserted somewhere is never inferred).
#[derive(Clone, Copy)]
pub(crate) struct Visibility<'a> {
    version: &'a Version,
    inferred_only: bool,
    /// The asserted stack holds named graphs.
    named_graphs: bool,
}

impl<'a> Visibility<'a> {
    pub(crate) fn new(version: &'a Version, model: ReadModel) -> Self {
        Self {
            version,
            inferred_only: model == ReadModel::Inferred,
            named_graphs: version.asserted.layout() == crate::index::Layout::Quads,
        }
    }

    fn asserted_anywhere(&self, copy: &EncodedQuad) -> bool {
        if !self.named_graphs {
            return self.version.asserted.contains(copy);
        }
        let pattern = QuadPattern {
            subject: Some(copy.subject),
            predicate: Some(copy.predicate),
            object: Some(copy.object),
            graph: GraphSelector::Any,
        };
        self.version.asserted.scan(&pattern).next().is_some()
    }

    /// Whether `copy` (in the default graph), a copy of a stored quad that `touching`
    /// classes or not, is shown.
    pub(crate) fn shows(&self, copy: &EncodedQuad, touching: bool) -> bool {
        if self.inferred_only {
            return !self.asserted_anywhere(copy);
        }
        // Without classes the stored quad is the only copy: asserted in the default graph,
        // or inferred (never asserted anywhere).
        if !touching || !self.named_graphs {
            return true;
        }
        self.version.asserted.contains(copy) || !self.asserted_anywhere(copy)
    }

    /// Whether every copy of a stored quad is shown without asking each.
    pub(crate) fn shows_all(&self, touching: bool) -> bool {
        !self.inferred_only && (!touching || !self.named_graphs)
    }
}

/// A sorted scan of stored quads read with expansion: default-graph quads over
/// representatives expanded, others skipped; quads of named graphs passed on when
/// `named` (they match the pattern as given); copies shown as [`Visibility`] says. Sorted
/// by `permutation` as its input is.
pub(crate) struct Expand<'a, I: Iterator<Item = EncodedQuad>> {
    input: std::iter::Peekable<I>,
    classes: Arc<Classes>,
    pattern: QuadPattern,
    permutation: Permutation,
    named: bool,
    visibility: Visibility<'a>,
    pending: BinaryHeap<Reverse<Key>>,
}

impl<'a, I: Iterator<Item = EncodedQuad>> Expand<'a, I> {
    pub(crate) fn new(
        input: I,
        classes: Arc<Classes>,
        pattern: QuadPattern,
        permutation: Permutation,
        named: bool,
        model: ReadModel,
        version: &'a Version,
    ) -> Self {
        Self {
            input: input.peekable(),
            classes,
            pattern,
            permutation,
            named,
            visibility: Visibility::new(version, model),
            pending: BinaryHeap::new(),
        }
    }

    /// The key the quad `raw` would have in the answer at the least: its own values with
    /// the pattern's constants in place.
    fn least_key(pattern: &QuadPattern, permutation: Permutation, raw: &EncodedQuad) -> Key {
        let quad = EncodedQuad::from_components([
            pattern.subject.unwrap_or(raw.subject).raw(),
            pattern.predicate.unwrap_or(raw.predicate).raw(),
            pattern.object.unwrap_or(raw.object).raw(),
            raw.graph.raw(),
        ]);
        permutation.to_key(&quad)
    }

    /// Moves the next input quad's copies into the heap; `false` at the end of the input.
    fn pull(&mut self) -> bool {
        let Some(raw) = self.input.next() else {
            return false;
        };
        if !raw.graph.is_default_graph() {
            if self.named {
                self.pending.push(Reverse(self.permutation.to_key(&raw)));
            }
            return true;
        }
        if !self.classes.is_canonical(&raw) {
            return true;
        }
        let (permutation, visibility) = (self.permutation, self.visibility);
        let touching = self.classes.touches(&raw);
        let all = visibility.shows_all(touching);
        let pending = &mut self.pending;
        self.classes.expand(&raw, &self.pattern, &mut |quad| {
            if all || visibility.shows(&quad, touching) {
                pending.push(Reverse(permutation.to_key(&quad)));
            }
        });
        true
    }
}

impl<I: Iterator<Item = EncodedQuad>> Iterator for Expand<'_, I> {
    type Item = EncodedQuad;

    fn next(&mut self) -> Option<EncodedQuad> {
        loop {
            // What the heap holds may go out once no later input can sort before it.
            if let Some(Reverse(first)) = self.pending.peek() {
                let (pattern, permutation) = (&self.pattern, self.permutation);
                let safe = match self.input.peek() {
                    None => true,
                    Some(next) => *first <= Self::least_key(pattern, permutation, next),
                };
                if safe {
                    let Reverse(key) = self.pending.pop().expect("peeked");
                    return Some(self.permutation.key_to_quad(&key));
                }
            }
            if !self.pull() && self.pending.is_empty() {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(s: u64, p: u64, o: u64) -> EncodedQuad {
        EncodedQuad::from_components([s, p, o, TermId::DEFAULT_GRAPH.raw()])
    }

    fn classes(groups: &[&[u64]]) -> Classes {
        let mut classes = Classes::default();
        for group in groups {
            for &alias in &group[1..] {
                classes.aliases.insert(alias, group[0]);
            }
            classes
                .members
                .insert(group[0], group.to_vec().into_boxed_slice());
        }
        classes
    }

    #[test]
    fn expansion_keeps_the_scan_order() {
        let classes = Arc::new(classes(&[&[10, 30], &[20, 25, 40]]));
        // Stored over representatives, in SPOG order; `30 p 1` is an alias's fact.
        let stored = vec![
            quad(10, 5, 20),
            quad(10, 6, 1),
            quad(20, 5, 10),
            quad(30, 5, 1),
        ];
        let version = Version::empty();
        let expanded: Vec<EncodedQuad> = Expand::new(
            stored.into_iter(),
            Arc::clone(&classes),
            QuadPattern::all(),
            Permutation::Spog,
            true,
            ReadModel::Materialised,
            &version,
        )
        .collect();
        let keys: Vec<Key> = expanded
            .iter()
            .map(|q| Permutation::Spog.to_key(q))
            .collect();
        assert!(keys.is_sorted(), "{keys:?}");
        // 10 5 20: 2 × 3 copies; 10 6 1: 2; 20 5 10: 3 × 2; the alias fact: none.
        assert_eq!(expanded.len(), 6 + 2 + 6);
        // A constant stands for itself.
        let pattern = QuadPattern {
            subject: Some(TermId::from_raw(30)),
            ..QuadPattern::all()
        };
        let stored = vec![quad(10, 5, 20), quad(10, 6, 1)];
        let normalised = classes.normalised(&pattern);
        assert_eq!(normalised.subject, Some(TermId::from_raw(10)));
        let expanded: Vec<EncodedQuad> = Expand::new(
            stored.into_iter(),
            Arc::clone(&classes),
            pattern,
            Permutation::Spog,
            true,
            ReadModel::Materialised,
            &version,
        )
        .collect();
        assert_eq!(expanded.len(), 3 + 1);
        assert!(expanded.iter().all(|q| q.subject.raw() == 30));
        assert_eq!(classes.multiplicity(&quad(10, 5, 20), &pattern), 3);
    }
}
