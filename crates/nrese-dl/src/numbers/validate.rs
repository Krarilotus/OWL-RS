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
}

impl Model<'_> {
    fn edges(&self, r: ObjProp) -> Vec<Edges> {
        match r {
            ObjProp::Named(t) => self.edges.get(&t).cloned().unwrap_or_default(),
            ObjProp::Inverse(t) => self
                .edges
                .get(&t)
                .map(|es| es.iter().map(|&(f, to, o, i)| (to, f, i, o)).collect())
                .unwrap_or_default(),
        }
    }

    /// The number of `r`-neighbours of each element of proxy `x` in proxies where `f`
    /// holds.
    fn count(&self, o: &Ontology, x: usize, r: ObjProp, f: ExprId) -> Result<u64, Declined> {
        let mut n = 0;
        for (from, to, out, _) in self.edges(r) {
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
                for (from, to, out, _) in self.edges(*r) {
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
                self.edges(*r)
                    .iter()
                    .any(|&(from, to, out, _)| from == x && to == y && out > 0)
            }
            // No block runs from a proxy to itself: no element is its own neighbour.
            ClassExpr::HasSelf(_) => false,
            ClassExpr::Min(n, r, f) => self.count(o, x, *r, *f)? >= u64::from(*n),
            ClassExpr::Max(n, r, f) => self.count(o, x, *r, *f)? <= u64::from(*n),
            ClassExpr::Exact(n, r, f) => self.count(o, x, *r, *f)? == u64::from(*n),
            other => return Err(Declined(format!("the check doesn't cover {other:?}"))),
        })
    }

    /// Whether every element of every proxy has at most one `r`-neighbour.
    fn functional(&self, r: ObjProp) -> bool {
        (0..self.c.proxies.len()).all(|x| {
            self.edges(r)
                .iter()
                .filter(|e| e.0 == x)
                .map(|e| e.2)
                .sum::<u64>()
                <= 1
        })
    }

    /// The proxies with an `r`-neighbour.
    fn with_neighbours(&self, r: ObjProp) -> HashSet<usize> {
        self.edges(r)
            .iter()
            .filter(|e| e.2 > 0)
            .map(|e| e.0)
            .collect()
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
    // Blocks: realisable, and one per property and pair of proxies.
    let inverse: HashMap<Term, Term> = ontology
        .axioms
        .iter()
        .filter_map(|a| match *a {
            Axiom::InverseObjectProperties(ObjProp::Named(a), ObjProp::Named(b)) => Some((a, b)),
            _ => None,
        })
        .flat_map(|(a, b)| [(a, b), (b, a)])
        .collect();
    let mut pairs = HashSet::new();
    let mut edges: HashMap<Term, Vec<Edges>> = HashMap::new();
    for b in &candidate.blocks {
        let (Some(f), Some(t)) = (proxies.get(b.from), proxies.get(b.to)) else {
            return no("a block between proxies that aren't there".into());
        };
        let realisable = b.from != b.to
            && b.out_degree.checked_mul(f.multiplicity) == b.in_degree.checked_mul(t.multiplicity)
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
            Axiom::ObjectCharacteristic(Characteristic::Functional, r) => m.functional(*r),
            Axiom::ObjectCharacteristic(Characteristic::InverseFunctional, r) => {
                m.functional(r.inverse())
            }
            Axiom::ObjectPropertyDomain(r, c) | Axiom::ObjectPropertyRange(r, c) => {
                let r = match axiom {
                    Axiom::ObjectPropertyDomain(..) => *r,
                    _ => r.inverse(),
                };
                let v = every(*c)?;
                m.with_neighbours(r).iter().all(|&x| v[x])
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
    use super::super::candidate::{Block, construct};
    use super::super::closure::close;
    use super::super::problem::{extract, tests::product};
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
}
