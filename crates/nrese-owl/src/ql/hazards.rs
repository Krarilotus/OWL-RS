//! What the rewriting doesn't follow (docs/design/ql-rewriting.md §7): axioms outside OWL 2
//! QL that can meet the anonymous individuals the rewriting reasons about, and the terms
//! whose answers may then be missing. Conservative: a term may be marked that loses
//! nothing, never the other way round.

use std::collections::{HashMap, HashSet, VecDeque};

use super::tbox::{Basic, Type};
use crate::mapping::Ontology;
use crate::model::{Axiom, Characteristic, ClassExpr, ExprId, Term};

/// An axiom the rewriting doesn't follow, by the property or class it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hazard {
    Transitive(Term),
    /// In a property chain, as a link or as its result.
    Chain(Term),
    /// Functional or inverse functional.
    Functional(Term),
    /// A class with a key.
    Key(Term),
    /// Under a cardinality restriction with a maximum.
    Cardinality(Term),
    /// Under `allValuesFrom`.
    Universal(Term),
    /// Under `hasValue`.
    Value(Term),
    /// Under `hasSelf`.
    SelfRestriction(Term),
    /// Under a qualified `someValuesFrom` on the left of an axiom.
    LeftExistential(Term),
    /// In an intersection, a filler or a complex expression on the left of an axiom.
    LeftClass(Term),
    /// A class contained in a union or an enumeration.
    Union(Term),
    Enumeration(Term),
    /// The role of an existential whose filler the rewriting reads only in part.
    Filler(Term),
    /// The role of an existential an individual is asserted to be in.
    Asserted(Term),
}

impl Hazard {
    /// The property or class it is about.
    pub fn term(self) -> Term {
        match self {
            Self::Transitive(t)
            | Self::Chain(t)
            | Self::Functional(t)
            | Self::Key(t)
            | Self::Cardinality(t)
            | Self::Universal(t)
            | Self::Value(t)
            | Self::SelfRestriction(t)
            | Self::LeftExistential(t)
            | Self::LeftClass(t)
            | Self::Union(t)
            | Self::Enumeration(t)
            | Self::Filler(t)
            | Self::Asserted(t) => t,
        }
    }

    /// Whether it can make an anonymous individual equal to another: then any fact may
    /// follow.
    pub fn equates(self) -> bool {
        matches!(
            self,
            Self::Functional(_) | Self::Key(_) | Self::Cardinality(_) | Self::Enumeration(_)
        )
    }

    /// What it is, with `name` writing its term.
    pub fn describe(self, name: &dyn Fn(Term) -> String) -> String {
        let t = name(self.term());
        match self {
            Self::Transitive(_) => format!("{t} is transitive"),
            Self::Chain(_) => format!("{t} is in a property chain"),
            Self::Functional(_) => format!("{t} is functional or inverse functional"),
            Self::Key(_) => format!("{t} has a key"),
            Self::Cardinality(_) => format!("a maximum cardinality on {t}"),
            Self::Universal(_) => format!("an allValuesFrom restriction on {t}"),
            Self::Value(_) => format!("a hasValue restriction on {t}"),
            Self::SelfRestriction(_) => format!("a hasSelf restriction on {t}"),
            Self::LeftExistential(_) => {
                format!("a qualified someValuesFrom on {t} on the left of an axiom")
            }
            Self::LeftClass(_) => format!("{t} in a complex class expression on the left"),
            Self::Union(_) => format!("{t} is contained in a union"),
            Self::Enumeration(_) => format!("{t} is contained in an enumeration"),
            Self::Filler(_) => format!("an existential on {t} with a filler beyond OWL 2 QL"),
            Self::Asserted(_) => {
                format!("an individual is asserted to be in an existential on {t}")
            }
        }
    }
}

/// The hazards of an ontology and the terms they reach.
#[derive(Debug, Clone, Default)]
pub(crate) struct Hazards {
    /// Terms whose answers may be missing, each with the hazard that reached it.
    pub affected: HashMap<Term, Hazard>,
    /// A hazard that reaches every term (one that can equate individuals).
    pub global: Option<Hazard>,
}

/// Where a class expression stands in an axiom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The whole left side (a subclass): plain classes, unions and `∃P` are QL.
    LeftTop,
    /// Inside a complex left side.
    Left,
    /// On the right; with the role of the existential it is the filler of, if any.
    Right(Option<Term>),
}

