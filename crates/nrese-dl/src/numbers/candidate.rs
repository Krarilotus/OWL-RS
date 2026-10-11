//! Layer 3 of the number module (docs/design/owl2-dl.md#number-reasoning-layers): a
//! compressed model built from the counts, **untrusted**. One proxy per counted class
//! stands for its elements (a multiplicity, the classes they are in, the individual where
//! the class is a singleton); one block per property stands for its edges between two
//! proxies, as a biregular relation (every element of `from` has `out_degree` edges into
//! `to`, every element of `to` has `in_degree` from `from`).
//!
//! The construction is a heuristic: it may build a candidate that isn't a model, and it
//! declines (`None`) where it can't build one. Neither means anything about the ontology:
//! only layer 4's validator may turn a candidate into "consistent", and a construction that
//! fails falls back to the search, never to "inconsistent".

use hashbrown::HashMap;
use nrese_owl::Term;

use super::closure::Counts;
use super::problem::{Dir, Expr, NumberProblem};

/// Elements that agree on every class and every block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    /// The named classes they are in, sorted.
    pub classes: Vec<Term>,
    /// The individual, where the proxy is a singleton class's one element.
    pub individual: Option<Term>,
    pub multiplicity: u64,
}

/// The edges of `property` (as written, not inverted) from `from` to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub property: Term,
    pub from: usize,
    pub to: usize,
    pub out_degree: u64,
    pub in_degree: u64,
}

/// A compressed interpretation, not yet checked against anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedModelCandidate {
    pub proxies: Vec<Proxy>,
    pub blocks: Vec<Block>,
}

impl CompressedModelCandidate {
    /// The elements it stands for.
    pub fn elements(&self) -> u64 {
        self.proxies.iter().map(|p| p.multiplicity).sum()
    }
}

/// A candidate for `p`'s counts, if one can be built.
pub fn construct(p: &NumberProblem, counts: &Counts) -> Option<CompressedModelCandidate> {
    if !p.complete() {
        return None;
    }
    // Singleton classes and their individuals.
    let singleton: HashMap<Term, Term> = p
        .inclusions
        .iter()
        .filter_map(|i| match (i.sub, i.sup) {
            (Expr::Class(c), Expr::Nominal(a)) => Some((c, a)),
            _ => None,
        })
        .collect();
    // Every asserted individual must be a singleton class's: an individual of its own
    // would need its own proxy, and its successors.
    if p.assertions
        .iter()
        .any(|&(_, a, _)| !singleton.values().any(|&b| b == a))
    {
        return None;
    }
    let mut proxies: Vec<Proxy> = Vec::new();
    let mut of_class: HashMap<Term, usize> = HashMap::new();
    let mut sized: Vec<(Term, u64)> = counts
        .values
        .iter()
        .filter_map(|(u, &v)| match u {
            super::closure::Unknown::Size(c) if v > 0 => Some((*c, v)),
            _ => None,
        })
        .collect();
    sized.sort_unstable();
    for (c, v) in sized {
        of_class.insert(c, proxies.len());
        proxies.push(Proxy {
            classes: vec![c],
            individual: singleton.get(&c).copied().filter(|_| v == 1),
            multiplicity: v,
        });
    }
    // One block per property with known edges, domain and range.
    let domain: HashMap<Dir, Term> = p
        .domains
        .iter()
        .filter_map(|&(d, c, _)| c.map(|c| (d, c)))
        .collect();
    let mut blocks = Vec::new();
    let mut properties: Vec<(Term, u64)> = counts
        .values
        .iter()
        .filter_map(|(u, &v)| match u {
            super::closure::Unknown::Edges(t) if v > 0 => Some((*t, v)),
            _ => None,
        })
        .collect();
    properties.sort_unstable();
    for (t, edges) in properties {
        let (Some(from), Some(to)) = (domain.get(&(t, false)), domain.get(&(t, true))) else {
            return None;
        };
        let (&from, &to) = (of_class.get(from)?, of_class.get(to)?);
        let (fm, tm) = (proxies[from].multiplicity, proxies[to].multiplicity);
        if edges % fm != 0 || edges % tm != 0 {
            return None;
        }
        blocks.push(Block {
            property: t,
            from,
            to,
            out_degree: edges / fm,
            in_degree: edges / tm,
        });
    }
    let mut candidate = CompressedModelCandidate { proxies, blocks };
    // Labels: a class wherever an inclusion's left side holds, to a fixpoint.
    loop {
        let mut changed = false;
        for i in &p.inclusions {
            let Expr::Class(d) = i.sup else {
                continue;
            };
            for x in 0..candidate.proxies.len() {
                if !candidate.proxies[x].classes.contains(&d) && holds(&candidate, x, i.sub) {
                    candidate.proxies[x].classes.push(d);
                    candidate.proxies[x].classes.sort_unstable();
                    changed = true;
                }
            }
        }
        if !changed {
            return Some(candidate);
        }
    }
}

