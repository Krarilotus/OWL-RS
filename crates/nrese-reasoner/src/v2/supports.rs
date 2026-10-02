//! Support counts (docs/design/reasoner-provenance.md, step 1): per inferred fact, the
//! number of its derivations by rule instances that are *non-recursive* with respect to
//! it (Hu, Motik and Horrocks, AAAI 2018). A deleted premise then removes a fact without
//! a proof search when one of those derivations survives; the recursive rest is repaired
//! by B/F or DRed as before ([`super::delta`]).
//!
//! **Recursion by key.** At the level of predicates almost every OWL 2 RL rule is
//! recursive (`rdf:type` feeds `rdf:type`). The ground program bakes the schema into the
//! rules (`?x a :C → ?x a :D` per subclass axiom), so recursion is decided over keys: an
//! atom's predicate, or `rdf:type` with its class. The keys' dependency graph (each rule
//! instance: body keys → head keys) is condensed into strongly connected components; an
//! instance is non-recursive when its head's component is none of its body's. Wildcards
//! (a variable predicate or class) are nodes of their own, linked so that what they read
//! and write is conservative: a cycle through a wildcard makes everything on it
//! recursive.
//!
//! **Equality.** The rules the equality module replaces (`eq-rep-*`) aren't rule
//! instances there and aren't counted. Most of them keep a fact's key (`eq-rep-s`, and
//! `eq-rep-o` outside `rdf:type`); but each depends on its `sameAs` fact, so `owl:sameAs`
//! feeds every key, and every key that can derive `sameAs` is on a cycle with it.
//! `owl:sameAs` between two classes (`eq-rep-o` on a type) or two properties
//! (`eq-rep-p`) moreover links their keys: those pairs are edges both ways, read from the
//! data (`partners`). A commit that changes such a `sameAs` leaves the counts stale.

use hashbrown::HashMap;

use super::eval::{GroundProgram, Job, Source, run_jobs};
use super::ir::{Atom, Head, Rule, Term, Triple};

/// A node of the key graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    /// `rdf:type` with a constant class.
    Class(u64),
    /// A constant predicate other than `rdf:type`.
    Predicate(u64),
    /// Read by an atom `?x rdf:type ?c`: every class flows into it.
    ReadAnyClass,
    /// Written by a head `?x rdf:type ?c`: it flows into every class.
    WriteAnyClass,
    /// Read by an atom with a variable predicate: every key flows into it.
    ReadAny,
    /// Written by a head with a variable predicate: it flows into every key.
    WriteAny,
}

/// The node an atom reads (in a body) or writes (in a head).
fn node(atom: &Atom, rdf_type: u64, head: bool) -> Node {
    match (atom.0[1], atom.0[2], head) {
        (Term::Const(p), Term::Const(c), _) if p == rdf_type => Node::Class(c),
        (Term::Const(p), _, false) if p == rdf_type => Node::ReadAnyClass,
        (Term::Const(p), _, true) if p == rdf_type => Node::WriteAnyClass,
        (Term::Const(p), _, _) => Node::Predicate(p),
        (Term::Var(_), _, false) => Node::ReadAny,
        (Term::Var(_), _, true) => Node::WriteAny,
    }
}

