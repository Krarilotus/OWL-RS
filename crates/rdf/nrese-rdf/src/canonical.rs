//! Blank node canonicalisation: renames the blank nodes of a set of quads so that
//! isomorphic sets become equal.
//!
//! 1. **Components.** Blank nodes that share a quad are connected; each connected component
//!    is canonicalised on its own, and the components are then ordered by their canonical
//!    form. Isomorphic components get the same form and an index, which is canonical
//!    because they are interchangeable. Disjoint symmetric parts (five equal cycles) don't
//!    multiply each other's search.
//! 2. **Refinement.** Each blank node's colour is rehashed with the colours and terms around
//!    it until the partition is stable (colour refinement, as in Hogan, "Canonical Forms
//!    for Isomorphic and Equivalent RDF Graphs", 2017). Colours hash term *contents*, never
//!    input labels or order.
//! 3. **Individualisation.** Where a class stays tied, one member gets a distinct colour and
//!    refinement runs again; each choice is tried and the least result kept. Twins, nodes
//!    whose swap maps the quads onto themselves, lead to the same result, so only one per
//!    group of twins is tried: a node with a thousand equal blank children costs linear time,
//!    not a thousand factorial.
//! 4. **Automorphisms** (as nauty and bliss prune): symmetric parts that aren't twins (a
//!    node and its formula, swapped together with another such pair) would still branch
//!    factorially. When a leaf of the search has the same form as the best leaf, the map
//!    between the two is an automorphism: the search goes straight back to the node where
//!    their paths parted, and every choice there in the same orbit is skipped. Symmetric
//!    structures cost about one path per member instead of a path per permutation.
//! 5. **Modules** (component recursion, as in bliss): once refinement has told some nodes
//!    apart, the tied rest may fall into parts that share no quad (the formulas that share
//!    only a told-apart node). Each part is searched on its own, and equal parts are ranked
//!    by their form: no branching across them at all. A node alone in its colour keeps it
//!    during refinement, so a choice in one part can't reach another through it.
//!
//! The names depend on the hash function, so they are stable only within one build.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, Hash};

use crate::term::{BlankNode, GraphName, NamedOrBlankNode, Term};
use crate::triple::Quad;

/// A fixed seed: colours must be the same in every run (foldhash, much faster than SipHash
/// on the many small values refinement hashes).
const HASHER: foldhash::fast::FixedState =
    foldhash::fast::FixedState::with_seed(0x6e72_6573_652d_7264);

fn hash_of(value: &impl Hash) -> u64 {
    HASHER.hash_one(value)
}

/// A position of a quad (subject, object, graph name): a blank node of the component (by
/// local index) or another term (by an exact id, and the hash of its content).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    Blank(usize),
    Fixed(u32),
}

/// A quad of a component, exactly: the three slots and the predicate's id.
type Key = ([Slot; 3], u32);

/// A component's canonical form, and each of its blank nodes with its colour.
type Form<'a> = (BTreeSet<Quad>, Vec<(&'a BlankNode, u64)>);

/// The quads of one component as they are collected, with its blank nodes numbered.
#[derive(Default)]
struct Group<'a> {
    quads: Vec<&'a Quad>,
    slots: Vec<[Slot; 3]>,
    predicates: Vec<u32>,
    local: HashMap<usize, usize>,
    blank_nodes: Vec<&'a BlankNode>,
}

impl<'a> Group<'a> {
    /// The component's number for blank node `id`.
    fn local(&mut self, id: usize, node: &'a BlankNode) -> usize {
        let next = self.local.len();
        *self.local.entry(id).or_insert_with(|| {
            self.blank_nodes.push(node);
            next
        })
    }
}

/// One connected component of blank nodes and the quads that hold them.
struct Component<'a> {
    quads: Vec<&'a Quad>,
    keys: Vec<Key>,
    /// The content hash of each fixed term id (predicates included).
    fixed_hash: &'a [u64],
    /// Per blank node the quads it is in.
    incident: Vec<Vec<usize>>,
    blank_nodes: Vec<&'a BlankNode>,
    present: HashSet<Key>,
}

