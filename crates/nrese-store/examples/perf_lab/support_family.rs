//! Support graph sets under strain (`reasoner.support_sets`, C-310): the generator of the
//! bench family `support-stress` (`perf_lab generate support-stress`, benches/cases), a seeded
//! family of inputs whose inferred statements are derivable through many alternative minimal
//! graph sets, and its oracle, which answers a restricted reader by brute force from the
//! closure of the statements it may read. The store's spec tests (`support_cap_tests`)
//! include it; it uses nothing of the store.
//!
//! A target is `?x a ex:D`: never asserted, derived from `?x a ex:C` through
//! `ex:C rdfs:subClassOf ex:D`, in the ladder also through `owl:sameAs`. Each shape gives the
//! target of every subject an exact number of minimal sets, by construction:
//! - `copies k`: the schema in the default graph, each subject's `a ex:C` in the same k named
//!   graphs: k sets, {the default graph, one of them}.
//! - `product a b`: the schema in a named schema graphs, each subject's `a ex:C` in b data
//!   graphs: a·b sets, {one schema graph, one data graph}.
//! - `ladder w l`: per subject an `owl:sameAs` chain from `m0` to the subject, each of its l
//!   links in the same w graphs of its own, `m0 a ex:C` and the schema in the default graph:
//!   w^l sets, one graph per link (the chain closes into a sameAs clique of l + 1 members).
//!
//! The graphs' names are a seeded permutation of their roles, so the IRI order, by which the
//! store numbers graphs and its cap orders sets of a size, changes with the seed; the oracle
//! doesn't depend on it.

use std::collections::{BTreeMap, BTreeSet};

pub const EX: &str = "http://example.org/support/";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// How a target's alternative derivations are spread over graphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Copies(u32),
    Product(u32, u32),
    Ladder(u32, u32),
}

impl Shape {
    /// The minimal support sets of every target.
    pub fn sets(self) -> u64 {
        match self {
            Shape::Copies(k) => u64::from(k),
            Shape::Product(a, b) => u64::from(a) * u64::from(b),
            Shape::Ladder(w, l) => u64::from(w).pow(l),
        }
    }

    /// The named graphs, by role: copies the data graphs; product the schema graphs, then
    /// the data graphs; ladder link 0's w graphs, then link 1's, ...
    fn graphs(self) -> u32 {
        match self {
            Shape::Copies(k) => k,
            Shape::Product(a, b) => a + b,
            Shape::Ladder(w, l) => w * l,
        }
    }
}

/// A statement: subject, predicate and object (IRIs), in a named graph or (`None`) the
/// default graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quad {
    pub s: String,
    pub p: String,
    pub o: String,
    pub g: Option<String>,
}

impl Quad {
    fn new(s: &str, p: &str, o: &str, g: Option<&str>) -> Self {
        Self {
            s: s.to_owned(),
            p: p.to_owned(),
            o: o.to_owned(),
            g: g.map(str::to_owned),
        }
    }

    /// The statement in N-Quads (a line without its end of line).
    pub fn nquad(&self) -> String {
        match &self.g {
            Some(g) => format!("<{}> <{}> <{}> <{g}> .", self.s, self.p, self.o),
            None => format!("<{}> <{}> <{}> .", self.s, self.p, self.o),
        }
    }

    pub fn typed(s: &str, class: &str, g: Option<&str>) -> Self {
        Self::new(&format!("{EX}{s}"), TYPE, &format!("{EX}{class}"), g)
    }

    pub fn same_as(a: &str, b: &str, g: Option<&str>) -> Self {
        Self::new(&format!("{EX}{a}"), SAME_AS, &format!("{EX}{b}"), g)
    }

    pub fn sub_class(a: &str, b: &str, g: Option<&str>) -> Self {
        Self::new(&format!("{EX}{a}"), SUB_CLASS_OF, &format!("{EX}{b}"), g)
    }
}

