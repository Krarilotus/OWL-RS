//! The OWL 2 profiles per axiom (W3C *OWL 2 Web Ontology Language Profiles*, second
//! edition): which of EL (§2), QL (§3) and RL (§4) an axiom's grammar admits, and so
//! which profiles an ontology is in. The store routes by it: an RL ontology is answered
//! by the RL rules (complete for it, theorem PR1), EL and Horn parts by the context core,
//! the rest by the hypertableau (docs/design/owl2-dl.md §4).
//!
//! The grammars are checked as written, by position: QL and RL distinguish the class
//! expressions allowed on the left of a subclass axiom (`subClassExpression`), on its
//! right (`superClassExpression`) and in an equivalence (RL's `equivClassExpression`),
//! and each profile has its datatypes. Not checked: the global restrictions beyond the
//! grammar (EL's on property chains and ranges, QL's and RL's on anonymous individuals in
//! assertions); an axiom outside every profile is still OWL 2 DL.

use crate::model::{Axiom, Characteristic, ClassExpr, DataRange, ExprId, ObjProp, RangeId};
use crate::{Ontology, Term};

/// The profiles an axiom (or an ontology: each of its axioms) is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Profiles {
    pub el: bool,
    pub ql: bool,
    pub rl: bool,
}

impl Profiles {
    pub const ALL: Self = Self {
        el: true,
        ql: true,
        rl: true,
    };

    /// In both.
    pub fn and(self, other: Self) -> Self {
        Self {
            el: self.el && other.el,
            ql: self.ql && other.ql,
            rl: self.rl && other.rl,
        }
    }

    /// The names of the profiles it is in (`EL`, `QL`, `RL`).
    pub fn names(self) -> Vec<&'static str> {
        [(self.el, "EL"), (self.ql, "QL"), (self.rl, "RL")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect()
    }
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// EL's and QL's datatypes (§2.2.1, §3.2.1).
const EL_QL_DATATYPES: &[&str] = &[
    "rdf:PlainLiteral",
    "rdf:XMLLiteral",
    "rdfs:Literal",
    "owl:real",
    "owl:rational",
    "xsd:decimal",
    "xsd:integer",
    "xsd:nonNegativeInteger",
    "xsd:string",
    "xsd:normalizedString",
    "xsd:token",
    "xsd:Name",
    "xsd:NCName",
    "xsd:NMTOKEN",
    "xsd:hexBinary",
    "xsd:base64Binary",
    "xsd:anyURI",
    "xsd:dateTime",
    "xsd:dateTimeStamp",
];

/// RL's: the OWL 2 datatype map without `owl:real` and `owl:rational` (§4.2).
const NOT_RL_DATATYPES: &[&str] = &["owl:real", "owl:rational"];

/// The short name of a datatype in one of the standard vocabularies, if it is in one.
fn short(iri: &str) -> Option<String> {
    for (ns, prefix) in [(XSD, "xsd"), (RDF, "rdf"), (RDFS, "rdfs"), (OWL, "owl")] {
        if let Some(local) = iri.strip_prefix(ns) {
            return Some(format!("{prefix}:{local}"));
        }
    }
    None
}

/// Which profiles' datatypes include `datatype` (a datatype the ontology defines itself,
/// outside the standard vocabularies, counts as in every profile's).
fn datatype(o: &Ontology, datatype: Term) -> Profiles {
    let Some(name) = o.data.iris.get(&datatype).and_then(|iri| short(iri)) else {
        return Profiles::ALL;
    };
    let el_ql = EL_QL_DATATYPES.contains(&name.as_str());
    Profiles {
        el: el_ql,
        ql: el_ql,
        rl: !NOT_RL_DATATYPES.contains(&name.as_str()),
    }
}

/// Which profiles admit the data range `r`.
fn range(o: &Ontology, r: RangeId) -> Profiles {
    match o.range(r) {
        DataRange::Literal => Profiles::ALL,
        DataRange::Datatype(t) => datatype(o, *t),
        DataRange::And(rs) => rs
            .iter()
            .fold(Profiles::ALL, |acc, &r| acc.and(range(o, r))),
        // EL: one literal only.
        DataRange::OneOf(xs) => Profiles {
            el: xs.len() == 1,
            ql: false,
            rl: false,
        },
        DataRange::Or(_) | DataRange::Not(_) | DataRange::Restriction(..) => Profiles {
            el: false,
            ql: false,
            rl: false,
        },
    }
}

fn named(p: ObjProp) -> bool {
    matches!(p, ObjProp::Named(_))
}

/// `e`, through intersections of one operand (an OWL 1 form the reader keeps), which
/// mean their operand.
fn unwrap(o: &Ontology, mut e: ExprId) -> ExprId {
    while let ClassExpr::And(es) = o.class(e) {
        match es.as_slice() {
            [only] => e = *only,
            _ => break,
        }
    }
    e
}

/// Whether EL admits the class expression `e` (EL has one grammar for every position).
fn el(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing => true,
        ClassExpr::And(es) => es.iter().all(|&e| el(o, e)),
        ClassExpr::Some(p, f) => named(*p) && el(o, *f),
        ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => named(*p),
        ClassExpr::OneOf(xs) => xs.len() == 1,
        ClassExpr::DataSome(_, r) => range(o, *r).el,
        ClassExpr::DataHasValue(..) => true,
        _ => false,
    }
}