/// The quads with their blank nodes renamed canonically.
pub(crate) fn canonical(quads: BTreeSet<Quad>) -> BTreeSet<Quad> {
    // Exact ids for every other term, with content hashes for colouring.
    let mut fixed: HashMap<Term, u32> = HashMap::new();
    let mut fixed_hash: Vec<u64> = Vec::new();
    let mut intern = |term: Term| -> u32 {
        let next = fixed.len() as u32;
        *fixed.entry(term.clone()).or_insert_with(|| {
            fixed_hash.push(hash_of(&term));
            next
        })
    };
    // The blank nodes, numbered, and the components by union–find.
    let mut index: HashMap<&BlankNode, usize> = HashMap::new();
    let mut blank_nodes: Vec<&BlankNode> = Vec::new();
    let mut parent: Vec<usize> = Vec::new();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut result = BTreeSet::new();
    let mut with_blanks: Vec<(&Quad, [Option<usize>; 3])> = Vec::new();
    for quad in &quads {
        let blanks = [
            match &quad.subject {
                NamedOrBlankNode::BlankNode(b) => Some(b),
                NamedOrBlankNode::NamedNode(_) => None,
            },
            match &quad.object {
                Term::BlankNode(b) => Some(b),
                _ => None,
            },
            match &quad.graph_name {
                GraphName::BlankNode(b) => Some(b),
                _ => None,
            },
        ];
        if blanks.iter().all(Option::is_none) {
            result.insert(quad.clone());
            continue;
        }
        let ids = blanks.map(|b| {
            b.map(|b| {
                *index.entry(b).or_insert_with(|| {
                    blank_nodes.push(b);
                    parent.push(parent.len());
                    parent.len() - 1
                })
            })
        });
        let present: Vec<usize> = ids.iter().flatten().copied().collect();
        for pair in present.windows(2) {
            let (a, b) = (find(&mut parent, pair[0]), find(&mut parent, pair[1]));
            if a != b {
                parent[a] = b;
            }
        }
        with_blanks.push((quad, ids));
    }
    if blank_nodes.is_empty() {
        return result;
    }
    // The quads of each component, with slots numbered within it.
    let mut groups: HashMap<usize, Group<'_>> = HashMap::new();
    for (quad, ids) in &with_blanks {
        let root = find(
            &mut parent,
            ids.iter().flatten().next().copied().unwrap_or(0),
        );
        let group = groups.entry(root).or_default();
        let subject = match ids[0] {
            Some(id) => Slot::Blank(group.local(id, blank_nodes[id])),
            None => Slot::Fixed(intern(Term::from(quad.subject.clone()))),
        };
        let object = match ids[1] {
            Some(id) => Slot::Blank(group.local(id, blank_nodes[id])),
            None => Slot::Fixed(intern(quad.object.clone())),
        };
        let graph = match (ids[2], &quad.graph_name) {
            (Some(id), _) => Slot::Blank(group.local(id, blank_nodes[id])),
            (None, GraphName::NamedNode(n)) => Slot::Fixed(intern(n.clone().into())),
            // The default graph: a term no quad can hold.
            _ => Slot::Fixed(intern(Term::NamedNode(
                crate::term::NamedNode::new_unchecked(""),
            ))),
        };
        group.quads.push(quad);
        group.slots.push([subject, object, graph]);
        group.predicates.push(intern(quad.predicate.clone().into()));
    }
    // Each component canonically, then ordered by its canonical form.
    let mut forms: Vec<Form<'_>> = groups
        .into_values()
        .map(
            |Group {
                 quads,
                 slots,
                 predicates,
                 blank_nodes,
                 ..
             }| {
                let mut incident = vec![Vec::new(); blank_nodes.len()];
                for (q, quad_slots) in slots.iter().enumerate() {
                    for slot in quad_slots {
                        if let Slot::Blank(b) = *slot
                            && incident[b].last() != Some(&q)
                        {
                            incident[b].push(q);
                        }
                    }
                }
                let keys: Vec<Key> = slots
                    .iter()
                    .zip(&predicates)
                    .map(|(s, &p)| (*s, p))
                    .collect();
                let component = Component {
                    present: keys.iter().copied().collect(),
                    quads,
                    keys,
                    fixed_hash: &fixed_hash,
                    incident,
                    blank_nodes,
                };
                let colours = component.search(vec![0; component.blank_nodes.len()]);
                (component.relabel(&colours, "p"), component.named(&colours))
            },
        )
        .collect();
    forms.sort_by(|a, b| a.0.cmp(&b.0));
    // The final names: the component's place in that order, and the node's colour.
    let mut name_of: HashMap<&BlankNode, BlankNode> = HashMap::new();
    for (i, (_, names)) in forms.iter().enumerate() {
        for &(b, colour) in names {
            name_of.insert(b, BlankNode::new_unchecked(format!("c{i}x{colour:016x}")));
        }
    }
    for (quad, _) in &with_blanks {
        let rename = |b: &BlankNode| name_of[b].clone();
        result.insert(Quad {
            subject: match &quad.subject {
                NamedOrBlankNode::BlankNode(b) => rename(b).into(),
                other => other.clone(),
            },
            predicate: quad.predicate.clone(),
            object: match &quad.object {
                Term::BlankNode(b) => rename(b).into(),
                other => other.clone(),
            },
            graph_name: match &quad.graph_name {
                GraphName::BlankNode(b) => rename(b).into(),
                other => other.clone(),
            },
        });
    }
    result
}