/// The degree of `x`'s elements in direction `d` (layer 3's own reading).
fn degree(c: &CompressedModelCandidate, x: usize, (t, inverted): Dir) -> u64 {
    c.blocks
        .iter()
        .filter(|b| b.property == t)
        .map(|b| match inverted {
            false if b.from == x => b.out_degree,
            true if b.to == x => b.in_degree,
            _ => 0,
        })
        .sum()
}

/// Whether `e` holds of `x`'s elements, as the construction reads it.
fn holds(c: &CompressedModelCandidate, x: usize, e: Expr) -> bool {
    let proxy = &c.proxies[x];
    match e {
        Expr::Thing => true,
        Expr::Class(a) => proxy.classes.contains(&a),
        Expr::Nominal(a) => proxy.individual == Some(a) && proxy.multiplicity == 1,
        Expr::Some((t, inverted), filler) => c.blocks.iter().any(|b| {
            let (here, there, d) = if inverted {
                (b.to, b.from, b.in_degree)
            } else {
                (b.from, b.to, b.out_degree)
            };
            b.property == t
                && here == x
                && d > 0
                && filler.is_none_or(|f| c.proxies[there].classes.contains(&f))
        }),
        Expr::AtLeast(n, d) => degree(c, x, d) >= u64::from(n),
        Expr::AtMost(n, d) => degree(c, x, d) <= u64::from(n),
        Expr::Exact(n, d) => degree(c, x, d) == u64::from(n),
    }
}

#[cfg(test)]
mod tests {
    use super::super::closure::close;
    use super::super::problem::{extract, tests::product};
    use super::*;

    fn candidate(n: u32, m: u32, k: u32) -> CompressedModelCandidate {
        let p = extract(&product(n, m, k));
        let counts = close(&p).expect("closes");
        construct(&p, &counts).expect("a candidate")
    }

    /// DL-906's shape: three proxies (the nominal, `N` × 20, `NM` × 600) and a block per
    /// property with its degrees (`q`: out-degree 1 on `NM`, in-degree 30 on `N`).
    #[test]
    fn the_product_of_906_has_three_proxies() {
        let c = candidate(20, 30, 600);
        let mult: Vec<u64> = c.proxies.iter().map(|p| p.multiplicity).collect();
        assert_eq!(mult, vec![1, 20, 600], "{c:?}");
        assert_eq!(c.proxies[0].individual, Some(100));
        assert_eq!(c.elements(), 621);
        assert_eq!(c.blocks.len(), 3, "{c:?}");
        // q (12) folded with invQ (13): NM (proxy 2) → N (proxy 1).
        let q = c
            .blocks
            .iter()
            .find(|b| b.property == 12)
            .expect("q's block");
        assert_eq!((q.from, q.to, q.out_degree, q.in_degree), (2, 1, 1, 30));
    }

    /// Guard (DL-907): the candidate stands for 60,201 elements and has three proxies; no
    /// element is built.
    #[test]
    fn the_product_of_907_stays_three_proxies() {
        let c = candidate(200, 300, 60_000);
        assert_eq!(c.proxies.len(), 3);
        assert_eq!(c.elements(), 60_201);
    }

    /// An incomplete problem gives no candidate, whatever its counts.
    #[test]
    fn an_incomplete_problem_has_no_candidate() {
        let mut o = product(20, 30, 600);
        o.axioms
            .push(nrese_owl::Axiom::SameIndividual(vec![100, 101]));
        o.sources.push(Vec::new());
        let p = extract(&o);
        let counts = close(&p).expect("closes");
        assert!(construct(&p, &counts).is_none());
    }
}