struct Collect<'a> {
    o: &'a Ontology,
    found: Vec<Hazard>,
}

impl Collect<'_> {
    fn walk(&mut self, e: ExprId, place: Place) {
        let o = self.o;
        let partial = |c: &mut Self| {
            if let Place::Right(Some(role)) = place {
                c.found.push(Hazard::Filler(role));
            }
        };
        match o.class(e) {
            ClassExpr::Class(c) => {
                if place == Place::Left {
                    self.found.push(Hazard::LeftClass(*c));
                }
            }
            ClassExpr::Thing | ClassExpr::Nothing => {}
            ClassExpr::And(parts) => {
                let inner = if place == Place::LeftTop {
                    Place::Left
                } else {
                    place
                };
                for &p in parts {
                    self.walk(p, inner);
                }
            }
            ClassExpr::Or(parts) => {
                partial(self);
                let inner = if place == Place::LeftTop {
                    Place::LeftTop
                } else {
                    place
                };
                for &p in parts {
                    self.walk(p, inner);
                }
            }
            ClassExpr::Not(inner) => {
                partial(self);
                self.walk(*inner, Place::Left);
            }
            ClassExpr::OneOf(_) => partial(self),
            ClassExpr::Some(p, f) | ClassExpr::Min(_, p, f) => {
                let thing = matches!(o.class(*f), ClassExpr::Thing);
                match place {
                    Place::LeftTop if thing => {}
                    Place::LeftTop | Place::Left => {
                        self.found.push(Hazard::LeftExistential(p.named()));
                        self.walk(*f, Place::Left);
                    }
                    Place::Right(_) => self.walk(*f, Place::Right(Some(p.named()))),
                }
            }
            ClassExpr::Exact(_, p, f) | ClassExpr::Max(_, p, f) => {
                partial(self);
                self.found.push(Hazard::Cardinality(p.named()));
                self.walk(*f, Place::Left);
            }
            ClassExpr::All(p, f) => {
                partial(self);
                self.found.push(Hazard::Universal(p.named()));
                self.walk(*f, Place::Left);
            }
            ClassExpr::HasValue(p, _) => {
                partial(self);
                self.found.push(Hazard::Value(p.named()));
            }
            ClassExpr::HasSelf(p) => {
                partial(self);
                self.found.push(Hazard::SelfRestriction(p.named()));
            }
            ClassExpr::DataSome(p, _) | ClassExpr::DataMin(_, p, _) => {
                if matches!(place, Place::LeftTop | Place::Left) {
                    self.found.push(Hazard::LeftExistential(*p));
                }
            }
            ClassExpr::DataAll(p, _) => {
                partial(self);
                self.found.push(Hazard::Universal(*p));
            }
            ClassExpr::DataHasValue(p, _) => {
                partial(self);
                self.found.push(Hazard::Value(*p));
            }
            ClassExpr::DataMax(_, p, _) | ClassExpr::DataExact(_, p, _) => {
                partial(self);
                self.found.push(Hazard::Cardinality(*p));
            }
        }
    }
}

/// The named classes and properties of an expression.
fn terms(o: &Ontology, e: ExprId, out: &mut Vec<Term>) {
    match o.class(e) {
        ClassExpr::Class(c) => out.push(*c),
        ClassExpr::Thing | ClassExpr::Nothing | ClassExpr::OneOf(_) => {}
        ClassExpr::And(parts) | ClassExpr::Or(parts) => {
            for &p in parts {
                terms(o, p, out);
            }
        }
        ClassExpr::Not(inner) => terms(o, *inner, out),
        ClassExpr::Some(p, f)
        | ClassExpr::All(p, f)
        | ClassExpr::Min(_, p, f)
        | ClassExpr::Max(_, p, f)
        | ClassExpr::Exact(_, p, f) => {
            out.push(p.named());
            terms(o, *f, out);
        }
        ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => out.push(p.named()),
        ClassExpr::DataSome(p, _)
        | ClassExpr::DataAll(p, _)
        | ClassExpr::DataHasValue(p, _)
        | ClassExpr::DataMin(_, p, _)
        | ClassExpr::DataMax(_, p, _)
        | ClassExpr::DataExact(_, p, _) => out.push(*p),
    }
}

