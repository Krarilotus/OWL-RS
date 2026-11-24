//! Datatype and anonymous-individual restrictions over the retained axiom closure.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{Axiom, DataRange, Diagnostic, Ontology, RangeId, Term, Terms};

pub(crate) struct Membership<'a> {
    ontology: &'a Ontology,
    terms: &'a dyn Terms,
    pub(crate) axiom: usize,
    pub(crate) occurs: BTreeMap<Term, usize>,
    definitions: BTreeMap<Term, Vec<(RangeId, usize)>>,
    found: Vec<Diagnostic>,
}

impl<'a> Membership<'a> {
    pub(crate) fn new(ontology: &'a Ontology, terms: &'a dyn Terms) -> Self {
        Self {
            ontology,
            terms,
            axiom: 0,
            occurs: BTreeMap::new(),
            definitions: BTreeMap::new(),
            found: Vec::new(),
        }
    }

    pub(crate) fn report(&mut self, term: Term, what: &'static str) {
        self.found.push(Diagnostic::GlobalRestriction {
            term,
            axiom: self.axiom,
            what,
        });
    }

    pub(crate) fn definition(&mut self, term: Term, range: RangeId) {
        let definitions = self.definitions.entry(term).or_default();
        let duplicate = !definitions.is_empty();
        definitions.push((range, self.axiom));
        if duplicate {
            self.report(term, "multiple definitions of a datatype");
        }
    }

    pub(crate) fn finish(mut self) -> Vec<Diagnostic> {
        if !self.occurs.is_empty() {
            let builtin: HashSet<_> = crate::vocab::DATA_IRIS
                .iter()
                .filter(|iri| !crate::mapping::FACETS.contains(iri))
                .filter_map(|iri| self.terms.iri(iri))
                .collect();
            for (term, witness) in std::mem::take(&mut self.occurs) {
                if builtin.contains(&term) {
                    if let Some(definitions) = self.definitions.get(&term) {
                        self.axiom = definitions[0].1;
                        self.report(term, "definition of a built-in datatype");
                    }
                } else if !self.definitions.contains_key(&term) {
                    self.axiom = witness;
                    self.report(term, "undefined datatype");
                }
            }
            self.cycles();
        }
        anonymous(self.ontology, &mut self.found);
        self.found
    }

    /// Three-colour DFS over datatype and range nodes. Each shared range is expanded once.
    fn cycles(&mut self) {
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        enum Node {
            Datatype(Term),
            Range(RangeId),
        }
        let mut colors = HashMap::new();
        let mut stack = Vec::new();
        for (&datatype, definitions) in &self.definitions {
            stack.push((
                Node::Datatype(datatype),
                false,
                (datatype, definitions[0].1),
            ));
            while let Some((node, leaving, (term, axiom))) = stack.pop() {
                if leaving {
                    colors.insert(node, false);
                    continue;
                }
                if let Some(&active) = colors.get(&node) {
                    if active {
                        self.found.push(Diagnostic::GlobalRestriction {
                            term,
                            axiom,
                            what: "cyclic datatype definition",
                        });
                    }
                    continue;
                }
                colors.insert(node, true);
                stack.push((node, true, (term, axiom)));
                match node {
                    Node::Datatype(t) => stack.extend(
                        self.definitions
                            .get(&t)
                            .into_iter()
                            .flatten()
                            .map(|&(r, i)| (Node::Range(r), false, (t, i))),
                    ),
                    Node::Range(r) => match self.ontology.range(r) {
                        DataRange::Datatype(t) | DataRange::Restriction(t, _) => {
                            stack.push((Node::Datatype(*t), false, (term, axiom)))
                        }
                        DataRange::And(rs) | DataRange::Or(rs) => {
                            stack.extend(rs.iter().map(|&r| (Node::Range(r), false, (term, axiom))))
                        }
                        DataRange::Not(r) => stack.push((Node::Range(*r), false, (term, axiom))),
                        DataRange::Literal | DataRange::OneOf(_) => {}
                    },
                }
            }
        }
    }
}