impl<'a> Component<'a> {
    /// Rehashes each blank node with its neighbourhood until the partition is stable.
    /// A node alone in its colour keeps it (it is told apart already, and its colour then
    /// carries nothing from one part of the component to another), and so does a node
    /// outside `scope`, when only part of the component is being searched.
    fn refine(&self, mut colours: Vec<u64>, scope: Option<&[bool]>) -> Vec<u64> {
        let mut classes = distinct(&colours);
        // Reused across nodes and rounds: no allocation per node.
        let mut signatures: Vec<u64> = Vec::new();
        let mut next: Vec<u64> = vec![0; colours.len()];
        let mut sizes: HashMap<u64, u32> = HashMap::new();
        loop {
            sizes.clear();
            for &c in &colours {
                *sizes.entry(c).or_default() += 1;
            }
            for (b, slot) in next.iter_mut().enumerate() {
                if sizes[&colours[b]] == 1 || scope.is_some_and(|s| !s[b]) {
                    *slot = colours[b];
                    continue;
                }
                signatures.clear();
                signatures.extend(self.incident[b].iter().map(|&q| {
                    let (slots, predicate) = self.keys[q];
                    let positions = slots.map(|slot| match slot {
                        Slot::Blank(other) if other == b => (0_u8, 0),
                        Slot::Blank(other) => (1, colours[other]),
                        Slot::Fixed(id) => (2, self.fixed_hash[id as usize]),
                    });
                    hash_of(&(self.fixed_hash[predicate as usize], positions))
                }));
                signatures.sort_unstable();
                *slot = hash_of(&(colours[b], signatures.as_slice()));
            }
            let next_classes = distinct(&next);
            std::mem::swap(&mut colours, &mut next);
            if next_classes == classes {
                return colours;
            }
            classes = next_classes;
        }
    }

    /// Whether swapping blank nodes `a` and `b` maps the component's quads onto themselves.
    fn twins(&self, a: usize, b: usize) -> bool {
        let swap = |slot: Slot| match slot {
            Slot::Blank(x) if x == a => Slot::Blank(b),
            Slot::Blank(x) if x == b => Slot::Blank(a),
            other => other,
        };
        self.incident[a].iter().chain(&self.incident[b]).all(|&q| {
            let (slots, predicate) = self.keys[q];
            self.present.contains(&(slots.map(swap), predicate))
        })
    }

    /// The colours of the least labelling (see the module's documentation).
    fn search(&self, colours: Vec<u64>) -> Vec<u64> {
        let mut search = Search::new(self, None);
        search.node(colours, &mut Vec::new());
        search.best.map(|leaf| leaf.colours).unwrap_or_default()
    }

