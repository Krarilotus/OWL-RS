//! RDF Dataset Canonicalization (RDFC-1.0, W3C Recommendation, 21 May 2024): canonical
//! blank node labels (`c14n0`, `c14n1`, …) that any conforming implementation gives the
//! same dataset, and so a canonical N-Quads document to hash or sign.
//!
//! [`crate::canonical`] is this crate's own canonicalisation for comparing datasets: faster
//! on symmetric data, but its labels are its own. Use RDFC-1.0 where the labels or the
//! document must be the standard's (signatures, verifiable credentials, exchange).
//!
//! The algorithm as the specification gives it (§4.4–4.8): first-degree hashes of each
//! blank node's quads; canonical labels for unique hashes, in hash order; for the rest,
//! N-degree hashes over the permutations of related blank nodes. Hashes are SHA-256, or
//! SHA-384 on request.
//!
//! - **Speed.** Blank nodes are numbered once; issuers are small integer tables (the
//!   N-degree hashing copies one per permutation tried), and labels are written straight
//!   into the hashed strings.
//! - **Poison graphs.** The N-degree hash can take exponential time on crafted input
//!   (§4.4, "Dataset Poisoning"). The work, counted as N-degree hash calls and permutations
//!   tried, is limited: by default to a budget that grows with the number of blank nodes
//!   (enough for long chains, which take quadratic work), or as set with
//!   [`Rdfc10::with_work_limit`]; over it, [`RdfcError::TooComplex`].
//! - **RDF 1.2.** RDFC-1.0 predates triple terms. Blank nodes inside a triple term are
//!   treated as components of the quad in object position, and triple terms serialise as
//!   canonical N-Quads 1.2 writes them; for RDF 1.1 data this is the specification exactly.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use sha2::{Digest, Sha256, Sha384};

use crate::term::{BlankNode, GraphName, NamedOrBlankNode, Term};
use crate::triple::{Quad, QuadRef, Triple};

/// The hash function of the algorithm (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashAlgorithm {
    #[default]
    Sha256,
    Sha384,
}

/// Why a dataset wasn't canonicalised.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RdfcError {
    /// The N-degree hashing needed more than the work limit (a poison graph, or a very
    /// symmetric one: raise the limit if the input is trusted).
    #[error("canonicalisation needs more than {limit} units of work (a poison graph?)")]
    TooComplex { limit: u64 },
}

/// The settings of RDFC-1.0.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rdfc10 {
    hash: HashAlgorithm,
    /// `None`: the default budget for the input's size.
    work_limit: Option<u64>,
}

/// A canonicalised dataset: the quads with canonical blank node labels, ordered as their
/// canonical N-Quads lines, and the label each input blank node got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canonical {
    pub quads: Vec<Quad>,
    /// Input blank node → canonical blank node, in the order the labels were issued.
    pub issued: Vec<(BlankNode, BlankNode)>,
}

impl Canonical {
    /// The canonical N-Quads document (§4.4.3 step 7): one line per quad, in code point
    /// order, duplicates once.
    pub fn to_nquads(&self) -> String {
        let mut out = String::new();
        for quad in &self.quads {
            let _ = writeln!(out, "{quad} .");
        }
        out
    }
}