/// The hazards of `o` and the terms they reach, given the anonymous individuals' types
/// (`types`, with `basics` naming their concepts).
pub(crate) fn hazards(o: &Ontology, types: &[Type], basic: &dyn Fn(u32) -> Basic) -> Hazards {
    let mut found: Vec<Hazard> = Vec::new();
    // Properties connected by inclusions, inverses, equivalences and chains.
    let mut links: Vec<(Term, Term)> = Vec::new();
    // Premise terms -> conclusion terms, per axiom.
    let mut edges: Vec<(Vec<Term>, Vec<Term>)> = Vec::new();
    let expr_terms = |e: ExprId| {
        let mut out = Vec::new();
        terms(o, e, &mut out);
        out
    };
    let mut collect = Collect {
        o,
        found: Vec::new(),
    };
    for axiom in &o.axioms {
        match axiom {
            Axiom::SubClassOf(l, r) => {
                collect.walk(*l, Place::LeftTop);
                collect.walk(*r, Place::Right(None));
                let left = expr_terms(*l);
                if matches!(o.class(*r), ClassExpr::Or(_)) {
                    found.extend(left.iter().map(|&c| Hazard::Union(c)));
                }
                if matches!(o.class(*r), ClassExpr::OneOf(_)) {
                    found.extend(left.iter().map(|&c| Hazard::Enumeration(c)));
                }
                edges.push((left, expr_terms(*r)));
            }
            Axiom::EquivalentClasses(classes) => {
                let all: Vec<Term> = classes.iter().flat_map(|&c| expr_terms(c)).collect();
                for &c in classes {
                    collect.walk(c, Place::LeftTop);
                    collect.walk(c, Place::Right(None));
                    if matches!(o.class(c), ClassExpr::Or(_)) {
                        found.extend(all.iter().map(|&t| Hazard::Union(t)));
                    }
                    if matches!(o.class(c), ClassExpr::OneOf(_)) {
                        found.extend(all.iter().map(|&t| Hazard::Enumeration(t)));
                    }
                }
                edges.push((all.clone(), all));
            }
            Axiom::DisjointUnion(class, parts) => {
                found.push(Hazard::Union(*class));
                let mut all: Vec<Term> = parts.iter().flat_map(|&c| expr_terms(c)).collect();
                for &p in parts {
                    collect.walk(p, Place::LeftTop);
                    collect.walk(p, Place::Right(None));
                }
                all.push(*class);
                edges.push((all.clone(), all));
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                if chain.len() > 1 {
                    found.extend(chain.iter().map(|p| Hazard::Chain(p.named())));
                    found.push(Hazard::Chain(sup.named()));
                }
                for p in chain {
                    links.push((p.named(), sup.named()));
                }
                edges.push((chain.iter().map(|p| p.named()).collect(), vec![sup.named()]));
            }
            Axiom::EquivalentObjectProperties(ps) => {
                let all: Vec<Term> = ps.iter().map(|p| p.named()).collect();
                links.extend(all.windows(2).map(|w| (w[0], w[1])));
                edges.push((all.clone(), all));
            }
            Axiom::InverseObjectProperties(a, b) => {
                links.push((a.named(), b.named()));
                edges.push((vec![a.named(), b.named()], vec![a.named(), b.named()]));
            }
            Axiom::SubDataPropertyOf(a, b) => {
                links.push((*a, *b));
                edges.push((vec![*a], vec![*b]));
            }
            Axiom::EquivalentDataProperties(ps) => {
                links.extend(ps.windows(2).map(|w| (w[0], w[1])));
                edges.push((ps.clone(), ps.clone()));
            }
            Axiom::ObjectPropertyDomain(p, c) | Axiom::ObjectPropertyRange(p, c) => {
                collect.walk(*c, Place::Right(None));
                edges.push((vec![p.named()], expr_terms(*c)));
            }
            Axiom::DataPropertyDomain(p, c) => {
                collect.walk(*c, Place::Right(None));
                edges.push((vec![*p], expr_terms(*c)));
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => {
                found.push(Hazard::Transitive(p.named()));
            }
            Axiom::ObjectCharacteristic(
                Characteristic::Functional | Characteristic::InverseFunctional,
                p,
            ) => found.push(Hazard::Functional(p.named())),
            Axiom::FunctionalDataProperty(p) => found.push(Hazard::Functional(*p)),
            Axiom::HasKey(class, ops, dps) => {
                found.extend(expr_terms(*class).into_iter().map(Hazard::Key));
                found.extend(ops.iter().map(|p| Hazard::Key(p.named())));
                found.extend(dps.iter().map(|&p| Hazard::Key(p)));
            }
            Axiom::ClassAssertion(c, _) if !matches!(o.class(*c), ClassExpr::Class(_)) => {
                let mut roles = Vec::new();
                existential_roles(o, *c, &mut roles);
                found.extend(roles.into_iter().map(Hazard::Asserted));
                collect.walk(*c, Place::Right(None));
            }
            _ => {}
        }
    }
    found.extend(collect.found);

    let mut by_term: HashMap<Term, Hazard> = HashMap::new();
    // The hazards that can equate individuals, apart: a term may have several hazards,
    // and one of those must not hide behind another.
    let mut equating: HashMap<Term, Hazard> = HashMap::new();
    for &h in &found {
        by_term.entry(h.term()).or_insert(h);
        if h.equates() {
            equating.entry(h.term()).or_insert(h);
        }
    }
    // Connected properties (union-find over the links).
    let mut parent: HashMap<Term, Term> = HashMap::new();
    fn find(parent: &mut HashMap<Term, Term>, t: Term) -> Term {
        let p = *parent.get(&t).unwrap_or(&t);
        if p == t {
            return t;
        }
        let root = find(parent, p);
        parent.insert(t, root);
        root
    }
    for &(a, b) in &links {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut components: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut properties: HashSet<Term> = links.iter().flat_map(|&(a, b)| [a, b]).collect();
    properties.extend(types.iter().map(|t| t.role.named()));
    for &p in &properties {
        let root = find(&mut parent, p);
        components.entry(root).or_default().push(p);
    }
    let component = |parent: &mut HashMap<Term, Term>, p: Term| -> Vec<Term> {
        let root = find(parent, p);
        components.get(&root).cloned().unwrap_or_else(|| vec![p])
    };

    // Where the hazards meet anonymous individuals: their terms.
    let mut start: Vec<(Term, Hazard)> = Vec::new();
    let mut global: Option<Hazard> = None;
    for ty in types {
        let props = component(&mut parent, ty.role.named());
        let classes: Vec<Term> = ty
            .concepts
            .iter()
            .filter_map(|&id| match basic(id) {
                Basic::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        let terms = || props.iter().chain(&classes);
        if global.is_none() {
            global = terms().find_map(|t| equating.get(t).copied());
        }
        let hazard = terms().find_map(|t| by_term.get(t).copied());
        if let Some(h) = hazard {
            start.extend(props.iter().map(|&p| (p, h)));
            start.extend(classes.iter().map(|&c| (c, h)));
        }
    }
    // Existentials asserted of individuals make anonymous individuals of their own.
    for &h in &found {
        if let Hazard::Asserted(p) = h {
            start.extend(component(&mut parent, p).into_iter().map(|q| (q, h)));
        }
    }

    // Upward along the axioms.
    let mut next: HashMap<Term, Vec<Term>> = HashMap::new();
    for (premise, conclusion) in &edges {
        for &a in premise {
            next.entry(a)
                .or_default()
                .extend(conclusion.iter().copied());
        }
    }
    let mut affected: HashMap<Term, Hazard> = HashMap::new();
    let mut queue: VecDeque<(Term, Hazard)> = VecDeque::new();
    for (t, h) in start {
        if let std::collections::hash_map::Entry::Vacant(e) = affected.entry(t) {
            e.insert(h);
            queue.push_back((t, h));
        }
    }
    while let Some((t, h)) = queue.pop_front() {
        for &u in next.get(&t).into_iter().flatten() {
            if let std::collections::hash_map::Entry::Vacant(e) = affected.entry(u) {
                e.insert(h);
                queue.push_back((u, h));
            }
        }
    }
    Hazards { affected, global }
}

/// The roles of the existentials (`∃`, `≥ n`) in an expression.
fn existential_roles(o: &Ontology, e: ExprId, out: &mut Vec<Term>) {
    match o.class(e) {
        ClassExpr::Some(p, f) | ClassExpr::Min(_, p, f) | ClassExpr::Exact(_, p, f) => {
            out.push(p.named());
            existential_roles(o, *f, out);
        }
        ClassExpr::DataSome(p, _) | ClassExpr::DataMin(_, p, _) | ClassExpr::DataExact(_, p, _) => {
            out.push(*p);
        }
        ClassExpr::And(parts) | ClassExpr::Or(parts) => {
            for &p in parts {
                existential_roles(o, p, out);
            }
        }
        _ => {}
    }
}