    /// The members of the class to split next (the smallest tied one, then the least
    /// colour), or `None` if every colour in `scope` is distinct.
    fn tied_class(colours: &[u64], scope: Option<&[bool]>) -> Option<Vec<usize>> {
        let mut classes: HashMap<u64, Vec<usize>> = HashMap::new();
        for (b, &c) in colours.iter().enumerate() {
            if scope.is_none_or(|s| s[b]) {
                classes.entry(c).or_default().push(b);
            }
        }
        classes
            .into_iter()
            .filter(|(_, members)| members.len() > 1)
            .min_by_key(|(c, members)| (members.len(), *c))
            .map(|(_, members)| members)
    }

    /// The tied nodes of `scope` in parts that share no quad, once the nodes alone in
    /// their colour are set aside; `None` unless there are at least two parts.
    fn modules(&self, colours: &[u64], scope: Option<&[bool]>) -> Option<Vec<Vec<usize>>> {
        let mut sizes: HashMap<u64, u32> = HashMap::new();
        for &c in colours {
            *sizes.entry(c).or_default() += 1;
        }
        let tied: Vec<bool> = (0..colours.len())
            .map(|b| sizes[&colours[b]] > 1 && scope.is_none_or(|s| s[b]))
            .collect();
        let mut parent: Vec<usize> = (0..colours.len()).collect();
        for (slots, _) in &self.keys {
            let mut first = None;
            for slot in slots {
                if let Slot::Blank(b) = *slot
                    && tied[b]
                {
                    match first {
                        None => first = Some(b),
                        Some(a) => {
                            let (x, y) = (find(&mut parent, a), find(&mut parent, b));
                            parent[x] = y;
                        }
                    }
                }
            }
        }
        let mut parts: HashMap<usize, Vec<usize>> = HashMap::new();
        for b in (0..colours.len()).filter(|&b| tied[b]) {
            parts.entry(find(&mut parent, b)).or_default().push(b);
        }
        (parts.len() > 1).then(|| parts.into_values().collect())
    }

    /// The form of the quads that hold a node of `module`, under `colours`.
    fn module_certificate(&self, colours: &[u64], module: &[usize]) -> Vec<[u64; 4]> {
        let mut quads: Vec<usize> = module
            .iter()
            .flat_map(|&b| self.incident[b].iter().copied())
            .collect();
        quads.sort_unstable();
        quads.dedup();
        let mut certificate: Vec<[u64; 4]> = quads
            .iter()
            .map(|&q| self.certificate_row(colours, q))
            .collect();
        certificate.sort_unstable();
        certificate
    }

    fn certificate_row(&self, colours: &[u64], q: usize) -> [u64; 4] {
        let slot = |s: Slot| match s {
            Slot::Blank(b) => colours[b] << 1,
            Slot::Fixed(id) => (self.fixed_hash[id as usize] << 1) | 1,
        };
        let (slots, predicate) = self.keys[q];
        [
            slot(slots[0]),
            self.fixed_hash[predicate as usize],
            slot(slots[1]),
            slot(slots[2]),
        ]
    }

    /// A form of the quads under a colouring that compares as the relabelled quads would
    /// (blank nodes by colour, other terms by content), without building them.
    fn certificate(&self, colours: &[u64]) -> Vec<[u64; 4]> {
        let mut certificate: Vec<[u64; 4]> = (0..self.keys.len())
            .map(|q| self.certificate_row(colours, q))
            .collect();
        certificate.sort_unstable();
        certificate
    }

    /// The component's quads with each blank node named by its colour.
    fn relabel(&self, colours: &[u64], prefix: &str) -> BTreeSet<Quad> {
        let names: Vec<BlankNode> = colours
            .iter()
            .map(|c| BlankNode::new_unchecked(format!("{prefix}{c:016x}")))
            .collect();
        let name = |slot: Slot| match slot {
            Slot::Blank(b) => Some(&names[b]),
            Slot::Fixed(_) => None,
        };
        self.quads
            .iter()
            .zip(&self.keys)
            .map(|(quad, (slots, _))| Quad {
                subject: name(slots[0]).map_or_else(|| quad.subject.clone(), |b| b.clone().into()),
                predicate: quad.predicate.clone(),
                object: name(slots[1]).map_or_else(|| quad.object.clone(), |b| b.clone().into()),
                graph_name: name(slots[2])
                    .map_or_else(|| quad.graph_name.clone(), |b| b.clone().into()),
            })
            .collect()
    }