impl Rdfc10 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_hash(mut self, hash: HashAlgorithm) -> Self {
        self.hash = hash;
        self
    }

    /// The most work the N-degree hashing may do (calls and permutations tried), instead
    /// of the default budget (100,000 plus 2,000 per blank node).
    pub fn with_work_limit(mut self, limit: u64) -> Self {
        self.work_limit = Some(limit);
        self
    }

    /// The canonical form of `quads` (a dataset: duplicates count once).
    pub fn canonicalize<'a>(
        &self,
        quads: impl IntoIterator<Item = QuadRef<'a>>,
    ) -> Result<Canonical, RdfcError> {
        let mut quads: Vec<Quad> = quads.into_iter().map(QuadRef::into_owned).collect();
        quads.sort();
        quads.dedup();
        let graph = Interned::new(&quads);
        let limit = self
            .work_limit
            .unwrap_or(100_000 + 2_000 * graph.nodes.len() as u64);
        let mut state = State {
            hash: self.hash,
            limit,
            first_degree: Vec::new(),
            canonical: Issuer::new("c14n", graph.nodes.len()),
            work: 0,
            graph: &graph,
        };
        // The N-degree hashing recurses once per blank node along a path of ties (a long
        // chain): a large input gets a thread with the stack that needs, rather than
        // overflowing the caller's (1 MiB on a Windows main thread).
        let nodes = graph.nodes.len();
        if nodes > ON_CALLER_STACK {
            let stack = (nodes * STACK_PER_NODE).clamp(8 << 20, 1 << 30);
            std::thread::scope(|scope| {
                std::thread::Builder::new()
                    .name("rdfc-1.0".to_owned())
                    .stack_size(stack)
                    .spawn_scoped(scope, || state.issue_all())
                    .expect("a thread for canonicalisation")
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })?;
        } else {
            state.issue_all()?;
        }
        let canonical = &state.canonical;
        let label = |b: &BlankNode| {
            let id = graph.id[b];
            BlankNode::new_unchecked(format!("c14n{}", canonical.get(id).expect("issued")))
        };
        let mut lines: Vec<(String, Quad)> = quads
            .iter()
            .map(|quad| {
                let quad = relabel(quad, &label);
                (format!("{quad} .\n"), quad)
            })
            .collect();
        lines.sort_by(|a, b| a.0.cmp(&b.0));
        lines.dedup_by(|a, b| a.0 == b.0);
        Ok(Canonical {
            quads: lines.into_iter().map(|(_, quad)| quad).collect(),
            issued: canonical
                .order
                .iter()
                .enumerate()
                .map(|(i, &id)| {
                    (
                        graph.nodes[id as usize].clone(),
                        BlankNode::new_unchecked(format!("c14n{i}")),
                    )
                })
                .collect(),
        })
    }
}

/// Up to this many blank nodes, the canonicalisation runs on the caller's stack.
const ON_CALLER_STACK: usize = 128;
/// Stack per blank node for deeper inputs: two frames per level of recursion, generously
/// (unoptimised builds have larger frames).
const STACK_PER_NODE: usize = 16 * 1024;

/// A blank node's number in [`Interned`].
type Node = u32;

/// The quads with their blank nodes numbered: per quad its blank nodes and their positions,
/// per blank node its quads.
struct Interned<'a> {
    quads: &'a [Quad],
    nodes: Vec<BlankNode>,
    id: HashMap<BlankNode, Node, foldhash::fast::RandomState>,
    components: Vec<Vec<(Node, u8)>>,
    quads_of: Vec<Vec<u32>>,
}

impl<'a> Interned<'a> {
    fn new(quads: &'a [Quad]) -> Self {
        let mut graph = Self {
            quads,
            nodes: Vec::new(),
            id: HashMap::default(),
            components: Vec::with_capacity(quads.len()),
            quads_of: Vec::new(),
        };
        for (q, quad) in quads.iter().enumerate() {
            let mut components = Vec::new();
            for_each_blank_node(quad, &mut |b, position| {
                let next = graph.nodes.len() as Node;
                let id = *graph.id.entry(b.clone()).or_insert_with(|| {
                    graph.nodes.push(b.clone());
                    graph.quads_of.push(Vec::new());
                    next
                });
                let of = &mut graph.quads_of[id as usize];
                if of.last() != Some(&(q as u32)) {
                    of.push(q as u32);
                }
                components.push((id, position));
            });
            graph.components.push(components);
        }
        graph
    }
}

/// An identifier issuer (§4.3): labels `prefix0`, `prefix1`, … in the order of issue. The
/// N-degree hashing copies issuers often, so labels are numbers in a small table.
#[derive(Clone)]
struct Issuer {
    prefix: &'static str,
    order: Vec<Node>,
    /// Per blank node, its label number plus one (0: none yet); grown on demand.
    label: Vec<u32>,
}

impl Issuer {
    fn new(prefix: &'static str, capacity: usize) -> Self {
        Self {
            prefix,
            order: Vec::new(),
            label: Vec::with_capacity(capacity),
        }
    }

    fn get(&self, node: Node) -> Option<u32> {
        match self.label.get(node as usize) {
            Some(&n) if n > 0 => Some(n - 1),
            _ => None,
        }
    }

    fn issue(&mut self, node: Node) -> u32 {
        if let Some(n) = self.get(node) {
            return n;
        }
        let n = self.order.len() as u32;
        if self.label.len() <= node as usize {
            self.label.resize(node as usize + 1, 0);
        }
        self.label[node as usize] = n + 1;
        self.order.push(node);
        n
    }

    /// Appends `_:` and the label of `node`, issuing it if needed.
    fn push_issued(&mut self, node: Node, out: &mut String) {
        let n = self.issue(node);
        push_label(out, self.prefix, n);
    }
}