/// SplitMix64's finaliser: the seeded order of the graphs' names.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One input of the family: a shape, its subjects, and the seed of its graphs' names.
#[derive(Debug, Clone)]
pub struct Family {
    pub shape: Shape,
    pub subjects: u32,
    pub seed: u64,
    /// The named graphs by role (see [`Shape::graphs`]).
    pub graphs: Vec<String>,
}

impl Family {
    pub fn new(shape: Shape, subjects: u32, seed: u64) -> Self {
        let n = shape.graphs();
        let mut order: Vec<u32> = (0..n).collect();
        order.sort_by_key(|&role| mix(seed ^ mix(u64::from(role))));
        // `rank[role]`: the role's place in IRI order.
        let mut rank = vec![0; n as usize];
        for (place, &role) in order.iter().enumerate() {
            rank[role as usize] = place;
        }
        let graphs = rank
            .iter()
            .map(|place| format!("{EX}g{place:05}"))
            .collect();
        Self {
            shape,
            subjects,
            seed,
            graphs,
        }
    }

    /// The family of `perf_lab generate support-stress`'s `key=value` parameters, as
    /// benches/datasets.toml's `made_by` gives them: `shape=copies sets=K`, `shape=product a=A
    /// b=B` or `shape=ladder width=W length=L`, with `subjects` and `seed`; `None` for a value
    /// that isn't a number.
    pub fn from_params<'a>(params: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let params: BTreeMap<&str, &str> = params.into_iter().collect();
        let number = |key: &str, default: u64| -> Option<u64> {
            params
                .get(key)
                .map_or(Some(default), |v| v.replace('_', "").parse().ok())
        };
        let small =
            |key: &str, default: u64| number(key, default).and_then(|n| u32::try_from(n).ok());
        let shape = match params.get("shape").copied() {
            Some("product") => Shape::Product(small("a", 8)?, small("b", 8)?),
            Some("ladder") => Shape::Ladder(small("width", 2)?, small("length", 4)?),
            _ => Shape::Copies(small("sets", 17)?),
        };
        Some(Self::new(
            shape,
            small("subjects", 1000)?,
            number("seed", 1)?,
        ))
    }

    fn graph(&self, role: u32) -> &str {
        &self.graphs[role as usize]
    }

    /// The schema's statements.
    pub fn schema(&self) -> Vec<Quad> {
        match self.shape {
            Shape::Product(a, _) => (0..a)
                .map(|i| Quad::sub_class("C", "D", Some(self.graph(i))))
                .collect(),
            _ => vec![Quad::sub_class("C", "D", None)],
        }
    }

    /// The statements of subject `j` (any `j`: those past [`Self::subjects`] are new ones,
    /// for commits). Its target is `ex:x{j} a ex:D`.
    pub fn subject(&self, j: u32) -> Vec<Quad> {
        let x = format!("x{j}");
        match self.shape {
            Shape::Copies(k) => (0..k)
                .map(|i| Quad::typed(&x, "C", Some(self.graph(i))))
                .collect(),
            Shape::Product(a, b) => (a..a + b)
                .map(|i| Quad::typed(&x, "C", Some(self.graph(i))))
                .collect(),
            Shape::Ladder(w, l) => {
                let member = |i: u32| match i == l {
                    true => x.clone(),
                    false => format!("x{j}-m{i}"),
                };
                let mut quads = vec![Quad::typed(&member(0), "C", None)];
                for link in 0..l {
                    for i in 0..w {
                        let graph = self.graph(link * w + i);
                        quads.push(Quad::same_as(&member(link), &member(link + 1), Some(graph)));
                    }
                }
                quads
            }
        }
    }

    /// Every statement: the schema, then the subjects in order.
    pub fn quads(&self) -> Vec<Quad> {
        let mut quads = self.schema();
        for j in 0..self.subjects {
            quads.extend(self.subject(j));
        }
        quads
    }

    /// The bench's reader (`perf_lab generate`'s `OUT.reader`): the graphs of one target set,
    /// drawn from the seed.
    pub fn reader(&self) -> Vec<String> {
        self.support(mix(self.seed) % self.shape.sets())
    }

    /// The `i`th minimal support set of every target (`i` below [`Shape::sets`]), as named
    /// graphs; the default graph, which every reader here reads, left out.
    pub fn support(&self, i: u64) -> Vec<String> {
        let roles: Vec<u32> = match self.shape {
            Shape::Copies(_) => vec![i as u32],
            Shape::Product(a, _) => {
                let a = u64::from(a);
                vec![(i % a) as u32, (a + i / a) as u32]
            }
            Shape::Ladder(w, l) => {
                let mut rest = i;
                (0..l)
                    .map(|link| {
                        let pick = (rest % u64::from(w)) as u32;
                        rest /= u64::from(w);
                        link * w + pick
                    })
                    .collect()
            }
        };
        roles
            .into_iter()
            .map(|r| self.graph(r).to_owned())
            .collect()
    }
}