    /// Each original blank node with its final colour.
    fn named(&self, colours: &[u64]) -> Vec<(&'a BlankNode, u64)> {
        self.blank_nodes
            .iter()
            .copied()
            .zip(colours.iter().copied())
            .collect()
    }
}

/// A leaf of the search: a colouring discrete in the search's scope, the choices that led
/// to it, and its form.
struct Leaf {
    colours: Vec<u64>,
    path: Vec<usize>,
    certificate: Vec<[u64; 4]>,
}

/// Individualisation and refinement with automorphism pruning and module decomposition,
/// over one component, or one module of it (`scope`).
struct Search<'s, 'a> {
    component: &'s Component<'a>,
    scope: Option<Vec<bool>>,
    best: Option<Leaf>,
    /// Automorphisms found (each maps a blank node to its image).
    automorphisms: Vec<Vec<usize>>,
}

impl<'s, 'a> Search<'s, 'a> {
    fn new(component: &'s Component<'a>, scope: Option<Vec<bool>>) -> Self {
        Self {
            component,
            scope,
            best: None,
            automorphisms: Vec::new(),
        }
    }

    fn in_scope(&self, b: usize) -> bool {
        self.scope.as_ref().is_none_or(|s| s[b])
    }

    /// Searches below the node with `colours`, reached by the choices `path`. Returns the
    /// level to go back to when a leaf proved an automorphism with the best leaf (the level
    /// where their paths parted).
    fn node(&mut self, colours: Vec<u64>, path: &mut Vec<usize>) -> Option<usize> {
        let component = self.component;
        let mut colours = component.refine(colours, self.scope.as_deref());
        loop {
            let Some(members) = Component::tied_class(&colours, self.scope.as_deref()) else {
                return self.leaf(colours, path);
            };
            // Parts that share no quad are canonicalised each on its own.
            if let Some(modules) = component.modules(&colours, self.scope.as_deref()) {
                let combined = self.modules(&colours, modules);
                return self.leaf(combined, path);
            }
            // One representative per group of twins.
            let mut representatives: Vec<usize> = Vec::new();
            for &m in &members {
                if !representatives.iter().any(|&r| component.twins(r, m)) {
                    representatives.push(m);
                }
            }
            if let [only] = representatives[..] {
                // No real choice: follow it without a level of recursion.
                path.push(only);
                colours = component.refine(individualised(&colours, only), self.scope.as_deref());
                continue;
            }
            let level = path.len();
            let mut orbits: Vec<usize> = (0..colours.len()).collect();
            let mut explored: Vec<usize> = Vec::new();
            for &m in &representatives {
                if explored
                    .iter()
                    .any(|&e| find(&mut orbits, e) == find(&mut orbits, m))
                {
                    continue;
                }
                path.push(m);
                let back = self.node(individualised(&colours, m), path);
                path.truncate(level);
                explored.push(m);
                // Automorphisms that keep this node's colouring join its choices' orbits.
                for gamma in &self.automorphisms {
                    if (0..colours.len()).all(|v| colours[v] == colours[gamma[v]]) {
                        for &r in &representatives {
                            let (a, b) = (find(&mut orbits, r), find(&mut orbits, gamma[r]));
                            orbits[a] = b;
                        }
                    }
                }
                if let Some(target) = back
                    && target < level
                {
                    return Some(target);
                }
            }
            return None;
        }
    }

    /// Each module searched on its own; equal modules (swapping them is an automorphism)
    /// told apart by their rank among the modules' forms, which can't depend on labels.
    fn modules(&self, colours: &[u64], modules: Vec<Vec<usize>>) -> Vec<u64> {
        let n = colours.len();
        let mut combined = colours.to_vec();
        let mut forms: Vec<(Vec<[u64; 4]>, Vec<usize>)> = Vec::with_capacity(modules.len());
        for module in modules {
            let mut scope = vec![false; n];
            for &b in &module {
                scope[b] = true;
            }
            let mut sub = Search::new(self.component, Some(scope));
            sub.node(colours.to_vec(), &mut Vec::new());
            let leaf = sub
                .best
                .map(|leaf| leaf.colours)
                .unwrap_or_else(|| colours.to_vec());
            for &b in &module {
                combined[b] = leaf[b];
            }
            forms.push((self.component.module_certificate(&leaf, &module), module));
        }
        forms.sort_by(|a, b| a.0.cmp(&b.0));
        for (rank, (_, module)) in forms.iter().enumerate() {
            for &b in module {
                combined[b] = hash_of(&(combined[b], rank));
            }
        }
        combined
    }