fn push_label(out: &mut String, prefix: &str, n: u32) {
    out.push_str("_:");
    out.push_str(prefix);
    let _ = write!(out, "{n}");
}

/// The canonicalisation state (§4.2).
struct State<'g> {
    hash: HashAlgorithm,
    limit: u64,
    graph: &'g Interned<'g>,
    /// Per blank node, its first-degree hash.
    first_degree: Vec<String>,
    canonical: Issuer,
    work: u64,
}

impl State<'_> {
    fn hash(&self, data: &str) -> String {
        match self.hash {
            HashAlgorithm::Sha256 => hex(&Sha256::digest(data.as_bytes())),
            HashAlgorithm::Sha384 => hex(&Sha384::digest(data.as_bytes())),
        }
    }

    fn charge(&mut self) -> Result<(), RdfcError> {
        self.work += 1;
        if self.work > self.limit {
            return Err(RdfcError::TooComplex { limit: self.limit });
        }
        Ok(())
    }

    /// §4.4.3: canonical labels for every blank node.
    fn issue_all(&mut self) -> Result<(), RdfcError> {
        let count = self.graph.nodes.len();
        self.first_degree = (0..count as Node)
            .map(|node| self.hash_first_degree(node))
            .collect();
        let mut by_hash: BTreeMap<&str, Vec<Node>> = BTreeMap::new();
        let first_degree = std::mem::take(&mut self.first_degree);
        for (node, hash) in first_degree.iter().enumerate() {
            by_hash.entry(hash.as_str()).or_default().push(node as Node);
        }
        // Unique hashes first, in hash order.
        let mut shared = Vec::new();
        for (_, nodes) in by_hash {
            if let [only] = nodes[..] {
                self.canonical.issue(only);
            } else {
                shared.push(nodes);
            }
        }
        self.first_degree = first_degree;
        // Then each group of equal hashes, by N-degree hashes.
        for nodes in shared {
            let mut paths: Vec<(String, Issuer)> = Vec::new();
            for &node in &nodes {
                if self.canonical.get(node).is_some() {
                    continue;
                }
                let mut issuer = Issuer::new("b", count);
                issuer.issue(node);
                paths.push(self.hash_n_degree(node, issuer)?);
            }
            paths.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, issuer) in paths {
                for &node in &issuer.order {
                    self.canonical.issue(node);
                }
            }
        }
        Ok(())
    }

    /// §4.6.3: the hash of `node`'s quads, itself as `_:a`, every other blank node as `_:z`.
    fn hash_first_degree(&self, node: Node) -> String {
        let graph = self.graph;
        let me = &graph.nodes[node as usize];
        let mut lines: Vec<String> = graph.quads_of[node as usize]
            .iter()
            .map(|&q| {
                let mut line = String::with_capacity(128);
                write_relabelled(&graph.quads[q as usize], &mut line, &|b| {
                    if b == me { "a" } else { "z" }
                });
                line
            })
            .collect();
        lines.sort_unstable();
        self.hash(&lines.concat())
    }

    /// §4.7.3: the hash of `related` as it appears at `position` of quad `q`.
    fn hash_related(&self, related: Node, q: u32, issuer: &Issuer, position: u8) -> String {
        let mut input = String::with_capacity(160);
        input.push(position as char);
        if position != b'g' {
            input.push('<');
            input.push_str(self.graph.quads[q as usize].predicate.as_str());
            input.push('>');
        }
        if let Some(n) = self.canonical.get(related) {
            push_label(&mut input, self.canonical.prefix, n);
        } else if let Some(n) = issuer.get(related) {
            push_label(&mut input, issuer.prefix, n);
        } else {
            input.push_str(&self.first_degree[related as usize]);
        }
        self.hash(&input)
    }

    /// §4.8.3: the N-degree hash of `node`, and the issuer that comes with it.
    fn hash_n_degree(
        &mut self,
        node: Node,
        mut issuer: Issuer,
    ) -> Result<(String, Issuer), RdfcError> {
        self.charge()?;
        let graph = self.graph;
        // The related blank nodes, by the hash of how they are related.
        let mut related: BTreeMap<String, Vec<Node>> = BTreeMap::new();
        for &q in &graph.quads_of[node as usize] {
            for &(b, position) in &graph.components[q as usize] {
                if b != node {
                    let hash = self.hash_related(b, q, &issuer, position);
                    related.entry(hash).or_default().push(b);
                }
            }
        }
        let mut data = String::new();
        for (hash, mut permutation) in related {
            data.push_str(&hash);
            let mut chosen_path = String::new();
            let mut chosen_issuer: Option<Issuer> = None;
            permutation.sort_unstable();
            loop {
                self.charge()?;
                if let Some((path, copy)) =
                    self.try_permutation(&permutation, &issuer, &chosen_path)?
                    && (chosen_path.is_empty() || path < chosen_path)
                {
                    chosen_path = path;
                    chosen_issuer = Some(copy);
                }
                if !next_permutation(&mut permutation) {
                    break;
                }
            }
            data.push_str(&chosen_path);
            if let Some(chosen) = chosen_issuer {
                issuer = chosen;
            }
        }
        Ok((self.hash(&data), issuer))
    }

    /// One permutation of the related nodes (§4.8.3 step 5.4): its path and issuer, or
    /// `None` once it can't beat `chosen_path`.
    fn try_permutation(
        &mut self,
        permutation: &[Node],
        issuer: &Issuer,
        chosen_path: &str,
    ) -> Result<Option<(String, Issuer)>, RdfcError> {
        let mut copy = issuer.clone();
        let mut path = String::new();
        let mut recursion = Vec::new();
        let worse = |path: &str| {
            !chosen_path.is_empty() && path.len() >= chosen_path.len() && path > chosen_path
        };
        for &related in permutation {
            if let Some(n) = self.canonical.get(related) {
                push_label(&mut path, self.canonical.prefix, n);
            } else {
                if copy.get(related).is_none() {
                    recursion.push(related);
                }
                copy.push_issued(related, &mut path);
            }
            if worse(&path) {
                return Ok(None);
            }
        }
        for related in recursion {
            let (hash, result) = self.hash_n_degree(related, copy)?;
            copy = result;
            copy.push_issued(related, &mut path);
            path.push('<');
            path.push_str(&hash);
            path.push('>');
            if worse(&path) {
                return Ok(None);
            }
        }
        Ok(Some((path, copy)))
    }
}

