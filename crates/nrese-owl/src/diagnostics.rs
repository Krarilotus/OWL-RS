//! What isn't well-formed OWL 2 DL in an ontology read from RDF: reported with the terms
//! involved, never dropped silently. A task an affected axiom takes part in is answered
//! `unsupported` (docs/design/owl2-dl.md §2).

use std::collections::{HashMap, HashSet};

use crate::mapping::Ontology;
use crate::model::{Axiom, Characteristic, ClassExpr, ObjProp, Term};

/// A problem found while reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// A structure that doesn't match the mapping.
    Malformed { node: Term, what: &'static str },
    /// A blank node used by two expressions (OWL 2 allows one).
    SharedBlankNode { node: Term },
    /// A list that doesn't end in `rdf:nil`, has a cell twice, or a cell without
    /// `rdf:first` and `rdf:rest`.
    BrokenList { head: Term },
    /// A property used without a declaration (typed by its use).
    UndeclaredProperty { property: Term },
    /// Declared both an object and a data property.
    AmbiguousProperty { property: Term },
    /// OWL 2 syntax NRESE doesn't read yet.
    Unsupported { node: Term, what: &'static str },
    /// OWL vocabulary used outside any form of the mapping.
    NotOwl { triple: [Term; 3] },
    /// A structure (an expression, a list) no axiom uses: it says nothing under the
    /// direct semantics (OWL 1 conclusions state expressions so).
    Unused { node: Term },
    /// A global restriction of OWL 2 DL broken: a non-simple property where only simple
    /// ones may be (cardinalities, `Self`, functional, inverse-functional, irreflexive,
    /// asymmetric, disjoint properties). `axiom` indexes [`Ontology::axioms`].
    NonSimpleProperty {
        property: Term,
        axiom: usize,
        what: &'static str,
    },
}

impl Diagnostic {
    /// What makes two reports the same one.
    pub(crate) fn key(&self) -> (Term, &'static str) {
        match *self {
            Self::Malformed { node, what } => (node, what),
            Self::SharedBlankNode { node } => (node, "shared"),
            Self::BrokenList { head } => (head, "list"),
            Self::UndeclaredProperty { property } => (property, "undeclared"),
            Self::AmbiguousProperty { property } => (property, "ambiguous"),
            Self::Unsupported { node, what } => (node, what),
            Self::NotOwl { triple } => (
                triple[0] ^ triple[1].rotate_left(21) ^ triple[2].rotate_left(42),
                "not owl",
            ),
            Self::Unused { node } => (node, "unused"),
            Self::NonSimpleProperty { property, what, .. } => (property, what),
        }
    }

    /// Whether reasoning over the ontology may go wrong because of it. Not for what OWL 2
    /// DL forbids but whose meaning is unambiguous: an undeclared property (typed by its
    /// use), a blank node shared by expressions (OWL 1 ontologies do it; the expression
    /// is the same), a structure no axiom uses (it says nothing).
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::UndeclaredProperty { .. } | Self::SharedBlankNode { .. } | Self::Unused { .. }
        )
    }
}