/// Which rules of a ground program are non-recursive (by index into `rules`).
/// `partners` gives the `owl:sameAs` partners of a term (other than itself): classes and
/// properties with partners are linked to them (see the module docs).
pub fn non_recursive(
    rules: &[Rule],
    rdf_type: u64,
    partners: &dyn Fn(u64) -> Vec<u64>,
) -> Vec<bool> {
    let counted = |rule: &Rule| !super::batch::is_replacement_rule(rule);
    let mut index: HashMap<Node, usize> = HashMap::new();
    let id = |n: Node, index: &mut HashMap<Node, usize>| {
        let next = index.len();
        *index.entry(n).or_insert(next)
    };
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut atoms_of: Vec<(Vec<usize>, Vec<usize>)> = Vec::with_capacity(rules.len());
    for rule in rules {
        let (body, head): (Vec<usize>, Vec<usize>) = match (&rule.head, counted(rule)) {
            (Head::Facts(atoms), true) => (
                rule.body
                    .iter()
                    .map(|a| id(node(a, rdf_type, false), &mut index))
                    .collect(),
                atoms
                    .iter()
                    .map(|a| id(node(a, rdf_type, true), &mut index))
                    .collect(),
            ),
            _ => (Vec::new(), Vec::new()),
        };
        for &b in &body {
            for &h in &head {
                edges.push((b, h));
            }
        }
        // A rule the equality module replaces copies a fact to a `sameAs` partner of one
        // of its terms: the copy keeps the fact's key (or moves it to a partner key,
        // linked below), but it depends on the `sameAs` fact, which therefore feeds every
        // key.
        if !counted(rule) {
            for atom in &rule.body {
                if let Node::Predicate(_) = node(atom, rdf_type, false) {
                    let same_as = id(node(atom, rdf_type, false), &mut index);
                    let every = id(Node::WriteAny, &mut index);
                    edges.push((same_as, every));
                }
            }
        }
        atoms_of.push((body, head));
    }
    // The wildcards' links: what is written to a wildcard may be any key it stands for,
    // and every key flows into the wildcards that read it.
    let wildcards = [
        Node::ReadAnyClass,
        Node::WriteAnyClass,
        Node::ReadAny,
        Node::WriteAny,
    ]
    .map(|n| id(n, &mut index));
    let [read_class, write_class, read_any, write_any] = wildcards;
    edges.extend([
        (write_class, read_class),
        (write_class, read_any),
        (write_any, read_any),
        (write_any, read_class),
    ]);
    let keys: Vec<(Node, usize)> = index.iter().map(|(&k, &n)| (k, n)).collect();
    for (k, n) in keys {
        match k {
            Node::Class(_) => edges.extend([
                (n, read_class),
                (n, read_any),
                (write_class, n),
                (write_any, n),
            ]),
            Node::Predicate(_) => edges.extend([(n, read_any), (write_any, n)]),
            _ => {}
        }
    }
    // Classes and properties made the same by `sameAs`.
    let terms: Vec<Node> = index
        .keys()
        .copied()
        .filter(|k| matches!(k, Node::Class(_) | Node::Predicate(_)))
        .collect();
    for k in terms {
        let (Node::Class(t) | Node::Predicate(t)) = k else {
            continue;
        };
        let n = index[&k];
        for partner in partners(t) {
            let other = match k {
                Node::Class(_) => Node::Class(partner),
                _ => Node::Predicate(partner),
            };
            let m = id(other, &mut index);
            edges.extend([(n, m), (m, n)]);
        }
    }
    let component = components(index.len(), &edges);
    atoms_of
        .iter()
        .zip(rules)
        .map(|((body, head), rule)| {
            counted(rule)
                && !head.is_empty()
                && !body.is_empty()
                && head
                    .iter()
                    .all(|h| body.iter().all(|b| component[*b] != component[*h]))
        })
        .collect()
}

/// Strongly connected components (iterative Tarjan): the component of each node.
fn components(nodes: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); nodes];
    for &(a, b) in edges {
        adjacency[a].push(b);
    }
    const UNSEEN: usize = usize::MAX;
    let (mut index, mut low, mut on_stack) =
        (vec![UNSEEN; nodes], vec![0; nodes], vec![false; nodes]);
    let mut component = vec![UNSEEN; nodes];
    let (mut next, mut count) = (0, 0);
    let mut stack = Vec::new();
    for root in 0..nodes {
        if index[root] != UNSEEN {
            continue;
        }
        // (node, next edge to look at)
        let mut work = vec![(root, 0usize)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut at)) = work.last_mut() {
            if let Some(&w) = adjacency[v].get(*at) {
                *at += 1;
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component[w] = count;
                    if w == v {
                        break;
                    }
                }
                count += 1;
            }
        }
    }
    component
}

/// The constant classes and predicates of `rules`' atoms: the terms whose `sameAs` links
/// keys.
pub fn key_terms(rules: &[Rule], rdf_type: u64) -> hashbrown::HashSet<u64> {
    let mut out = hashbrown::HashSet::new();
    for rule in rules {
        let heads: &[Atom] = match &rule.head {
            Head::Facts(atoms) => atoms,
            Head::Inconsistent => &[],
        };
        for atom in rule.body.iter().chain(heads) {
            match node(atom, rdf_type, false) {
                Node::Class(t) | Node::Predicate(t) => {
                    out.insert(t);
                }
                _ => {}
            }
        }
    }
    out
}

/// The `owl:sameAs` partners of `term` in `source` (other than itself).
pub fn same_as_partners<S: Source + ?Sized>(
    source: &S,
    same_as: Option<u64>,
    term: u64,
) -> Vec<u64> {
    let mut out = Vec::new();
    if let Some(same_as) = same_as {
        source.scan(
            [Some(term), Some(same_as), None],
            super::eval::Seg::All,
            &mut |t| {
                if t[2] != term {
                    out.push(t[2]);
                }
            },
        );
    }
    out
}

/// Per inferred fact, its non-recursive derivations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Supports {
    counts: HashMap<Triple, u32>,
}

