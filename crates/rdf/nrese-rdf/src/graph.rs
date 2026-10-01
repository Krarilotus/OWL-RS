//! In-memory graphs and datasets: sets of triples and quads in a deterministic order,
//! with blank node canonicalisation to compare them up to isomorphism.

use std::collections::BTreeSet;
use std::collections::btree_set;
use std::fmt;
use std::ops::Bound;

use crate::term::{
    GraphName, GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Term, TermRef,
};
use crate::triple::{Quad, QuadRef, Triple, TripleRef};

/// A set of triples, ordered subject–predicate–object.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Graph {
    triples: BTreeSet<Triple>,
}

/// The least term in the order of [`Term`].
fn least_term() -> Term {
    Term::NamedNode(NamedNode::new_unchecked(""))
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the triple is new.
    pub fn insert<'a>(&mut self, triple: impl Into<TripleRef<'a>>) -> bool {
        self.triples.insert(triple.into().into_owned())
    }

    /// Whether the triple was there.
    pub fn remove<'a>(&mut self, triple: impl Into<TripleRef<'a>>) -> bool {
        self.triples.remove(&triple.into().into_owned())
    }

    pub fn contains<'a>(&self, triple: impl Into<TripleRef<'a>>) -> bool {
        self.triples.contains(&triple.into().into_owned())
    }

    pub fn len(&self) -> usize {
        self.triples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }

    pub fn clear(&mut self) {
        self.triples.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = TripleRef<'_>> + '_ {
        self.triples.iter().map(Triple::as_ref)
    }

    /// The triples with this subject, by a range scan.
    pub fn triples_for_subject<'a>(
        &'a self,
        subject: impl Into<NamedOrBlankNodeRef<'a>>,
    ) -> impl Iterator<Item = TripleRef<'a>> + 'a {
        let subject = subject.into().into_owned();
        let start = Triple {
            subject: subject.clone(),
            predicate: NamedNode::new_unchecked(""),
            object: least_term(),
        };
        self.triples
            .range((Bound::Included(start), Bound::Unbounded))
            .take_while(move |t| t.subject == subject)
            .map(Triple::as_ref)
    }

    /// The objects of this subject and predicate, by a range scan.
    pub fn objects_for_subject_predicate<'a>(
        &'a self,
        subject: impl Into<NamedOrBlankNodeRef<'a>>,
        predicate: impl Into<NamedNodeRef<'a>>,
    ) -> impl Iterator<Item = TermRef<'a>> + 'a {
        let subject = subject.into().into_owned();
        let predicate = predicate.into().into_owned();
        let start = Triple {
            subject: subject.clone(),
            predicate: predicate.clone(),
            object: least_term(),
        };
        self.triples
            .range((Bound::Included(start), Bound::Unbounded))
            .take_while(move |t| t.subject == subject && t.predicate == predicate)
            .map(|t| t.object.as_ref())
    }

    pub fn object_for_subject_predicate<'a>(
        &'a self,
        subject: impl Into<NamedOrBlankNodeRef<'a>>,
        predicate: impl Into<NamedNodeRef<'a>>,
    ) -> Option<TermRef<'a>> {
        self.objects_for_subject_predicate(subject, predicate)
            .next()
    }

    /// The subjects with this predicate and object (a full scan).
    pub fn subjects_for_predicate_object<'a>(
        &'a self,
        predicate: impl Into<NamedNodeRef<'a>>,
        object: impl Into<TermRef<'a>>,
    ) -> impl Iterator<Item = NamedOrBlankNodeRef<'a>> + 'a {
        let predicate = predicate.into();
        let object = object.into();
        self.iter()
            .filter(move |t| t.predicate == predicate && t.object == object)
            .map(|t| t.subject)
    }

    /// Renames the blank nodes so that isomorphic graphs become equal (see
    /// [`Dataset::canonicalize`]).
    pub fn canonicalize(&mut self) {
        let quads: BTreeSet<Quad> = std::mem::take(&mut self.triples)
            .into_iter()
            .map(|t| t.in_graph(GraphName::DefaultGraph))
            .collect();
        self.triples = crate::canonical::canonical(quads)
            .into_iter()
            .map(Triple::from)
            .collect();
    }
}

impl<'a> IntoIterator for &'a Graph {
    type Item = TripleRef<'a>;
    type IntoIter = std::iter::Map<btree_set::Iter<'a, Triple>, fn(&Triple) -> TripleRef<'_>>;