/// Reports the axioms that use non-simple properties where OWL 2 DL requires simple
/// ones. A property is non-simple if a chain of two or more, or a transitive property,
/// is (through the hierarchy) a subproperty of it; so is its inverse.
pub(crate) fn check_global_restrictions(ontology: &mut Ontology) {
    let mut supers: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut seeds: Vec<Term> = Vec::new();
    for axiom in &ontology.axioms {
        match axiom {
            Axiom::SubObjectPropertyOf(chain, sup) => {
                if chain.len() > 1 {
                    seeds.push(sup.named());
                } else {
                    supers
                        .entry(chain[0].named())
                        .or_default()
                        .push(sup.named());
                }
            }
            Axiom::EquivalentObjectProperties(properties) => {
                for a in properties {
                    for b in properties {
                        if a != b {
                            supers.entry(a.named()).or_default().push(b.named());
                        }
                    }
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                supers.entry(a.named()).or_default().push(b.named());
                supers.entry(b.named()).or_default().push(a.named());
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => seeds.push(p.named()),
            _ => {}
        }
    }
    let mut non_simple: HashSet<Term> = HashSet::new();
    while let Some(property) = seeds.pop() {
        if non_simple.insert(property) {
            seeds.extend(supers.get(&property).into_iter().flatten().copied());
        }
    }
    if non_simple.is_empty() {
        return;
    }
    let simple = |p: &ObjProp| !non_simple.contains(&p.named());
    // Class expressions that need a simple property.
    let mut needing: HashMap<u32, (Term, &'static str)> = HashMap::new();
    for id in 0..ontology.classes.len() as u32 {
        let (property, what) = match ontology.classes.get(id) {
            ClassExpr::Min(_, p, _) | ClassExpr::Max(_, p, _) | ClassExpr::Exact(_, p, _) => {
                (*p, "a cardinality restriction")
            }
            ClassExpr::HasSelf(p) => (*p, "a self restriction"),
            _ => continue,
        };
        if !simple(&property) {
            needing.insert(id, (property.named(), what));
        }
    }
    let mut found = Vec::new();
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        let direct = match axiom {
            Axiom::ObjectCharacteristic(
                kind @ (Characteristic::Functional
                | Characteristic::InverseFunctional
                | Characteristic::Irreflexive
                | Characteristic::Asymmetric),
                p,
            ) if !simple(p) => Some((
                p.named(),
                match kind {
                    Characteristic::Functional => "a functional property",
                    Characteristic::InverseFunctional => "an inverse-functional property",
                    Characteristic::Irreflexive => "an irreflexive property",
                    _ => "an asymmetric property",
                },
            )),
            Axiom::DisjointObjectProperties(properties) => properties
                .iter()
                .find(|p| !simple(p))
                .map(|p| (p.named(), "disjoint properties")),
            _ => None,
        };
        if let Some((property, what)) = direct {
            found.push(Diagnostic::NonSimpleProperty {
                property,
                axiom: index,
                what,
            });
            continue;
        }
        for expr in expressions_of(axiom) {
            if let Some(&(property, what)) = uses_any(ontology, expr, &needing) {
                found.push(Diagnostic::NonSimpleProperty {
                    property,
                    axiom: index,
                    what,
                });
                break;
            }
        }
    }
    ontology.diagnostics.extend(found);
}

/// The class expressions an axiom holds directly.
fn expressions_of(axiom: &Axiom) -> Vec<u32> {
    match axiom {
        Axiom::SubClassOf(a, b) => vec![a.0, b.0],
        Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => {
            xs.iter().map(|x| x.0).collect()
        }
        Axiom::DisjointUnion(_, xs) => xs.iter().map(|x| x.0).collect(),
        Axiom::ObjectPropertyDomain(_, x)
        | Axiom::ObjectPropertyRange(_, x)
        | Axiom::DataPropertyDomain(_, x)
        | Axiom::HasKey(x, _, _)
        | Axiom::ClassAssertion(x, _) => vec![x.0],
        _ => Vec::new(),
    }
}

/// The first expression in `id`'s subexpressions that `needing` names.
fn uses_any<'a>(
    ontology: &Ontology,
    id: u32,
    needing: &'a HashMap<u32, (Term, &'static str)>,
) -> Option<&'a (Term, &'static str)> {
    let mut stack = vec![id];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(found) = needing.get(&id) {
            return Some(found);
        }
        match ontology.classes.get(id) {
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs.iter().map(|x| x.0)),
            ClassExpr::Not(x)
            | ClassExpr::Some(_, x)
            | ClassExpr::All(_, x)
            | ClassExpr::Min(_, _, x)
            | ClassExpr::Max(_, _, x)
            | ClassExpr::Exact(_, _, x) => stack.push(x.0),
            _ => {}
        }
    }
    None
}
