//! What isn't well-formed OWL 2 DL in an ontology read from RDF: reported with the terms
//! involved, never dropped silently. A task an affected axiom takes part in is answered
//! `unsupported` (docs/design/owl2-dl.md §2).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::mapping::{Ontology, Terms};
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
    /// A global restriction of OWL 2 DL broken: an anonymous individual in a nominal or a
    /// `hasValue`, which take named ones only (*OWL 2 Structural Specification* §11.2).
    AnonymousIndividual { node: Term },
    /// Text the functional-syntax reader can't read (`crate::ofn`), where it is (from 1;
    /// the column in characters): the axiom it is in is left out.
    Syntax {
        line: u32,
        column: u32,
        what: &'static str,
    },
    /// A disjointness that repeats an operand (`x owl:disjointWith x`, `a owl:differentFrom
    /// a`): OWL 2 DL's n-ary axioms take operands without repeats, so it is read as what
    /// being disjoint from itself means (`crate::model::Disjoint`).
    RepeatedOperand,
    /// An IRI of the reserved vocabulary used as a class, property or individual (*OWL 2
    /// Structural Specification* §2.4): the axioms that do are left out.
    Reserved { term: Term },
    /// A DL-safe rule (SWRL, not OWL 2: *OWL 2 Structural Specification* §9 has no rule
    /// axiom), kept in [`Ontology::rules`] at this index.
    Rule { rule: usize },
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
            Self::AnonymousIndividual { node } => (node, "anonymous individual"),
            Self::Syntax { line, column, what } => {
                ((u64::from(line) << 32) | u64::from(column), what)
            }
            Self::Rule { rule } => (rule as Term, "rule"),
            Self::RepeatedOperand => (0, "repeated operand"),
            Self::Reserved { term } => (term, "reserved"),
        }
    }

    /// Whether reasoning over the ontology may go wrong because of it. Not for what OWL 2
    /// DL forbids but whose meaning is unambiguous: an undeclared property (typed by its
    /// use), a blank node shared by expressions (OWL 1 ontologies do it; the expression
    /// is the same), a structure no axiom uses (it says nothing).
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::UndeclaredProperty { .. }
                | Self::SharedBlankNode { .. }
                | Self::Unused { .. }
                | Self::RepeatedOperand
        )
    }
}