    fn into_iter(self) -> Self::IntoIter {
        self.triples.iter().map(Triple::as_ref)
    }
}

impl IntoIterator for Graph {
    type Item = Triple;
    type IntoIter = btree_set::IntoIter<Triple>;

    fn into_iter(self) -> Self::IntoIter {
        self.triples.into_iter()
    }
}

impl FromIterator<Triple> for Graph {
    fn from_iter<I: IntoIterator<Item = Triple>>(iter: I) -> Self {
        Self {
            triples: iter.into_iter().collect(),
        }
    }
}

impl<'a> FromIterator<TripleRef<'a>> for Graph {
    fn from_iter<I: IntoIterator<Item = TripleRef<'a>>>(iter: I) -> Self {
        iter.into_iter().map(TripleRef::into_owned).collect()
    }
}

impl Extend<Triple> for Graph {
    fn extend<I: IntoIterator<Item = Triple>>(&mut self, iter: I) {
        self.triples.extend(iter);
    }
}

impl<'a> Extend<TripleRef<'a>> for Graph {
    fn extend<I: IntoIterator<Item = TripleRef<'a>>>(&mut self, iter: I) {
        self.triples
            .extend(iter.into_iter().map(TripleRef::into_owned));
    }
}

/// N-Triples.
impl fmt::Display for Graph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for triple in &self.triples {
            writeln!(f, "{triple} .")?;
        }
        Ok(())
    }
}

/// A set of quads, ordered subject–predicate–object–graph.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Dataset {
    quads: BTreeSet<Quad>,
}

impl Dataset {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the quad is new.
    pub fn insert<'a>(&mut self, quad: impl Into<QuadRef<'a>>) -> bool {
        self.quads.insert(quad.into().into_owned())
    }

    /// Whether the quad was there.
    pub fn remove<'a>(&mut self, quad: impl Into<QuadRef<'a>>) -> bool {
        self.quads.remove(&quad.into().into_owned())
    }

    pub fn contains<'a>(&self, quad: impl Into<QuadRef<'a>>) -> bool {
        self.quads.contains(&quad.into().into_owned())
    }

    pub fn len(&self) -> usize {
        self.quads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }

    pub fn clear(&mut self) {
        self.quads.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = QuadRef<'_>> + '_ {
        self.quads.iter().map(Quad::as_ref)
    }

    /// The triples of one graph (a full scan).
    pub fn graph<'a>(
        &'a self,
        graph_name: impl Into<GraphNameRef<'a>>,
    ) -> impl Iterator<Item = TripleRef<'a>> + 'a {
        let graph_name = graph_name.into();
        self.iter()
            .filter(move |q| q.graph_name == graph_name)
            .map(TripleRef::from)
    }

    /// Renames the blank nodes so that isomorphic datasets become equal (see
    /// [`crate::canonical`]: components, colour refinement, individualisation with twins
    /// pruned). The names are stable only within one build.
    pub fn canonicalize(&mut self) {
        self.quads = crate::canonical::canonical(std::mem::take(&mut self.quads));
    }
}

impl<'a> IntoIterator for &'a Dataset {
    type Item = QuadRef<'a>;
    type IntoIter = std::iter::Map<btree_set::Iter<'a, Quad>, fn(&Quad) -> QuadRef<'_>>;

    fn into_iter(self) -> Self::IntoIter {
        self.quads.iter().map(Quad::as_ref)
    }
}

impl IntoIterator for Dataset {
    type Item = Quad;
    type IntoIter = btree_set::IntoIter<Quad>;

    fn into_iter(self) -> Self::IntoIter {
        self.quads.into_iter()
    }
}

impl FromIterator<Quad> for Dataset {
    fn from_iter<I: IntoIterator<Item = Quad>>(iter: I) -> Self {
        Self {
            quads: iter.into_iter().collect(),
        }
    }
}

impl<'a> FromIterator<QuadRef<'a>> for Dataset {
    fn from_iter<I: IntoIterator<Item = QuadRef<'a>>>(iter: I) -> Self {
        iter.into_iter().map(QuadRef::into_owned).collect()
    }
}

impl Extend<Quad> for Dataset {
    fn extend<I: IntoIterator<Item = Quad>>(&mut self, iter: I) {
        self.quads.extend(iter);
    }
}

impl<'a> Extend<QuadRef<'a>> for Dataset {
    fn extend<I: IntoIterator<Item = QuadRef<'a>>>(&mut self, iter: I) {
        self.quads.extend(iter.into_iter().map(QuadRef::into_owned));
    }
}