/// QL's `subClassExpression` (§3.2.3): a class, `∃R.⊤`, `∃D.Literal`.
fn ql_sub(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing => true,
        ClassExpr::Some(_, f) => matches!(o.class(*f), ClassExpr::Thing),
        ClassExpr::DataSome(_, r) => matches!(o.range(*r), DataRange::Literal),
        _ => false,
    }
}

/// QL's `superClassExpression`: a class, an intersection of them, the complement of a
/// `subClassExpression`, `∃R.A`, `∃D.range`.
fn ql_super(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing => true,
        ClassExpr::And(es) => es.iter().all(|&e| ql_super(o, e)),
        ClassExpr::Not(e) => ql_sub(o, *e),
        ClassExpr::Some(_, f) => matches!(o.class(*f), ClassExpr::Class(_) | ClassExpr::Thing),
        ClassExpr::DataSome(_, r) => range(o, *r).ql,
        _ => false,
    }
}

/// RL's `subClassExpression` (§4.2.3): a class other than `owl:Thing`, intersections and
/// unions of them, enumerations, `∃R.sub` (or `∃R.⊤`), `∃D.range`, values.
fn rl_sub(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Nothing => true,
        ClassExpr::And(es) | ClassExpr::Or(es) => es.iter().all(|&e| rl_sub(o, e)),
        ClassExpr::OneOf(_) => true,
        ClassExpr::Some(_, f) => matches!(o.class(*f), ClassExpr::Thing) || rl_sub(o, *f),
        ClassExpr::DataSome(_, r) => range(o, *r).rl,
        ClassExpr::HasValue(..) | ClassExpr::DataHasValue(..) => true,
        _ => false,
    }
}

/// RL's `superClassExpression`: a class other than `owl:Thing`, intersections, the
/// complement of a `subClassExpression`, `∀R.super`, values, `≤ 0/1 R(.sub)`, `∀D.range`,
/// `≤ 0/1 D(.range)`.
fn rl_super(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Nothing => true,
        ClassExpr::And(es) => es.iter().all(|&e| rl_super(o, e)),
        ClassExpr::Not(e) => rl_sub(o, *e),
        ClassExpr::All(_, f) => rl_super(o, *f),
        ClassExpr::HasValue(..) | ClassExpr::DataHasValue(..) => true,
        ClassExpr::Max(n, _, f) => {
            *n <= 1 && (matches!(o.class(*f), ClassExpr::Thing) || rl_sub(o, *f))
        }
        ClassExpr::DataAll(_, r) => range(o, *r).rl,
        ClassExpr::DataMax(n, _, r) => *n <= 1 && range(o, *r).rl,
        _ => false,
    }
}

/// RL's `equivClassExpression`: a class other than `owl:Thing`, intersections, values.
fn rl_equiv(o: &Ontology, e: ExprId) -> bool {
    let e = unwrap(o, e);
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Nothing => true,
        ClassExpr::And(es) => es.iter().all(|&e| rl_equiv(o, e)),
        ClassExpr::HasValue(..) | ClassExpr::DataHasValue(..) => true,
        _ => false,
    }
}

/// The individuals an assertion names.
fn individuals(axiom: &Axiom) -> Vec<Term> {
    match axiom {
        Axiom::ClassAssertion(_, a) => vec![*a],
        Axiom::ObjectPropertyAssertion(_, a, b)
        | Axiom::NegativeObjectPropertyAssertion(_, a, b) => {
            vec![*a, *b]
        }
        Axiom::DataPropertyAssertion(_, a, _) | Axiom::NegativeDataPropertyAssertion(_, a, _) => {
            vec![*a]
        }
        Axiom::SameIndividual(v) | Axiom::DifferentIndividuals(v) => v.clone(),
        _ => Vec::new(),
    }
}

