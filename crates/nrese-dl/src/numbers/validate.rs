//! Layer 4 of the number module (docs/design/owl2-dl.md#number-reasoning-layers): an
//! independent check of a [`CompressedModelCandidate`] against the ontology's own axioms,
//! the only way the module answers "consistent".
//!
//! It knows nothing of how the candidate was built: it reads the ontology (not the
//! `NumberProblem`), and its class expressions as written. The candidate stands for an
//! interpretation where each proxy is `multiplicity` elements with the same classes, and
//! each block is a biregular relation between two proxies. Such a relation exists as a
//! simple relation when `out · |from| = in · |to|`, `out ≤ |to|`, `in ≤ |from|` and
//! `from ≠ to` (edge `e` from element `e / out` to element `e mod |to|`); blocks of one
//! property between different pairs of proxies don't meet. Then every element of a proxy
//! has the same degree into every other proxy, so a class expression holds of all of a
//! proxy's elements or of none, and checking an axiom proxy by proxy is checking it on
//! the interpretation. An axiom or expression the check doesn't cover makes it decline.

use hashbrown::{HashMap, HashSet};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term};

use super::candidate::CompressedModelCandidate;

/// A candidate checked against every axiom: the ontology has a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    elements: u64,
}

impl Validated {
    /// The elements of the model the candidate compresses.
    pub fn elements(&self) -> u64 {
        self.elements
    }
}

/// Why a candidate isn't validated (not that the ontology has no model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declined(pub String);

/// An edge set of a property read in a direction: from proxy, to proxy, the degree of
/// each `from` element, of each `to` element.
type Edges = (usize, usize, u64, u64);

struct Model<'a> {
    c: &'a CompressedModelCandidate,
    /// Per property as written, its edge sets.
    edges: HashMap<Term, Vec<Edges>>,
    /// Where each individual is.
    individual: HashMap<Term, usize>,
    /// The built-in object properties: fixed relations, which no block gives.
    fixed: [Option<Term>; 2],
}

impl Model<'_> {
    fn edges(&self, r: ObjProp) -> Result<Vec<Edges>, Declined> {
        if self.fixed.contains(&Some(r.named())) {
            return Err(Declined(format!("the built-in property {}", r.named())));
        }
        Ok(match r {
            ObjProp::Named(t) => self.edges.get(&t).cloned().unwrap_or_default(),
            ObjProp::Inverse(t) => self
                .edges
                .get(&t)
                .map(|es| es.iter().map(|&(f, to, o, i)| (to, f, i, o)).collect())
                .unwrap_or_default(),
        })
    }

    /// The number of `r`-neighbours of each element of proxy `x` in proxies where `f`
    /// holds.
    fn count(&self, o: &Ontology, x: usize, r: ObjProp, f: ExprId) -> Result<u64, Declined> {
        let mut n = 0;
        for (from, to, out, _) in self.edges(r)? {
            if from == x && self.holds(o, to, f)? {
                n += out;
            }
        }
        Ok(n)
    }

    fn holds(&self, o: &Ontology, x: usize, e: ExprId) -> Result<bool, Declined> {
        let proxy = &self.c.proxies[x];
        Ok(match o.classes.get(e.0) {
            ClassExpr::Thing => true,
            ClassExpr::Nothing => false,
            ClassExpr::Class(a) => proxy.classes.contains(a),
            ClassExpr::And(xs) => {
                for &y in xs {
                    if !self.holds(o, x, y)? {
                        return Ok(false);
                    }
                }
                true
            }
            ClassExpr::Or(xs) => {
                for &y in xs {
                    if self.holds(o, x, y)? {
                        return Ok(true);
                    }
                }
                false
            }
            ClassExpr::Not(y) => !self.holds(o, x, *y)?,
            ClassExpr::OneOf(xs) => {
                proxy.multiplicity == 1 && xs.iter().any(|a| self.individual.get(a) == Some(&x))
            }
            ClassExpr::Some(r, f) => self.count(o, x, *r, *f)? >= 1,
            ClassExpr::All(r, f) => {
                for (from, to, out, _) in self.edges(*r)? {
                    if from == x && out > 0 && !self.holds(o, to, *f)? {
                        return Ok(false);
                    }
                }
                true
            }
            ClassExpr::HasValue(r, a) => {
                let Some(&y) = self.individual.get(a) else {
                    return Err(Declined(format!("individual {a} has no element")));
                };
                self.edges(*r)?
                    .iter()
                    .any(|&(from, to, out, _)| from == x && to == y && out > 0)
            }
            // No block runs from a proxy to itself: no element is its own neighbour (by a
            // property the blocks give; a built-in one declines).
            ClassExpr::HasSelf(r) => self.edges(*r).map(|_| false)?,
            ClassExpr::Min(n, r, f) => self.count(o, x, *r, *f)? >= u64::from(*n),
            ClassExpr::Max(n, r, f) => self.count(o, x, *r, *f)? <= u64::from(*n),
            ClassExpr::Exact(n, r, f) => self.count(o, x, *r, *f)? == u64::from(*n),
            other => return Err(Declined(format!("the check doesn't cover {other:?}"))),
        })
    }

    /// Whether every element of every proxy has at most one `r`-neighbour.
    fn functional(&self, r: ObjProp) -> Result<bool, Declined> {
        let edges = self.edges(r)?;
        Ok((0..self.c.proxies.len())
            .all(|x| edges.iter().filter(|e| e.0 == x).map(|e| e.2).sum::<u64>() <= 1))
    }

    /// The proxies with an `r`-neighbour.
    fn with_neighbours(&self, r: ObjProp) -> Result<HashSet<usize>, Declined> {
        Ok(self
            .edges(r)?
            .iter()
            .filter(|e| e.2 > 0)
            .map(|e| e.0)
            .collect())
    }
}