/// Every blank node of `quad`, with its position: `s`, `o` (inside a triple term too) or
/// `g`.
fn for_each_blank_node(quad: &Quad, f: &mut impl FnMut(&BlankNode, u8)) {
    fn in_term(term: &Term, f: &mut impl FnMut(&BlankNode, u8)) {
        match term {
            Term::BlankNode(b) => f(b, b'o'),
            Term::Triple(t) => {
                if let NamedOrBlankNode::BlankNode(b) = &t.subject {
                    f(b, b'o');
                }
                in_term(&t.object, f);
            }
            _ => {}
        }
    }
    if let NamedOrBlankNode::BlankNode(b) = &quad.subject {
        f(b, b's');
    }
    in_term(&quad.object, f);
    if let GraphName::BlankNode(b) = &quad.graph_name {
        f(b, b'g');
    }
}

/// `quad` as a canonical N-Quads line with each blank node labelled `label` of it: the
/// line `relabel` and `Display` would give, without building the quad.
fn write_relabelled(quad: &Quad, out: &mut String, label: &impl Fn(&BlankNode) -> &'static str) {
    fn node(n: &NamedOrBlankNode, out: &mut String, label: &impl Fn(&BlankNode) -> &'static str) {
        match n {
            NamedOrBlankNode::BlankNode(b) => {
                out.push_str("_:");
                out.push_str(label(b));
            }
            NamedOrBlankNode::NamedNode(iri) => {
                let _ = write!(out, "{iri}");
            }
        }
    }
    fn term(t: &Term, out: &mut String, label: &impl Fn(&BlankNode) -> &'static str) {
        match t {
            Term::BlankNode(b) => {
                out.push_str("_:");
                out.push_str(label(b));
            }
            Term::Triple(triple) => {
                out.push_str("<<( ");
                node(&triple.subject, out, label);
                let _ = write!(out, " {} ", triple.predicate);
                term(&triple.object, out, label);
                out.push_str(" )>>");
            }
            other => {
                let _ = write!(out, "{other}");
            }
        }
    }
    node(&quad.subject, out, label);
    let _ = write!(out, " {} ", quad.predicate);
    term(&quad.object, out, label);
    match &quad.graph_name {
        GraphName::BlankNode(b) => {
            out.push_str(" _:");
            out.push_str(label(b));
        }
        GraphName::NamedNode(iri) => {
            let _ = write!(out, " {iri}");
        }
        GraphName::DefaultGraph => {}
    }
    out.push_str(" .\n");
}