/// The properties an expression names, with those of its fillers.
fn expr_properties(o: &Ontology, e: ExprId, out: &mut Vec<Term>) {
    match o.class(e) {
        ClassExpr::And(es) | ClassExpr::Or(es) => {
            for &e in es {
                expr_properties(o, e, out);
            }
        }
        ClassExpr::Not(e) => expr_properties(o, *e, out),
        ClassExpr::Some(p, f)
        | ClassExpr::All(p, f)
        | ClassExpr::Min(_, p, f)
        | ClassExpr::Max(_, p, f)
        | ClassExpr::Exact(_, p, f) => {
            out.push(p.named());
            expr_properties(o, *f, out);
        }
        ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => out.push(p.named()),
        ClassExpr::DataSome(d, _)
        | ClassExpr::DataAll(d, _)
        | ClassExpr::DataHasValue(d, _)
        | ClassExpr::DataMin(_, d, _)
        | ClassExpr::DataMax(_, d, _)
        | ClassExpr::DataExact(_, d, _) => out.push(*d),
        _ => {}
    }
}

/// The properties an axiom names, in its expressions too.
fn properties(o: &Ontology, axiom: &Axiom) -> Vec<Term> {
    let mut out = Vec::new();
    let mut exprs: Vec<ExprId> = Vec::new();
    match axiom {
        Axiom::SubClassOf(a, b) => exprs.extend([*a, *b]),
        Axiom::EquivalentClasses(v) | Axiom::DisjointClasses(v) | Axiom::DisjointUnion(_, v) => {
            exprs.extend(v)
        }
        Axiom::SubObjectPropertyOf(chain, q) => {
            out.extend(chain.iter().map(|p| p.named()));
            out.push(q.named());
        }
        Axiom::EquivalentObjectProperties(v) | Axiom::DisjointObjectProperties(v) => {
            out.extend(v.iter().map(|p| p.named()))
        }
        Axiom::InverseObjectProperties(p, q) => out.extend([p.named(), q.named()]),
        Axiom::ObjectPropertyDomain(p, c) | Axiom::ObjectPropertyRange(p, c) => {
            out.push(p.named());
            exprs.push(*c);
        }
        Axiom::ObjectCharacteristic(_, p) => out.push(p.named()),
        Axiom::SubDataPropertyOf(a, b) => out.extend([*a, *b]),
        Axiom::EquivalentDataProperties(v) | Axiom::DisjointDataProperties(v) => out.extend(v),
        Axiom::DataPropertyDomain(d, c) => {
            out.push(*d);
            exprs.push(*c);
        }
        Axiom::DataPropertyRange(d, _) | Axiom::FunctionalDataProperty(d) => out.push(*d),
        Axiom::HasKey(c, ps, ds) => {
            exprs.push(*c);
            out.extend(ps.iter().map(|p| p.named()));
            out.extend(ds);
        }
        Axiom::ClassAssertion(c, _) => exprs.push(*c),
        Axiom::ObjectPropertyAssertion(p, ..)
        | Axiom::NegativeObjectPropertyAssertion(p, ..)
        | Axiom::DataPropertyAssertion(p, ..)
        | Axiom::NegativeDataPropertyAssertion(p, ..) => out.push(*p),
        _ => {}
    }
    for e in exprs {
        expr_properties(o, e, &mut out);
    }
    out
}

/// The profiles that admit `axiom` (of `o`'s interners). EL and QL admit no anonymous
/// individual (`Individual := NamedIndividual`); RL admits none of the built-in top and
/// bottom properties (§4.2.1).
pub fn of(o: &Ontology, axiom: &Axiom) -> Profiles {
    let mut profiles = grammar(o, axiom);
    if individuals(axiom).iter().any(|a| o.anonymous.contains(a)) {
        profiles.el = false;
        profiles.ql = false;
    }
    let b = o.builtin;
    let builtin = [b.top_object, b.bottom_object, b.top_data, b.bottom_data];
    if properties(o, axiom)
        .iter()
        .any(|p| builtin.contains(&Some(*p)))
    {
        profiles.rl = false;
    }
    profiles
}