    fn leaf(&mut self, colours: Vec<u64>, path: &[usize]) -> Option<usize> {
        let certificate = self.component.certificate(&colours);
        let Some(best) = &self.best else {
            self.best = Some(Leaf {
                colours,
                path: path.to_vec(),
                certificate,
            });
            return None;
        };
        match certificate.cmp(&best.certificate) {
            std::cmp::Ordering::Less => {
                self.best = Some(Leaf {
                    colours,
                    path: path.to_vec(),
                    certificate,
                });
                None
            }
            std::cmp::Ordering::Greater => None,
            std::cmp::Ordering::Equal => {
                // The same form: the map from the best leaf to this one (by colour, in the
                // scope; the rest stays) is an automorphism.
                let by_colour: HashMap<u64, usize> = (0..colours.len())
                    .filter(|&v| self.in_scope(v))
                    .map(|v| (colours[v], v))
                    .collect();
                let gamma: Option<Vec<usize>> = (0..colours.len())
                    .map(|v| {
                        if self.in_scope(v) {
                            by_colour.get(&best.colours[v]).copied()
                        } else {
                            Some(v)
                        }
                    })
                    .collect();
                let gamma = gamma?;
                let parted = best
                    .path
                    .iter()
                    .zip(path)
                    .position(|(a, b)| a != b)
                    .unwrap_or(path.len().min(best.path.len()));
                // It must map the best path onto this one to stand for the whole subtree.
                let maps_path = best
                    .path
                    .iter()
                    .zip(path)
                    .take(parted + 1)
                    .all(|(&a, &b)| gamma[a] == b);
                self.automorphisms.push(gamma);
                maps_path.then_some(parted)
            }
        }
    }
}

fn individualised(colours: &[u64], member: usize) -> Vec<u64> {
    let mut split = colours.to_vec();
    split[member] = hash_of(&(split[member], "individualised"));
    split
}