/// `quad` with every blank node replaced by `rename` of it.
fn relabel(quad: &Quad, rename: &impl Fn(&BlankNode) -> BlankNode) -> Quad {
    fn node(n: &NamedOrBlankNode, rename: &impl Fn(&BlankNode) -> BlankNode) -> NamedOrBlankNode {
        match n {
            NamedOrBlankNode::BlankNode(b) => rename(b).into(),
            other => other.clone(),
        }
    }
    fn term(t: &Term, rename: &impl Fn(&BlankNode) -> BlankNode) -> Term {
        match t {
            Term::BlankNode(b) => rename(b).into(),
            Term::Triple(triple) => Triple::new(
                node(&triple.subject, rename),
                triple.predicate.clone(),
                term(&triple.object, rename),
            )
            .into(),
            other => other.clone(),
        }
    }
    Quad {
        subject: node(&quad.subject, rename),
        predicate: quad.predicate.clone(),
        object: term(&quad.object, rename),
        graph_name: match &quad.graph_name {
            GraphName::BlankNode(b) => rename(b).into(),
            other => other.clone(),
        },
    }
}

/// The next permutation in lexicographic order; `false` after the last.
fn next_permutation<T: Ord>(items: &mut [T]) -> bool {
    let Some(i) = items.windows(2).rposition(|w| w[0] < w[1]) else {
        return false;
    };
    let j = items
        .iter()
        .rposition(|x| *x > items[i])
        .expect("a larger element exists after i");
    items.swap(i, j);
    items[i + 1..].reverse();
    true
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::NamedNode;

    #[test]
    fn permutations_in_order() {
        let mut items = vec![1, 2, 3];
        let mut all = vec![items.clone()];
        while next_permutation(&mut items) {
            all.push(items.clone());
        }
        assert_eq!(all.len(), 6);
        assert_eq!(all.last().unwrap(), &vec![3, 2, 1]);
    }

    /// A chain of blank nodes whose values repeat: every link is a tie, so the N-degree
    /// hashing recurses along the chain. Called from a thread with a small stack (256 KiB,
    /// on which this chain overflows without the canonicaliser's own thread), it must
    /// neither overflow nor depend on the labels.
    #[test]
    fn a_long_chain_of_ties_needs_no_large_caller_stack() {
        fn chain(prefix: &str) -> Vec<Quad> {
            let n = |iri: &str| NamedNode::new_unchecked(format!("http://example.com/#{iri}"));
            (0..600)
                .flat_map(|i| {
                    let node = BlankNode::new_unchecked(format!("{prefix}{i}"));
                    [
                        Quad::new(
                            node.clone(),
                            n("next"),
                            BlankNode::new_unchecked(format!("{prefix}{}", i + 1)),
                            GraphName::DefaultGraph,
                        ),
                        Quad::new(
                            node,
                            n("value"),
                            crate::term::Literal::new_simple_literal(format!("v{}", i % 5)),
                            GraphName::DefaultGraph,
                        ),
                    ]
                })
                .collect()
        }
        let forms = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                ["x", "other"].map(|prefix| {
                    Rdfc10::new()
                        .canonicalize(chain(prefix).iter().map(Quad::as_ref))
                        .unwrap()
                        .to_nquads()
                })
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(forms[0], forms[1]);
    }

    #[test]
    fn the_specification_example() {
        // RDFC-1.0, the example of §4.4: two blank nodes, each with a value of its own.
        let n = |iri: &str| NamedNode::new_unchecked(format!("http://example.com/#{iri}"));
        let b = BlankNode::new_unchecked;
        let quads = [
            Quad::new(n("p"), n("q"), b("e0"), GraphName::DefaultGraph),
            Quad::new(n("p"), n("r"), b("e1"), GraphName::DefaultGraph),
            Quad::new(b("e0"), n("s"), n("u"), GraphName::DefaultGraph),
            Quad::new(b("e1"), n("t"), n("u"), GraphName::DefaultGraph),
        ];
        let canonical = Rdfc10::new()
            .canonicalize(quads.iter().map(Quad::as_ref))
            .unwrap();
        assert_eq!(
            canonical.to_nquads(),
            "<http://example.com/#p> <http://example.com/#q> _:c14n0 .\n\
             <http://example.com/#p> <http://example.com/#r> _:c14n1 .\n\
             _:c14n0 <http://example.com/#s> <http://example.com/#u> .\n\
             _:c14n1 <http://example.com/#t> <http://example.com/#u> .\n"
        );
    }
}