/// The subjects `?x` with `?x a ex:D` in the closure of the statements a reader of the
/// default graph and of `graphs` may read, under the OWL 2 RL rules this vocabulary meets:
/// `owl:sameAs` classes (eq-sym, eq-trans), the subclass closure (scm-sco), memberships
/// (cax-sco) and their copies within a class (eq-rep-s). Computed afresh from those
/// statements alone.
pub fn members_of_d(quads: &[Quad], graphs: &[String]) -> BTreeSet<String> {
    let readable: Vec<&Quad> = quads
        .iter()
        .filter(|q| q.g.as_ref().is_none_or(|g| graphs.contains(g)))
        .collect();
    // The classes `ex:D` is reachable from by subClassOf, itself included.
    let d = format!("{EX}D");
    let mut below: BTreeSet<&str> = BTreeSet::from([d.as_str()]);
    loop {
        let before = below.len();
        for q in readable.iter().filter(|q| q.p == SUB_CLASS_OF) {
            if below.contains(q.o.as_str()) {
                below.insert(&q.s);
            }
        }
        if below.len() == before {
            break;
        }
    }
    // The individuals (typed, or in a sameAs link) and their sameAs classes, by union-find.
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for q in &readable {
        let terms = match q.p.as_str() {
            TYPE => vec![q.s.as_str()],
            SAME_AS => vec![q.s.as_str(), q.o.as_str()],
            _ => Vec::new(),
        };
        for term in terms {
            let next = index.len();
            index.entry(term).or_insert(next);
        }
    }
    let mut parent: Vec<usize> = (0..index.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for q in readable.iter().filter(|q| q.p == SAME_AS) {
        let a = root(&mut parent, index[q.s.as_str()]);
        let b = root(&mut parent, index[q.o.as_str()]);
        parent[a] = b;
    }
    let mut in_d: BTreeSet<usize> = BTreeSet::new();
    for q in readable.iter().filter(|q| q.p == TYPE) {
        if below.contains(q.o.as_str()) {
            in_d.insert(root(&mut parent, index[q.s.as_str()]));
        }
    }
    index
        .iter()
        .filter(|&(_, &i)| in_d.contains(&root(&mut parent, i)))
        .map(|(term, _)| (*term).to_owned())
        .collect()
}

/// Whether `subject` is in `ex:D`, read from the default graph (the inferred statements) and
/// every named graph the reader may read.
pub fn ask(subject: &str) -> String {
    format!(
        "ASK {{ {{ <{subject}> a <{EX}D> }} UNION {{ GRAPH ?g {{ <{subject}> a <{EX}D> }} }} }}"
    )
}

/// How many subjects are in `ex:D`, read as [`ask`] reads.
pub fn count() -> String {
    format!(
        "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {{ ?s a <{EX}D> }} UNION {{ GRAPH ?g {{ ?s a <{EX}D> }} }} }}"
    )
}