/// Union-find: the representative of `x`'s set.
fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn distinct(colours: &[u64]) -> usize {
    colours.iter().collect::<HashSet<_>>().len()
}

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};

    use super::*;
    use crate::term::{Literal, NamedNode};

    fn p(name: &str) -> NamedNode {
        NamedNode::new_unchecked(format!("http://e/{name}"))
    }

    fn quad(s: &str, pred: &str, o: &str) -> Quad {
        Quad::new(
            BlankNode::new_unchecked(s),
            p(pred),
            BlankNode::new_unchecked(o),
            GraphName::DefaultGraph,
        )
    }

    /// The graph with its blank nodes renamed by `rename`.
    fn renamed(quads: &BTreeSet<Quad>, rename: impl Fn(&str) -> String) -> BTreeSet<Quad> {
        let node = |b: &BlankNode| BlankNode::new_unchecked(rename(b.as_str()));
        quads
            .iter()
            .map(|q| Quad {
                subject: match &q.subject {
                    NamedOrBlankNode::BlankNode(b) => node(b).into(),
                    other => other.clone(),
                },
                predicate: q.predicate.clone(),
                object: match &q.object {
                    Term::BlankNode(b) => node(b).into(),
                    other => other.clone(),
                },
                graph_name: match &q.graph_name {
                    GraphName::BlankNode(b) => node(b).into(),
                    other => other.clone(),
                },
            })
            .collect()
    }

    fn timed(quads: BTreeSet<Quad>) -> (BTreeSet<Quad>, std::time::Duration) {
        let start = std::time::Instant::now();
        let result = canonical(quads);
        (result, start.elapsed())
    }

    #[test]
    fn a_long_chain_needs_no_deep_recursion() {
        let chain: BTreeSet<Quad> = (0..2000)
            .map(|i| quad(&format!("n{i}"), "p", &format!("n{}", i + 1)))
            .collect();
        let (a, _) = timed(chain.clone());
        let (b, _) = timed(renamed(&chain, |n| format!("x{n}")));
        assert_eq!(a, b);
        assert_eq!(a.len(), 2000);
    }

    #[test]
    fn many_equal_children_are_twins() {
        // A root with a thousand blank children, each with the same literal.
        let mut star = BTreeSet::new();
        for i in 0..1000 {
            star.insert(quad("root", "child", &format!("c{i}")));
            star.insert(Quad::new(
                BlankNode::new_unchecked(format!("c{i}")),
                p("v"),
                Literal::new_simple_literal("same"),
                GraphName::DefaultGraph,
            ));
        }
        let (a, elapsed) = timed(star.clone());
        assert!(elapsed.as_secs() < 5, "{elapsed:?}");
        let (b, _) = timed(renamed(&star, |n| format!("z{n}")));
        assert_eq!(a, b);
        assert_eq!(a.len(), 2000);
        // Every child keeps a name of its own.
        let children: HashSet<&Term> = a
            .iter()
            .filter(|q| q.predicate == p("child"))
            .map(|q| &q.object)
            .collect();
        assert_eq!(children.len(), 1000);
    }

    #[test]
    fn equal_components_dont_multiply() {
        let cycles = |count: usize, size: usize| -> BTreeSet<Quad> {
            (0..count)
                .flat_map(|c| {
                    (0..size).map(move |i| {
                        quad(
                            &format!("c{c}n{i}"),
                            "p",
                            &format!("c{c}n{}", (i + 1) % size),
                        )
                    })
                })
                .collect()
        };
        let five = cycles(5, 8);
        let (a, elapsed) = timed(five.clone());
        assert!(elapsed.as_secs() < 5, "{elapsed:?}");
        let (b, _) = timed(renamed(&five, |n| {
            format!("r{}", n.chars().rev().collect::<String>())
        }));
        assert_eq!(a, b);
        assert_ne!(a, timed(cycles(4, 10)).0);
        assert_eq!(a.len(), 40);
    }

    #[test]
    fn twins_that_are_not_interchangeable_are_told_apart() {
        // Two children of a root, one of them also linked to a second root.
        let g: BTreeSet<Quad> = [
            quad("r1", "p", "a"),
            quad("r1", "p", "b"),
            quad("r2", "q", "a"),
        ]
        .into();
        let h: BTreeSet<Quad> = [
            quad("r1", "p", "a"),
            quad("r1", "p", "b"),
            quad("r2", "q", "b"),
        ]
        .into();
        assert_eq!(canonical(g.clone()), canonical(h));
        let k: BTreeSet<Quad> = [
            quad("r1", "p", "a"),
            quad("r1", "p", "b"),
            quad("r2", "p", "a"),
        ]
        .into();
        assert_ne!(canonical(g), canonical(k));
    }

    /// Random graphs and random renamings and orders of them canonicalise alike, and a
    /// changed graph doesn't.
    #[test]
    fn isomorphic_random_graphs_become_equal() {
        let mut rng = StdRng::seed_from_u64(11);
        for _ in 0..300 {
            let nodes = rng.random_range(2..12);
            let mut g = BTreeSet::new();
            for _ in 0..rng.random_range(1..20) {
                let s = format!("n{}", rng.random_range(0..nodes));
                let o = format!("n{}", rng.random_range(0..nodes));
                let pred = ["p", "q"][rng.random_range(0..2)];
                g.insert(quad(&s, pred, &o));
            }
            let mut permutation: Vec<usize> = (0..nodes).collect();
            permutation.shuffle(&mut rng);
            let h = renamed(&g, |n| {
                format!("m{}", permutation[n[1..].parse::<usize>().unwrap()])
            });
            let (cg, ch) = (canonical(g.clone()), canonical(h));
            assert_eq!(cg, ch);
            // Removing one statement changes the graph's canonical form.
            let mut smaller = g.clone();
            let first = smaller.iter().next().cloned().unwrap();
            smaller.remove(&first);
            assert_ne!(canonical(smaller), cg);
        }
    }
}