/// `candidate` checked against every axiom of `ontology`.
pub fn validate(
    ontology: &Ontology,
    candidate: &CompressedModelCandidate,
) -> Result<Validated, Declined> {
    let no = |why: String| Err(Declined(why));
    if ontology.diagnostics.iter().any(|d| d.is_fatal()) {
        return no("the reader left axioms out".into());
    }
    let proxies = &candidate.proxies;
    if proxies.is_empty() || proxies.iter().any(|p| p.multiplicity == 0) {
        return no("an empty proxy, or none".into());
    }
    // Every degree sum below is at most the elements (a property's blocks from one proxy
    // run to different proxies), which a `u64` then holds.
    if (proxies.iter())
        .try_fold(0u64, |n, p| n.checked_add(p.multiplicity))
        .is_none()
    {
        return no("more elements than a u64 counts".into());
    }
    // Individuals: one element each.
    let mut individual = HashMap::new();
    for (x, p) in proxies.iter().enumerate() {
        if let Some(a) = p.individual
            && (p.multiplicity != 1 || individual.insert(a, x).is_some())
        {
            return no(format!("individual {a} isn't one element"));
        }
    }
    // Every individual the ontology names has its element (an expression the axioms don't
    // use only makes the check stricter).
    let mut named: Vec<Term> = Vec::new();
    for i in 0..ontology.classes.len() as u32 {
        match ontology.classes.get(i) {
            ClassExpr::OneOf(xs) => named.extend(xs),
            ClassExpr::HasValue(_, a) => named.push(*a),
            _ => {}
        }
    }
    for a in &ontology.axioms {
        if let Axiom::ClassAssertion(_, x) = a {
            named.push(*x);
        }
    }
    if let Some(a) = named.iter().find(|a| !individual.contains_key(*a)) {
        return no(format!("individual {a} has no element"));
    }
    // One inverse per property, which its blocks give (below): overlapping pairs relate
    // more properties than one block's reversal does, and a built-in property is fixed.
    let fixed = [ontology.builtin.top_object, ontology.builtin.bottom_object];
    let mut inverse: HashMap<Term, Term> = HashMap::new();
    for axiom in &ontology.axioms {
        if let Axiom::InverseObjectProperties(ObjProp::Named(a), ObjProp::Named(b)) = *axiom {
            for (x, y) in [(a, b), (b, a)] {
                if fixed.contains(&Some(x)) || *inverse.entry(x).or_insert(y) != y {
                    return no(format!("{x}'s inverse isn't one property its blocks give"));
                }
            }
        }
    }
    // Blocks: realisable, and one per property and pair of proxies.
    let mut pairs = HashSet::new();
    let mut edges: HashMap<Term, Vec<Edges>> = HashMap::new();
    for b in &candidate.blocks {
        let (Some(f), Some(t)) = (proxies.get(b.from), proxies.get(b.to)) else {
            return no("a block between proxies that aren't there".into());
        };
        // Its edges counted from either end, the same number (not two overflows).
        let count = b.out_degree.checked_mul(f.multiplicity);
        let realisable = b.from != b.to
            && count.is_some()
            && count == b.in_degree.checked_mul(t.multiplicity)
            && b.out_degree <= t.multiplicity
            && b.in_degree <= f.multiplicity;
        if !realisable || !pairs.insert((b.property, b.from, b.to)) {
            return no(format!("{b:?} isn't one simple relation"));
        }
        if let Some(&u) = inverse.get(&b.property)
            && candidate.blocks.iter().any(|c| c.property == u)
        {
            return no(format!(
                "blocks for both {} and its inverse {u}",
                b.property
            ));
        }
        let e = (b.from, b.to, b.out_degree, b.in_degree);
        edges.entry(b.property).or_default().push(e);
        // The inverse's edges are these, reversed.
        if let Some(&u) = inverse.get(&b.property) {
            edges
                .entry(u)
                .or_default()
                .push((b.to, b.from, b.in_degree, b.out_degree));
        }
    }
    let m = Model {
        c: candidate,
        edges,
        individual,
        fixed,
    };
    let all = 0..proxies.len();
    let every = |e: ExprId| -> Result<Vec<bool>, Declined> {
        all.clone().map(|x| m.holds(ontology, x, e)).collect()
    };
    for axiom in &ontology.axioms {
        let ok = match axiom {
            Axiom::Declaration(..) => true,
            Axiom::SubClassOf(a, b) => {
                let (a, b) = (every(*a)?, every(*b)?);
                a.iter().zip(&b).all(|(&a, &b)| !a || b)
            }
            Axiom::EquivalentClasses(xs) => {
                let vs: Vec<Vec<bool>> = xs.iter().map(|&x| every(x)).collect::<Result<_, _>>()?;
                vs.windows(2).all(|w| w[0] == w[1])
            }
            Axiom::DisjointClasses(xs) => {
                let vs: Vec<Vec<bool>> = xs.iter().map(|&x| every(x)).collect::<Result<_, _>>()?;
                all.clone().all(|x| vs.iter().filter(|v| v[x]).count() <= 1)
            }
            Axiom::ClassAssertion(c, a) => {
                let Some(&x) = m.individual.get(a) else {
                    return no(format!("individual {a} has no element"));
                };
                m.holds(ontology, x, *c)?
            }
            Axiom::ObjectCharacteristic(Characteristic::Functional, r) => m.functional(*r)?,
            Axiom::ObjectCharacteristic(Characteristic::InverseFunctional, r) => {
                m.functional(r.inverse())?
            }
            Axiom::ObjectPropertyDomain(r, c) | Axiom::ObjectPropertyRange(r, c) => {
                let r = match axiom {
                    Axiom::ObjectPropertyDomain(..) => *r,
                    _ => r.inverse(),
                };
                let v = every(*c)?;
                m.with_neighbours(r)?.iter().all(|&x| v[x])
            }
            // Holds by how the edges of an inverse were made, above.
            Axiom::InverseObjectProperties(ObjProp::Named(_), ObjProp::Named(_)) => true,
            other => return no(format!("the check doesn't cover {other:?}")),
        };
        if !ok {
            return no(format!("{axiom:?} fails"));
        }
    }
    Ok(Validated {
        elements: candidate.elements(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::candidate::{Block, Proxy, construct};
    use super::super::closure::close;
    use super::super::problem::extract;
    use super::super::problem::tests::{overlapping_inverses, product, top_over_product};
    use super::*;

    fn checked(o: &Ontology) -> Result<Validated, Declined> {
        let p = extract(o);
        let c = construct(&p, &close(&p).expect("closes")).expect("a candidate");
        validate(o, &c)
    }

    /// DL-906's and 907's shapes: the candidates are models (621 and 60,201 elements).
    #[test]
    fn the_products_are_validated() {
        assert_eq!(
            checked(&product(20, 30, 600)).map(|v| v.elements()),
            Ok(621)
        );
        assert_eq!(
            checked(&product(200, 300, 60_000)).map(|v| v.elements()),
            Ok(60_201)
        );
    }

    /// Guard (the validator is independent): a candidate with a wrong degree, a missing
    /// individual or a block that can't be a simple relation is declined.
    #[test]
    fn broken_candidates_are_declined() {
        let o = product(20, 30, 600);
        let p = extract(&o);
        let good = construct(&p, &close(&p).expect("closes")).expect("a candidate");
        // q's in-degree on N off by one, and the multiplicity to match it.
        let mut c = good.clone();
        let q = c.blocks.iter_mut().find(|b| b.property == 12).expect("q");
        q.in_degree = 29;
        c.proxies[2].multiplicity = 580;
        assert!(validate(&o, &c).is_err());
        // The nominal's individual gone.
        let mut c = good.clone();
        c.proxies[0].individual = None;
        assert!(validate(&o, &c).is_err());
        // A block from a proxy to itself.
        let mut c = good.clone();
        c.blocks.push(Block {
            property: 10,
            from: 1,
            to: 1,
            out_degree: 1,
            in_degree: 1,
        });
        assert!(validate(&o, &c).is_err());
        assert!(validate(&o, &good).is_ok());
    }

    /// The validator holds on its own, whatever built the candidate (PR #38 review, R1-2).
    /// Neither ontology has a model:
    /// - [`overlapping_inverses`], with a's q-edge to b: by `inverse(p, q)` it is b's p-edge
    ///   to a, which `B ⊑ ≤ 0 p` forbids;
    /// - [`top_over_product`], with the product's own candidate: U relates every pair, which
    ///   no block gives;
    /// - the product with `⊤ ⊑ ¬∃U.Self`, with its own candidate: U relates every element to
    ///   itself;
    /// - [`overflowing_block`].
    #[test]
    fn candidates_for_ontologies_without_a_model_are_declined() {
        let proxy = |class, individual| Proxy {
            classes: vec![class],
            individual: Some(individual),
            multiplicity: 1,
        };
        let q_edge = CompressedModelCandidate {
            proxies: vec![proxy(1, 100), proxy(2, 101)],
            blocks: vec![Block {
                property: 11,
                from: 0,
                to: 1,
                out_degree: 1,
                in_degree: 1,
            }],
        };
        let p = extract(&product(2, 3, 6));
        let product_candidate = construct(&p, &close(&p).expect("closes")).expect("a candidate");
        let top = 501;
        let mut no_self = product(2, 3, 6);
        no_self.builtin.top_object = Some(top);
        let mut e = |x| ExprId(no_self.classes.intern(x));
        let thing = e(ClassExpr::Thing);
        let own = e(ClassExpr::HasSelf(ObjProp::Named(top)));
        let not_own = e(ClassExpr::Not(own));
        no_self.axioms.push(Axiom::SubClassOf(thing, not_own));
        no_self.sources.push(Vec::new());
        let (square, square_candidate) = overflowing_block();
        let mut wrong = Vec::new();
        for (what, o, c) in [
            ("overlapping inverse pairs", overlapping_inverses(), q_edge),
            (
                "a built-in property",
                top_over_product(),
                product_candidate.clone(),
            ),
            ("a built-in property's self", no_self, product_candidate),
            ("a block's two edge counts", square, square_candidate),
        ] {
            if let Ok(v) = validate(&o, &c) {
                wrong.push(format!("{what}: validated, {} elements", v.elements()));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// `{d}` with exactly 2³¹ p-predecessors (N), each of them with exactly 2³¹ q- and 2³¹
    /// q2-predecessors (NM and NM2, 2⁶² each; p, q, q2 functional). `NM ⊑ = 4 s.NM2` and
    /// `NM2 ⊑ = 8 s⁻.NM` count the s-edges between them as `4 · 2⁶²` and `8 · 2⁶²`: no
    /// model. The candidate has these sizes, and an s-block whose two counts both overflow
    /// a `u64`.
    fn overflowing_block() -> (Ontology, CompressedModelCandidate) {
        let ([od, n, nm, nm2], d, [p, q, q2, s]) = ([1, 2, 3, 4], 100, [10, 11, 12, 13]);
        let big = 1 << 31;
        let mut o = Ontology::default();
        let mut e = |x| ExprId(o.classes.intern(x));
        let thing = e(ClassExpr::Thing);
        let [c_od, c_n, c_nm, c_nm2] = [od, n, nm, nm2].map(|t| e(ClassExpr::Class(t)));
        let one = e(ClassExpr::OneOf(vec![d]));
        let back = |r| ClassExpr::Exact(big, ObjProp::Inverse(r), thing);
        let [n_p, m_q, m_q2] = [p, q, q2].map(|r| e(back(r)));
        let has_d = e(ClassExpr::Some(ObjProp::Named(p), c_od));
        let [has_n, has_n2] = [q, q2].map(|r| e(ClassExpr::Some(ObjProp::Named(r), c_n)));
        let four = e(ClassExpr::Exact(4, ObjProp::Named(s), c_nm2));
        let eight = e(ClassExpr::Exact(8, ObjProp::Inverse(s), c_nm));
        let functional =
            |r| Axiom::ObjectCharacteristic(Characteristic::Functional, ObjProp::Named(r));
        o.axioms = vec![
            Axiom::EquivalentClasses(vec![c_od, one]),
            Axiom::EquivalentClasses(vec![c_od, n_p]),
            Axiom::EquivalentClasses(vec![c_n, has_d]),
            Axiom::EquivalentClasses(vec![c_n, m_q]),
            Axiom::EquivalentClasses(vec![c_n, m_q2]),
            Axiom::EquivalentClasses(vec![c_nm, has_n]),
            Axiom::EquivalentClasses(vec![c_nm2, has_n2]),
            Axiom::SubClassOf(c_nm, four),
            Axiom::SubClassOf(c_nm2, eight),
            functional(p),
            functional(q),
            functional(q2),
            Axiom::ClassAssertion(thing, d),
        ];
        o.sources = vec![Vec::new(); o.axioms.len()];
        let proxy = |class, individual, multiplicity| Proxy {
            classes: vec![class],
            individual,
            multiplicity,
        };
        let block = |property, from, to, out_degree, in_degree| Block {
            property,
            from,
            to,
            out_degree,
            in_degree,
        };
        let c = CompressedModelCandidate {
            proxies: vec![
                proxy(od, Some(d), 1),
                proxy(n, None, 1 << 31),
                proxy(nm, None, 1 << 62),
                proxy(nm2, None, 1 << 62),
            ],
            blocks: vec![
                block(p, 1, 0, 1, 1 << 31),
                block(q, 2, 1, 1, 1 << 31),
                block(q2, 3, 1, 1, 1 << 31),
                block(s, 2, 3, 4, 8),
            ],
        };
        (o, c)
    }

    /// `a : ≤ 1 r`, with r-successors `b`, one in B1 and one in B2, pairwise disjoint: no
    /// model. The candidate gives B1 and B2 2⁶³ elements each, more than a `u64` counts,
    /// and a's r-degree past it.
    #[test]
    fn element_counts_past_a_u64_are_declined() {
        let ([b1, b2], [a, b], r) = ([2, 3], [100, 101], 10);
        let mut o = Ontology::default();
        let mut e = |x| ExprId(o.classes.intern(x));
        let thing = e(ClassExpr::Thing);
        let [c1, c2] = [b1, b2].map(|t| e(ClassExpr::Class(t)));
        let only_b = e(ClassExpr::OneOf(vec![b]));
        let to_b = e(ClassExpr::HasValue(ObjProp::Named(r), b));
        let [to_1, to_2] = [c1, c2].map(|c| e(ClassExpr::Some(ObjProp::Named(r), c)));
        let at_most_one = e(ClassExpr::Max(1, ObjProp::Named(r), thing));
        o.axioms = vec![
            Axiom::ClassAssertion(to_b, a),
            Axiom::ClassAssertion(to_1, a),
            Axiom::ClassAssertion(to_2, a),
            Axiom::ClassAssertion(at_most_one, a),
            Axiom::DisjointClasses(vec![c1, c2, only_b]),
        ];
        o.sources = vec![Vec::new(); o.axioms.len()];
        let half = 1 << 63;
        let proxy = |classes, individual, multiplicity| Proxy {
            classes,
            individual,
            multiplicity,
        };
        let block = |to, out_degree| Block {
            property: r,
            from: 0,
            to,
            out_degree,
            in_degree: 1,
        };
        let c = CompressedModelCandidate {
            proxies: vec![
                proxy(vec![], Some(a), 1),
                proxy(vec![], Some(b), 1),
                proxy(vec![b1], None, half),
                proxy(vec![b2], None, half),
            ],
            blocks: vec![block(1, 1), block(2, half), block(3, half)],
        };
        assert!(validate(&o, &c).is_err());
    }
}