/// The profiles whose grammar admits `axiom`.
fn grammar(o: &Ontology, axiom: &Axiom) -> Profiles {
    let all = |v: &[ExprId], f: &dyn Fn(ExprId) -> bool| v.iter().all(|&e| f(e));
    let el_ = |e: ExprId| el(o, e);
    let none = Profiles {
        el: false,
        ql: false,
        rl: false,
    };
    match axiom {
        Axiom::Declaration(..) => Profiles::ALL,
        Axiom::SubClassOf(a, b) => Profiles {
            el: el_(*a) && el_(*b),
            ql: ql_sub(o, *a) && ql_super(o, *b),
            rl: rl_sub(o, *a) && rl_super(o, *b),
        },
        Axiom::EquivalentClasses(v) => Profiles {
            el: all(v, &el_),
            ql: all(v, &|e| ql_sub(o, e)),
            rl: all(v, &|e| rl_equiv(o, e)),
        },
        Axiom::DisjointClasses(v) => Profiles {
            el: all(v, &el_),
            ql: all(v, &|e| ql_sub(o, e)),
            rl: all(v, &|e| rl_sub(o, e)),
        },
        Axiom::DisjointUnion(..) => none,
        Axiom::SubObjectPropertyOf(chain, q) => Profiles {
            el: chain.iter().all(|&p| named(p)) && named(*q),
            ql: chain.len() == 1,
            rl: true,
        },
        Axiom::EquivalentObjectProperties(v) => Profiles {
            el: v.iter().all(|&p| named(p)),
            ql: true,
            rl: true,
        },
        Axiom::DisjointObjectProperties(_) | Axiom::InverseObjectProperties(..) => Profiles {
            el: false,
            ql: true,
            rl: true,
        },
        Axiom::ObjectPropertyDomain(p, c) | Axiom::ObjectPropertyRange(p, c) => Profiles {
            el: named(*p) && el_(*c),
            ql: ql_super(o, *c),
            rl: rl_super(o, *c),
        },
        Axiom::ObjectCharacteristic(c, p) => match c {
            Characteristic::Reflexive => Profiles {
                el: named(*p),
                ql: true,
                rl: false,
            },
            Characteristic::Transitive => Profiles {
                el: named(*p),
                ql: false,
                rl: true,
            },
            Characteristic::Symmetric | Characteristic::Asymmetric => Profiles {
                el: false,
                ql: true,
                rl: true,
            },
            Characteristic::Irreflexive => Profiles {
                el: false,
                ql: false,
                rl: true,
            },
            Characteristic::Functional | Characteristic::InverseFunctional => Profiles {
                el: false,
                ql: false,
                rl: true,
            },
        },
        Axiom::SubDataPropertyOf(..) | Axiom::EquivalentDataProperties(_) => Profiles::ALL,
        Axiom::DisjointDataProperties(_) => Profiles {
            el: false,
            ql: true,
            rl: true,
        },
        Axiom::DataPropertyDomain(_, c) => Profiles {
            el: el_(*c),
            ql: ql_super(o, *c),
            rl: rl_super(o, *c),
        },
        Axiom::DataPropertyRange(_, r) => range(o, *r),
        Axiom::FunctionalDataProperty(_) => Profiles {
            el: true,
            ql: false,
            rl: true,
        },
        Axiom::DatatypeDefinition(_, r) => Profiles {
            rl: false,
            ..range(o, *r)
        },
        Axiom::HasKey(c, ps, _) => Profiles {
            el: el_(*c) && ps.iter().all(|&p| named(p)),
            ql: false,
            rl: rl_sub(o, *c),
        },
        Axiom::ClassAssertion(c, _) => Profiles {
            el: el_(*c),
            ql: matches!(
                o.class(unwrap(o, *c)),
                ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing
            ),
            rl: rl_super(o, *c),
        },
        Axiom::ObjectPropertyAssertion(..)
        | Axiom::DataPropertyAssertion(..)
        | Axiom::DifferentIndividuals(_) => Profiles::ALL,
        Axiom::NegativeObjectPropertyAssertion(..)
        | Axiom::NegativeDataPropertyAssertion(..)
        | Axiom::SameIndividual(_) => Profiles {
            el: true,
            ql: false,
            rl: true,
        },
    }
}

/// The profiles every axiom of `o` is in.
pub fn of_ontology(o: &Ontology) -> Profiles {
    o.axioms
        .iter()
        .fold(Profiles::ALL, |acc, a| acc.and(of(o, a)))
}