/// The anonymous individual graph: union by size and path halving.
fn root(sets: &mut HashMap<Term, (Term, usize, Option<usize>)>, mut node: Term) -> Term {
    sets.entry(node).or_insert((node, 1, None));
    loop {
        let parent = sets[&node].0;
        if parent == node {
            return node;
        }
        let grandparent = sets[&parent].0;
        sets.get_mut(&node).expect("registered node").0 = grandparent;
        node = grandparent;
    }
}

fn anonymous(ontology: &Ontology, found: &mut Vec<Diagnostic>) {
    if ontology.anonymous.is_empty() {
        return;
    }
    let mut emit =
        |node, what, axioms| found.push(Diagnostic::AnonymousIndividual { node, what, axioms });
    let mut sets = HashMap::new();
    let mut pairs = HashMap::new();
    let mut named = HashMap::<Term, [Option<usize>; 2]>::new();
    let blank = |t| ontology.anonymous.contains(&t);
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        let (individuals, what): (&[Term], _) = match axiom {
            Axiom::SameIndividual(ts) => (ts, "anonymous individual in SameIndividual"),
            Axiom::DifferentIndividuals(ts) => (ts, "anonymous individual in DifferentIndividuals"),
            Axiom::NegativeObjectPropertyAssertion(_, a, b) => {
                for &t in [a, b].into_iter().filter(|&&t| blank(t)) {
                    emit(
                        t,
                        "anonymous individual in a negative assertion",
                        [Some(index), None],
                    );
                }
                continue;
            }
            Axiom::NegativeDataPropertyAssertion(_, a, _) => (
                std::slice::from_ref(a),
                "anonymous individual in a negative assertion",
            ),
            Axiom::ObjectPropertyAssertion(_, a, b) => {
                match (blank(*a), blank(*b)) {
                    (true, true) => {
                        let pair = (*a.min(b), *a.max(b));
                        match pairs.entry(pair) {
                            std::collections::hash_map::Entry::Occupied(first) => {
                                emit(
                                    *a,
                                    "multiple assertions between anonymous individuals",
                                    [Some(*first.get()), Some(index)],
                                );
                                continue;
                            }
                            std::collections::hash_map::Entry::Vacant(new) => {
                                new.insert(index);
                            }
                        }
                        let (mut x, mut y) = (root(&mut sets, *a), root(&mut sets, *b));
                        if x == y {
                            emit(*a, "anonymous individual cycle", [sets[&x].2, Some(index)]);
                        } else {
                            if sets[&x].1 < sets[&y].1 {
                                std::mem::swap(&mut x, &mut y);
                            }
                            let (_, size, first) = sets[&y];
                            sets.get_mut(&y).expect("registered root").0 = x;
                            let parent = sets.get_mut(&x).expect("registered root");
                            parent.1 += size;
                            parent.2 = first.into_iter().chain(parent.2).min().or(Some(index));
                        }
                    }
                    (true, false) | (false, true) => {
                        let node = if blank(*a) { a } else { b };
                        let attachments = named.entry(*node).or_default();
                        attachments[usize::from(attachments[0].is_some())] = Some(index);
                    }
                    (false, false) => {}
                }
                continue;
            }
            _ => continue,
        };
        for &node in individuals.iter().filter(|&&t| blank(t)) {
            emit(node, what, [Some(index), None]);
        }
    }
    let mut trees = BTreeMap::new();
    for &node in &ontology.anonymous {
        let attachments = named.get(&node).copied().unwrap_or_default();
        let tree = trees.entry(root(&mut sets, node)).or_insert(attachments);
        if attachments[1].is_none() {
            *tree = [None; 2];
        }
    }
    for (node, axioms) in trees {
        if axioms[1].is_some() {
            emit(
                node,
                "anonymous tree has no root with at most one named attachment",
                axioms,
            );
        }
    }
}