/// N-Quads.
impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for quad in &self.quads {
            writeln!(f, "{quad} .")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Literal;
    use crate::term::{BlankNode, NamedOrBlankNode};

    fn n(iri: &str) -> NamedNode {
        NamedNode::new_unchecked(format!("http://e/{iri}"))
    }

    fn b(id: &str) -> BlankNode {
        BlankNode::new_unchecked(id)
    }

    fn graph(triples: &[(&str, &str, &str)]) -> Graph {
        let node = |t: &str| -> Term {
            match t.strip_prefix("_:") {
                Some(id) => b(id).into(),
                None => n(t).into(),
            }
        };
        triples
            .iter()
            .map(|(s, p, o)| {
                Triple::new(NamedOrBlankNode::try_from(node(s)).unwrap(), n(p), node(o))
            })
            .collect()
    }

    fn isomorphic(left: &Graph, right: &Graph) -> bool {
        let (mut left, mut right) = (left.clone(), right.clone());
        left.canonicalize();
        right.canonicalize();
        left == right
    }

    #[test]
    fn lookups_scan_ranges() {
        let g = graph(&[
            ("a", "p", "x"),
            ("a", "p", "y"),
            ("a", "q", "z"),
            ("b", "p", "x"),
        ]);
        let objects: Vec<String> = g
            .objects_for_subject_predicate(&n("a"), &n("p"))
            .map(|t| t.to_string())
            .collect();
        assert_eq!(objects, ["<http://e/x>", "<http://e/y>"]);
        assert_eq!(g.triples_for_subject(&n("a")).count(), 3);
        assert_eq!(
            g.subjects_for_predicate_object(&n("p"), &Term::from(n("x")))
                .count(),
            2
        );
        assert!(g.object_for_subject_predicate(&n("c"), &n("p")).is_none());
        assert_eq!(g.to_string().lines().count(), 4);
    }

    #[test]
    fn canonicalisation_ignores_blank_node_names() {
        let left = graph(&[("_:x", "p", "_:y"), ("_:y", "p", "a"), ("_:x", "q", "b")]);
        let right = graph(&[("_:m", "p", "_:n"), ("_:n", "p", "a"), ("_:m", "q", "b")]);
        let other = graph(&[("_:m", "p", "_:n"), ("_:n", "p", "b"), ("_:m", "q", "a")]);
        assert!(isomorphic(&left, &right));
        assert!(!isomorphic(&left, &other));
    }

    #[test]
    fn canonicalisation_breaks_symmetry() {
        // Two 3-cycles against one 6-cycle: every node looks alike to colour refinement.
        let two = graph(&[
            ("_:a", "p", "_:b"),
            ("_:b", "p", "_:c"),
            ("_:c", "p", "_:a"),
            ("_:d", "p", "_:e"),
            ("_:e", "p", "_:f"),
            ("_:f", "p", "_:d"),
        ]);
        let two_renamed = graph(&[
            ("_:1", "p", "_:5"),
            ("_:5", "p", "_:3"),
            ("_:3", "p", "_:1"),
            ("_:2", "p", "_:6"),
            ("_:6", "p", "_:4"),
            ("_:4", "p", "_:2"),
        ]);
        let six = graph(&[
            ("_:a", "p", "_:b"),
            ("_:b", "p", "_:c"),
            ("_:c", "p", "_:d"),
            ("_:d", "p", "_:e"),
            ("_:e", "p", "_:f"),
            ("_:f", "p", "_:a"),
        ]);
        assert!(isomorphic(&two, &two_renamed));
        assert!(!isomorphic(&two, &six));
        let mut canonical = two.clone();
        canonical.canonicalize();
        assert_eq!(canonical.len(), 6);
    }

    #[test]
    fn datasets_canonicalise_graph_names_too() {
        let quad = |g: &str, o: &str| {
            Quad::new(
                b("s"),
                n("p"),
                Literal::new_simple_literal(o),
                GraphName::from(b(g)),
            )
        };
        let mut left: Dataset = [quad("g1", "a"), quad("g2", "b")].into_iter().collect();
        let mut right: Dataset = [quad("h2", "a"), quad("h1", "b")].into_iter().collect();
        left.canonicalize();
        right.canonicalize();
        assert_eq!(left, right);
        assert_eq!(left.graph(GraphNameRef::DefaultGraph).count(), 0);
    }
}