impl Supports {
    /// The supports of the closure `source` under `program`: every non-recursive rule
    /// instance over all facts, counted per head that `inferred` accepts.
    pub fn count<S: Source + ?Sized>(
        source: &S,
        program: &GroundProgram,
        rdf_type: u64,
        same_as: Option<u64>,
        inferred: &(dyn Fn(Triple) -> bool + Sync),
    ) -> Self {
        let counted = non_recursive(&program.rules, rdf_type, &|term| {
            same_as_partners(source, same_as, term)
        });
        let jobs: Vec<Job<'_>> = program
            .rules
            .iter()
            .zip(&counted)
            .filter(|(rule, counted)| **counted && !rule.body.is_empty())
            .filter_map(|(rule, _)| Job::full(source, rule))
            .collect();
        let mut counts: HashMap<Triple, u32> = HashMap::new();
        for fact in run_jobs(source, &jobs, inferred, super::eval::NEVER) {
            *counts.entry(fact).or_default() += 1;
        }
        Self { counts }
    }

    /// The supports of an in-memory closure (asserted and inferred facts).
    pub fn of_closure(
        facts: &[Triple],
        program: &GroundProgram,
        rdf_type: u64,
        same_as: Option<u64>,
        inferred: &(dyn Fn(Triple) -> bool + Sync),
    ) -> Self {
        let store = super::batch::Store::new(facts.to_vec());
        Self::count(&store, program, rdf_type, same_as, inferred)
    }

    /// The non-recursive derivations of `fact`.
    pub fn of(&self, fact: Triple) -> u32 {
        self.counts.get(&fact).copied().unwrap_or(0)
    }

    /// Applies the changes an update computed ([`super::delta::Update::support_changes`]).
    pub fn apply(&mut self, changes: &[(Triple, i64)]) {
        for &(fact, change) in changes {
            let now = i64::from(self.of(fact)) + change;
            if now <= 0 {
                self.counts.remove(&fact);
            } else {
                self.counts
                    .insert(fact, u32::try_from(now).unwrap_or(u32::MAX));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(s: Term, p: u64, o: Term) -> Atom {
        Atom([s, Term::Const(p), o])
    }

    fn rule(body: Vec<Atom>, head: Vec<Atom>) -> Rule {
        Rule {
            name: "r".to_owned(),
            body,
            guards: Vec::new(),
            head: Head::Facts(head),
        }
    }

    #[test]
    fn class_hierarchies_are_non_recursive_and_cycles_are_not() {
        let (ty, x) = (1, Term::Var(0));
        let c = |class| Term::Const(class);
        let rules = [
            // C ⊑ D, D ⊑ E: non-recursive.
            rule(vec![atom(x, ty, c(10))], vec![atom(x, ty, c(11))]),
            rule(vec![atom(x, ty, c(11))], vec![atom(x, ty, c(12))]),
            // F ⊑ G, G ⊑ F: a cycle.
            rule(vec![atom(x, ty, c(20))], vec![atom(x, ty, c(21))]),
            rule(vec![atom(x, ty, c(21))], vec![atom(x, ty, c(20))]),
            // A property's domain: non-recursive.
            rule(vec![atom(x, 5, Term::Var(1))], vec![atom(x, ty, c(10))]),
            // Transitivity: recursive.
            rule(
                vec![
                    atom(x, 6, Term::Var(1)),
                    atom(Term::Var(1), 6, Term::Var(2)),
                ],
                vec![atom(x, 6, Term::Var(2))],
            ),
        ];
        assert_eq!(
            non_recursive(&rules, ty, &|_| Vec::new()),
            [true, true, false, false, true, false]
        );
        // C sameAs D: C and D are one component, so C ⊑ D is recursive.
        let same = |term: u64| match term {
            10 => vec![11],
            11 => vec![10],
            _ => Vec::new(),
        };
        assert_eq!(
            non_recursive(&rules[..2], ty, &same),
            [false, true],
            "D ⊑ E stays non-recursive"
        );
    }

    #[test]
    fn a_variable_class_reads_every_class() {
        let (ty, x, y) = (1, Term::Var(0), Term::Var(1));
        let rules = [
            // ?x a ?c -> ?x p ?c, and ?x p ?c -> ?x a ?c: a cycle through the wildcard.
            rule(vec![atom(x, ty, y)], vec![atom(x, 7, y)]),
            rule(vec![atom(x, 7, y)], vec![atom(x, ty, y)]),
            // A class fed from it is on the cycle too.
            rule(
                vec![atom(x, ty, Term::Const(10))],
                vec![atom(x, ty, Term::Const(11))],
            ),
        ];
        assert_eq!(
            non_recursive(&rules, ty, &|_| Vec::new()),
            [false, false, false]
        );
    }
}