/// Reports the axioms that use non-simple properties where OWL 2 DL requires simple
/// ones. A property is non-simple if a composite one is (through the hierarchy) a
/// subproperty of it; so is its inverse. Composite (*OWL 2 Structural Specification*
/// §11.1): a chain's or a transitive property's superproperty, and
/// `owl:topObjectProperty`. (§11.1 makes `owl:bottomObjectProperty` composite too; it
/// relates nothing, so nothing above it gets a pair from it, and it isn't counted.)
pub(crate) fn check_global_restrictions(ontology: &mut Ontology) {
    // Nominals and `hasValue` take named individuals only (§11.2).
    let anonymous: BTreeSet<Term> = crate::mapping::in_expressions(ontology)
        .filter(|t| ontology.anonymous.contains(t))
        .collect();
    for node in anonymous {
        ontology
            .diagnostics
            .push(Diagnostic::AnonymousIndividual { node });
    }
    let mut supers: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut seeds: Vec<Term> = ontology.builtin.top_object.into_iter().collect();
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
pub(crate) fn expressions_of(axiom: &Axiom) -> Vec<u32> {
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

/// Leaves out the axioms that use an IRI of the reserved vocabulary (`rdf:`, `rdfs:`,
/// `xsd:`, `owl:`; *OWL 2 Structural Specification* §2.4) as a class, a property or an
/// individual, but for those §2.4 gives OWL 2 DL (`owl:Thing`, `owl:Nothing`, the top and
/// bottom properties; datatypes are data ranges, not touched here): each with a
/// [`Diagnostic::Reserved`]. Such a name means in OWL 2 DL nothing of what it means in
/// RDF (`rdfs:Class`, `rdf:type`, `rdf:first` as entities belong to OWL 2 Full), so
/// reading it as a fresh name would decide what the input doesn't say; left out, what
/// follows from the rest still follows. Terms whose IRI the source doesn't give
/// ([`Terms::iri_text`]) aren't checked.
pub(crate) fn leave_out_reserved(ontology: &mut Ontology, terms: &dyn Terms) {
    use crate::vocab::{OWL, RDF, RDFS, XSD};
    let b = ontology.builtin;
    let allowed = [
        b.top_object,
        b.bottom_object,
        b.top_data,
        b.bottom_data,
        terms.iri(&format!("{OWL}Thing")),
        terms.iri(&format!("{OWL}Nothing")),
    ];
    let mut cache: HashMap<Term, bool> = HashMap::new();
    let mut reserved = |t: Term| -> bool {
        *cache.entry(t).or_insert_with(|| {
            !allowed.contains(&Some(t))
                && terms
                    .iri_text(t)
                    .is_some_and(|iri| [RDF, RDFS, XSD, OWL].iter().any(|ns| iri.starts_with(ns)))
        })
    };
    let mut kept = Vec::with_capacity(ontology.axioms.len());
    let mut sources = Vec::with_capacity(ontology.axioms.len());
    let mut found = Vec::new();
    let axioms = std::mem::take(&mut ontology.axioms);
    let all_sources = std::mem::take(&mut ontology.sources);
    for (axiom, source) in axioms.into_iter().zip(all_sources) {
        let names = entities_of(ontology, &axiom);
        match names.into_iter().find(|&t| reserved(t)) {
            Some(term) => found.push(Diagnostic::Reserved { term }),
            None => {
                kept.push(axiom);
                sources.push(source);
            }
        }
    }
    ontology.axioms = kept;
    ontology.sources = sources;
    for d in found {
        if !ontology.diagnostics.contains(&d) {
            ontology.diagnostics.push(d);
        }
    }
}

/// The classes, properties and individuals an axiom names (datatypes, literals and
/// facets aside).
fn entities_of(o: &Ontology, axiom: &Axiom) -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    let p = |q: &ObjProp| q.named();
    match axiom {
        Axiom::Declaration(kind, t) => {
            // Datatypes and annotation properties of the reserved vocabulary are OWL 2's.
            if !matches!(
                kind,
                crate::model::EntityKind::Datatype | crate::model::EntityKind::AnnotationProperty
            ) {
                out.push(*t);
            }
        }
        Axiom::DisjointUnion(c, _) => out.push(*c),
        Axiom::SubObjectPropertyOf(chain, sup) => {
            out.extend(chain.iter().map(p));
            out.push(sup.named());
        }
        Axiom::EquivalentObjectProperties(ps) | Axiom::DisjointObjectProperties(ps) => {
            out.extend(ps.iter().map(p));
        }
        Axiom::InverseObjectProperties(a, b2) => out.extend([a.named(), b2.named()]),
        Axiom::ObjectPropertyDomain(q, _)
        | Axiom::ObjectPropertyRange(q, _)
        | Axiom::ObjectCharacteristic(_, q) => out.push(q.named()),
        Axiom::SubDataPropertyOf(a, b2) => out.extend([*a, *b2]),
        Axiom::EquivalentDataProperties(ps) | Axiom::DisjointDataProperties(ps) => {
            out.extend(ps);
        }
        Axiom::DataPropertyDomain(q, _)
        | Axiom::DataPropertyRange(q, _)
        | Axiom::FunctionalDataProperty(q) => out.push(*q),
        Axiom::HasKey(_, os, ds) => {
            out.extend(os.iter().map(p));
            out.extend(ds);
        }
        Axiom::ClassAssertion(_, a) => out.push(*a),
        Axiom::ObjectPropertyAssertion(q, a, b2)
        | Axiom::NegativeObjectPropertyAssertion(q, a, b2) => out.extend([*q, *a, *b2]),
        Axiom::DataPropertyAssertion(q, a, _) | Axiom::NegativeDataPropertyAssertion(q, a, _) => {
            out.extend([*q, *a]);
        }
        Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => out.extend(xs),
        _ => {}
    }
    let mut stack: Vec<u32> = expressions_of(axiom);
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        match o.classes.get(id) {
            ClassExpr::Class(t) => out.push(*t),
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs.iter().map(|x| x.0)),
            ClassExpr::Not(x) => stack.push(x.0),
            ClassExpr::OneOf(xs) => out.extend(xs),
            ClassExpr::Some(q, x)
            | ClassExpr::All(q, x)
            | ClassExpr::Min(_, q, x)
            | ClassExpr::Max(_, q, x)
            | ClassExpr::Exact(_, q, x) => {
                out.push(q.named());
                stack.push(x.0);
            }
            ClassExpr::HasValue(q, a) => out.extend([q.named(), *a]),
            ClassExpr::HasSelf(q) => out.push(q.named()),
            ClassExpr::DataSome(q, _)
            | ClassExpr::DataAll(q, _)
            | ClassExpr::DataHasValue(q, _)
            | ClassExpr::DataMin(_, q, _)
            | ClassExpr::DataMax(_, q, _)
            | ClassExpr::DataExact(_, q, _) => out.push(*q),
            ClassExpr::Thing | ClassExpr::Nothing => {}
        }
    }
    out
}
