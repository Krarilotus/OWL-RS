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
    /// Under a minimum cardinality on the left of an axiom that isn't read: above 1, or 0
    /// inside a complex left side.
    LeftMinimum(Term),
    /// In an intersection, a filler or a complex expression on the left of an axiom.
    LeftClass(Term),
    /// A class contained in a union or an enumeration.
    Union(Term),
    Enumeration(Term),
    /// The role of an existential whose filler the rewriting reads only in part.
    Filler(Term),
    /// The role of an existential an individual is asserted to be in.
    Asserted(Term),
    /// The role of an existential in a conclusion the TBox doesn't read (under a universal,
    /// a union or a complement, after a left side it doesn't read, a disjoint union's
    /// covering): anonymous individuals no generator types (I24b).
    Untyped(Term),
    /// A term on the right of an axiom whose left side any individual may be in, with
    /// none of that side's classes or properties (`⊤ ⊓ ≥ 0 R.⊤`, `B ⊔ ¬B`), where the
    /// rewriting doesn't read the axiom.
    OpenLeft(Term),
    /// An axiom on the term outside the profile of the closure the rewriting runs over,
    /// and not covered by another hazard ([`crate::profile::covered`]). The closure's rules
    /// are complete only for that profile, so named individuals may miss its conclusions
    /// (I24c).
    OutsideProfile(Term),
    /// An axiom outside the profile, as [`Self::OutsideProfile`], that equates individuals
    /// and names no term: global, which needs none (master, 8 Nov 13:15).
    Equating,
}

impl Hazard {
    /// The property or class it is about; none for [`Self::Equating`].
    pub fn term(self) -> Option<Term> {
        Some(match self {
            Self::Transitive(t)
            | Self::Chain(t)
            | Self::Functional(t)
            | Self::Key(t)
            | Self::Cardinality(t)
            | Self::Universal(t)
            | Self::Value(t)
            | Self::SelfRestriction(t)
            | Self::LeftExistential(t)
            | Self::LeftMinimum(t)
            | Self::LeftClass(t)
            | Self::Union(t)
            | Self::Enumeration(t)
            | Self::Filler(t)
            | Self::Asserted(t)
            | Self::Untyped(t)
            | Self::OpenLeft(t)
            | Self::OutsideProfile(t) => t,
            Self::Equating => return None,
        })
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
        let t = self.term().map(name).unwrap_or_default();
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
            Self::LeftMinimum(_) => {
                format!("a minimum cardinality on {t} on the left of an axiom")
            }
            Self::LeftClass(_) => format!("{t} in a complex class expression on the left"),
            Self::Union(_) => format!("{t} is contained in a union"),
            Self::Enumeration(_) => format!("{t} is contained in an enumeration"),
            Self::Filler(_) => format!("an existential on {t} with a filler beyond OWL 2 QL"),
            Self::Asserted(_) => {
                format!("an individual is asserted to be in an existential on {t}")
            }
            Self::Untyped(_) => {
                format!("an existential on {t} in a conclusion the TBox doesn't read")
            }
            Self::OpenLeft(_) => {
                format!("an axiom on {t} whose left side any individual may be in")
            }
            Self::OutsideProfile(_) => {
                format!("an axiom on {t} outside the profile whose rules the closure applies")
            }
            Self::Equating => "an axiom outside the profile equates individuals".to_owned(),
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
            // On the left, `≥ 0 R.C` is `⊤` and `≥ n R.C` for n ≥ 2 isn't read (`Tbox`'s
            // left sides). `≥ 0 R.C` as a whole left side is read exactly; inside one, it
            // is marked with its filler, as an existential there is.
            ClassExpr::Min(n, p, f) if *n != 1 && matches!(place, Place::LeftTop | Place::Left) => {
                if *n >= 2 || place == Place::Left {
                    self.found.push(Hazard::LeftMinimum(p.named()));
                    self.walk(*f, Place::Left);
                }
            }
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

/// What the TBox read of an ontology, for its hazards.
pub(crate) struct Read<'a> {
    /// The conclusions it doesn't read, each with its polarity: its ledger.
    pub unread: &'a std::collections::BTreeSet<(ExprId, bool)>,
    /// The axioms it reads only in part or not at all, by index.
    pub partial: &'a std::collections::BTreeSet<usize>,
    /// Whether the closure is OWL 2 RL's (`Closure::lists`); QL's otherwise.
    pub rl: bool,
}

/// The hazards of `o` and the terms they reach, given the anonymous individuals' types
/// (`types`, with `basics` naming their concepts) and what the TBox `read`.
pub(crate) fn hazards(
    o: &Ontology,
    types: &[Type],
    read: &Read<'_>,
    basic: &dyn Fn(u32) -> Basic,
) -> Hazards {
    let (unread, rl) = (read.unread, read.rl);
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
    // What axioms whose left side any individual may be in give every anonymous one
    // unread (`open_axiom`): their right sides' terms, and an enumerated individual.
    let mut open: Vec<Term> = Vec::new();
    let mut open_individual: Option<Term> = None;
    // Whether such an axiom concludes an existential: anonymous individuals of its own.
    let mut open_generates = false;
    // The terms of expressions individuals are asserted to be in, with an existential.
    let mut asserted: Vec<(Term, Hazard)> = Vec::new();
    for axiom in &o.axioms {
        match axiom {
            Axiom::SubClassOf(l, r) => {
                open_axiom(
                    o,
                    *l,
                    *r,
                    &mut open,
                    &mut open_individual,
                    &mut open_generates,
                );
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
                    for &d in classes.iter().filter(|&&d| d != c) {
                        open_axiom(
                            o,
                            c,
                            d,
                            &mut open,
                            &mut open_individual,
                            &mut open_generates,
                        );
                    }
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
                // A part is a left side of the class, which the TBox reads (a class).
                if parts
                    .iter()
                    .any(|&p| needs_no_term(o, p) && !read_as_top(o, p))
                {
                    open.push(*class);
                }
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
                // The anonymous individuals it makes are in its fillers' classes too.
                if let Some(&role) = roles.first() {
                    let h = Hazard::Asserted(role);
                    asserted.extend(expr_terms(*c).into_iter().map(|t| (t, h)));
                }
                found.extend(roles.into_iter().map(Hazard::Asserted));
                collect.walk(*c, Place::Right(None));
            }
            _ => {}
        }
    }
    found.extend(collect.found);
    // The ledger: a conclusion the TBox doesn't read leaves the anonymous individuals of
    // its existentials untyped. Each is seeded like an asserted one: its role, and its
    // filler's terms.
    let mut untyped: Vec<(Term, Hazard)> = Vec::new();
    for &(e, positive) in unread {
        let (mut roles, mut fillers) = (Vec::new(), Vec::new());
        generated(o, e, positive, &mut roles, Some(&mut fillers));
        if let Some(&role) = roles.first() {
            let h = Hazard::Untyped(role);
            untyped.extend(fillers.into_iter().map(|t| (t, h)));
        }
        found.extend(roles.into_iter().map(Hazard::Untyped));
    }

    let mut by_term: HashMap<Term, Hazard> = HashMap::new();
    // The hazards that can equate individuals, apart: a term may have several hazards,
    // and one of those must not hide behind another.
    let mut equating: HashMap<Term, Hazard> = HashMap::new();
    // (The walk finds hazards on terms; [`Hazard::Equating`] is only ever global.)
    for (t, h) in found.iter().filter_map(|&h| Some((h.term()?, h))) {
        by_term.entry(t).or_insert(h);
        if h.equates() {
            equating.entry(t).or_insert(h);
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
    // Existentials asserted of individuals, or in a right side the TBox doesn't read, make
    // anonymous individuals of their own.
    for &h in &found {
        if let Hazard::Asserted(p) | Hazard::Untyped(p) = h {
            start.extend(component(&mut parent, p).into_iter().map(|q| (q, h)));
        }
    }
    start.extend(asserted);
    start.extend(untyped);
    // Axioms outside the closure's profile that no other hazard covers (I24c, design note
    // "Source 3"): seeded after the walk below, along their own edges.
    let mut outside: Vec<(Term, Hazard, usize)> = Vec::new();
    // An uncovered axiom that equates named individuals: global, unless another hazard
    // already is (that one's reason is the more specific).
    let mut outside_global: Option<Hazard> = None;
    let equating_properties = crate::profile::equating_properties(o);
    for (i, axiom) in o.axioms.iter().enumerate() {
        // Read whole, the rewriting answers it; in the closure's profile, its rules do.
        if !read.partial.contains(&i) || crate::profile::covered(o, axiom, rl) {
            continue;
        }
        let concluded = crate::profile::concluded_terms(o, axiom, rl);
        let equality = crate::profile::concludes_equality(o, axiom, &equating_properties);
        // A conclusion that equates may name no term of its own (`∀R.{a}`, `C ⊑ {a}`): its
        // hazard is global, named by any term of the axiom (review r1, F1).
        let name = match concluded.first() {
            Some(&t) => Some(t),
            None if equality => crate::profile::label(o, axiom),
            None => None,
        };
        // An equating axiom is global whether or not a term names it: a global hazard
        // reaches every term (master, 8 Nov 13:15).
        if equality && outside_global.is_none() {
            outside_global = Some(name.map_or(Hazard::Equating, Hazard::OutsideProfile));
        }
        let Some(first) = name else {
            continue;
        };
        let h = Hazard::OutsideProfile(first);
        outside.extend(concluded.into_iter().map(|t| (t, h, i)));
    }
    // An axiom whose left side any individual may be in meets every anonymous individual,
    // whether or not it shares a term with the individual's type. Seeded after the others,
    // so a term they reach keeps their reason.
    let anonymous = open_generates
        || types.iter().any(|t| !t.data)
        || found
            .iter()
            .any(|h| matches!(h, Hazard::Asserted(_) | Hazard::Untyped(_)));
    let open: Vec<(Term, Hazard)> = match anonymous {
        true => open.iter().map(|&t| (t, Hazard::OpenLeft(t))).collect(),
        false => Vec::new(),
    };

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
    for seeds in [start, open] {
        for (t, h) in seeds {
            if let std::collections::hash_map::Entry::Vacant(e) = affected.entry(t) {
                e.insert(h);
                queue.push_back((t, h));
            }
        }
        while let Some((t, h)) = queue.pop_front() {
            // An anonymous individual may be in `t` unread, and `t`'s own hazard can equate
            // it with another: any fact may follow, as for the types' terms above.
            if global.is_none() {
                global = equating.get(&t).copied();
            }
            for &u in next.get(&t).into_iter().flatten() {
                if let std::collections::hash_map::Entry::Vacant(e) = affected.entry(u) {
                    e.insert(h);
                    queue.push_back((u, h));
                }
            }
        }
    }
    // An enumeration such an axiom concludes can make every individual one.
    if anonymous && global.is_none() {
        global = open_individual.map(Hazard::OpenLeft);
    }
    if global.is_none() {
        global = outside_global;
    }
    // What named individuals may miss of uncovered axioms' conclusions, along every axiom
    // from premise to conclusion (design note "Source 3"). A seed doesn't follow its own
    // axiom back to its premise, which holds wherever its conclusion is missed. A term
    // reached through another axiom follows every axiom, its seed's included.
    let mut forward: HashMap<Term, Vec<(usize, Term)>> = HashMap::new();
    for (i, axiom) in o.axioms.iter().enumerate() {
        for (premise, conclusion) in directed_edges(o, axiom) {
            for &a in &premise {
                forward
                    .entry(a)
                    .or_default()
                    .extend(conclusion.iter().map(|&c| (i, c)));
            }
        }
    }
    // Per term: the axiom whose edges it skips (`Some`), or none (`None`: every edge).
    let mut reached: HashMap<Term, Option<usize>> = HashMap::new();
    let mut queue: VecDeque<(Term, Hazard, Option<usize>)> = VecDeque::new();
    for (t, h, i) in outside {
        // Seeded by a second axiom: missed with another premise, so every edge.
        let skip = match reached.get(&t) {
            None => Some(i),
            Some(&Some(j)) if j == i => continue,
            Some(_) => None,
        };
        reached.insert(t, skip);
        queue.push_back((t, h, skip));
    }
    while let Some((t, h, skip)) = queue.pop_front() {
        affected.entry(t).or_insert(h);
        if global.is_none() {
            global = equating.get(&t).copied();
        }
        for &(j, u) in forward.get(&t).into_iter().flatten() {
            if Some(j) == skip {
                continue;
            }
            // Reached through another axiom: every edge from it, once.
            if reached.get(&u) != Some(&None) {
                reached.insert(u, None);
                queue.push_back((u, h, None));
            }
        }
    }
    Hazards { affected, global }
}

/// An axiom's links from its premise terms to its conclusion terms, one per direction:
/// a subclass, domain or range axiom one way, each ordered pair of an equivalence on its
/// own, property inclusions, inverses and equivalences as they link. Assertions have
/// none.
fn directed_edges(o: &Ontology, axiom: &Axiom) -> Vec<(Vec<Term>, Vec<Term>)> {
    // Premises by every term they read; conclusions by the terms they can add positive
    // facts on (`profile::positive_terms`, review r1, F3: `∀R.D` adds none on `R`).
    let t = |e: ExprId| {
        let mut out = Vec::new();
        terms(o, e, &mut out);
        out
    };
    let c = |e: ExprId, positive: bool| {
        let mut out = Vec::new();
        crate::profile::positive_terms(o, e, positive, &mut out);
        out
    };
    let pairs = |v: &[ExprId], positive: bool| -> Vec<(Vec<Term>, Vec<Term>)> {
        v.iter()
            .flat_map(|&a| v.iter().filter(move |&&b| b != a).map(move |&b| (a, b)))
            .map(|(a, b)| (t(a), c(b, positive)))
            .collect()
    };
    match axiom {
        Axiom::SubClassOf(l, r) => vec![(t(*l), c(*r, true))],
        Axiom::EquivalentClasses(v) => pairs(v, true),
        Axiom::DisjointClasses(v) => pairs(v, false),
        Axiom::DisjointUnion(class, parts) => parts
            .iter()
            .flat_map(|&p| [(t(p), vec![*class]), (vec![*class], c(p, true))])
            .collect(),
        Axiom::ObjectPropertyDomain(p, e) | Axiom::ObjectPropertyRange(p, e) => {
            vec![(vec![p.named()], c(*e, true))]
        }
        Axiom::DataPropertyDomain(p, e) => vec![(vec![*p], c(*e, true))],
        Axiom::SubObjectPropertyOf(chain, sup) => {
            vec![(chain.iter().map(|p| p.named()).collect(), vec![sup.named()])]
        }
        Axiom::EquivalentObjectProperties(ps) => {
            let all: Vec<Term> = ps.iter().map(|p| p.named()).collect();
            vec![(all.clone(), all)]
        }
        Axiom::InverseObjectProperties(a, b) => {
            vec![
                (vec![a.named()], vec![b.named()]),
                (vec![b.named()], vec![a.named()]),
            ]
        }
        Axiom::SubDataPropertyOf(a, b) => vec![(vec![*a], vec![*b])],
        Axiom::EquivalentDataProperties(ps) => vec![(ps.clone(), ps.clone())],
        _ => Vec::new(),
    }
}

/// For an axiom `l ⊑ r` whose left side any individual may be in with none of its terms,
/// unless the TBox reads it exactly (`l` as `⊤`, `r` whole): `r`'s terms into `terms`, an
/// individual `r` may make every individual equal to into `individual`, and whether `r`
/// has an existential (anonymous individuals the TBox doesn't type) into `generates`.
fn open_axiom(
    o: &Ontology,
    l: ExprId,
    r: ExprId,
    terms_out: &mut Vec<Term>,
    individual: &mut Option<Term>,
    generates: &mut bool,
) {
    if !needs_no_term(o, l) || (read_as_top(o, l) && read_on_right(o, r)) {
        return;
    }
    terms(o, r, terms_out);
    if individual.is_none() {
        *individual = enumerated(o, r);
    }
    let mut roles = Vec::new();
    existential_roles(o, r, &mut roles);
    *generates |= !roles.is_empty();
}

/// Whether an individual may be in `e` with none of `e`'s classes or properties holding of
/// it: no membership of its classes, no edge of its properties. Conservative: a part that
/// is negative or bounded above may hold without either.
fn needs_no_term(o: &Ontology, e: ExprId) -> bool {
    match o.class(e) {
        ClassExpr::Thing
        | ClassExpr::Not(_)
        | ClassExpr::All(..)
        | ClassExpr::Max(..)
        | ClassExpr::DataAll(..)
        | ClassExpr::DataMax(..) => true,
        ClassExpr::Min(n, ..)
        | ClassExpr::Exact(n, ..)
        | ClassExpr::DataMin(n, ..)
        | ClassExpr::DataExact(n, ..) => *n == 0,
        ClassExpr::And(parts) => parts.iter().all(|&p| needs_no_term(o, p)),
        ClassExpr::Or(parts) => parts.iter().any(|&p| needs_no_term(o, p)),
        ClassExpr::Nothing
        | ClassExpr::Class(_)
        | ClassExpr::OneOf(_)
        | ClassExpr::Some(..)
        | ClassExpr::HasValue(..)
        | ClassExpr::HasSelf(_)
        | ClassExpr::DataSome(..)
        | ClassExpr::DataHasValue(..) => false,
    }
}

/// Whether the TBox reads left side `e` as `⊤` (`Tbox`'s left sides: `⊤`, `≥ 0 R.C`, and a
/// union with such a part).
fn read_as_top(o: &Ontology, e: ExprId) -> bool {
    match o.class(e) {
        ClassExpr::Thing | ClassExpr::Min(0, ..) => true,
        ClassExpr::Or(parts) => parts.iter().any(|&p| read_as_top(o, p)),
        _ => false,
    }
}

/// Whether the TBox reads right side `e` whole: no hazard in it, and no union, enumeration
/// or complement outside an existential.
fn read_on_right(o: &Ontology, e: ExprId) -> bool {
    fn plain(o: &Ontology, e: ExprId) -> bool {
        match o.class(e) {
            ClassExpr::Or(_) | ClassExpr::OneOf(_) | ClassExpr::Not(_) => false,
            ClassExpr::And(parts) => parts.iter().all(|&p| plain(o, p)),
            _ => true,
        }
    }
    let mut collect = Collect {
        o,
        found: Vec::new(),
    };
    collect.walk(e, Place::Right(None));
    collect.found.is_empty() && plain(o, e)
}

/// An individual of an enumeration in `e` outside an existential, if there is one.
fn enumerated(o: &Ontology, e: ExprId) -> Option<Term> {
    match o.class(e) {
        ClassExpr::OneOf(individuals) => individuals.first().copied(),
        ClassExpr::And(parts) | ClassExpr::Or(parts) => {
            parts.iter().find_map(|&p| enumerated(o, p))
        }
        _ => None,
    }
}

/// The roles of the existentials (`∃`, `≥ n`) in an expression ([`generated`]).
fn existential_roles(o: &Ontology, e: ExprId, out: &mut Vec<Term>) {
    generated(o, e, true, out, None);
}

/// The existentials concluded by `e` (`positive`) or by its complement: their roles into
/// `roles`, their fillers' terms into `fillers` where given (a caller without them reads
/// no filler's terms: #155's count). Every `≥ 1`-form of the expression's
/// negation normal form is collected, at any depth (fix check r2, F4):
/// - A restriction makes a successor where it reads as `≥ n` for n ≥ 1: `∃R.F`, `≥ n` and
///   `= n` for n ≥ 1, `¬∀R.F` (`∃R.¬F`), `¬≤ n` (`≥ n+1`) and `¬= n` (it may be `≥ n+1`).
///   `∀`, `≤` and `= 0` make none, nor do `¬∃` and `¬≥ n`, which are `∀` and `≤ n−1`.
/// - Its filler is read where it holds of a successor, made or already there: `F` under
///   `∃`, `∀` and `≥`; `¬F` under `¬∃` and `¬∀`; both under `≤` and `=`, which put each
///   successor in `F` or in `¬F` (`≤ 0 R.¬∃S.D` is `∀R.∃S.D`).
/// - Conjunctions and unions are read part by part, complements flip the polarity.
/// - Nothing is inside a class, `⊤`, `⊥`, a nominal, `hasValue` or `hasSelf`: they make no
///   anonymous individual (the `Enumeration`, `Value` and `SelfRestriction` hazards
///   mark them). A data range has no class expression either: datatypes, facets and their
///   combinations make a data value at most.
///
/// Inverses give the named property, its role component. Conservative: it may name a
/// role no individual is made for, never miss one. Under a bound, whose filler is read both
/// ways, each (expression, polarity) is walked once, so nested bounds don't double the walk
/// at each level; elsewhere the walk follows the expression as before and allocates
/// nothing (#155's count: a set per walk cost 3.9 % of a compile over 1,000 asserted
/// restrictions). A successor's filler terms are still collected per restriction that
/// makes one.
fn generated(
    o: &Ontology,
    e: ExprId,
    positive: bool,
    roles: &mut Vec<Term>,
    fillers: Option<&mut Vec<Term>>,
) {
    nnf(o, e, positive, false, &mut HashSet::new(), roles, fillers);
}

/// [`generated`]'s walk; `bounded` where it is inside a filler read both ways.
fn nnf(
    o: &Ontology,
    e: ExprId,
    positive: bool,
    bounded: bool,
    seen: &mut HashSet<(ExprId, bool)>,
    roles: &mut Vec<Term>,
    mut fillers: Option<&mut Vec<Term>>,
) {
    use ClassExpr as C;
    if bounded && !seen.insert((e, positive)) {
        return;
    }
    // An object restriction: its role and filler, whether it makes a successor, and
    // whether its filler (`inside`) or the filler's complement (`outside`) holds of one.
    let (p, f, makes, inside, outside) = match (o.class(e), positive) {
        (
            C::Thing
            | C::Nothing
            | C::Class(_)
            | C::OneOf(_)
            | C::HasValue(..)
            | C::HasSelf(_)
            | C::DataHasValue(..),
            _,
        ) => return,
        (C::And(parts) | C::Or(parts), _) => {
            for &part in parts {
                nnf(
                    o,
                    part,
                    positive,
                    bounded,
                    seen,
                    roles,
                    fillers.as_deref_mut(),
                );
            }
            return;
        }
        (C::Not(inner), _) => return nnf(o, *inner, !positive, bounded, seen, roles, fillers),
        (C::DataSome(p, _), true)
        | (C::DataAll(p, _), false)
        | (C::DataMax(_, p, _), false)
        | (C::DataExact(_, p, _), false) => {
            roles.push(*p);
            return;
        }
        (C::DataMin(n, p, _) | C::DataExact(n, p, _), true) => {
            if *n >= 1 {
                roles.push(*p);
            }
            return;
        }
        (C::DataSome(..), false)
        | (C::DataAll(..), true)
        | (C::DataMin(..), false)
        | (C::DataMax(..), true) => return,
        (C::Some(p, f), true) => (p, f, true, true, false),
        (C::Some(p, f), false) => (p, f, false, false, true),
        (C::All(p, f), true) => (p, f, false, true, false),
        (C::All(p, f), false) => (p, f, true, false, true),
        (C::Min(n, p, f), true) => (p, f, *n >= 1, true, false),
        (C::Min(_, p, f), false) | (C::Max(_, p, f), true) => (p, f, false, true, true),
        (C::Max(_, p, f), false) => (p, f, true, true, false),
        (C::Exact(n, p, f), true) => (p, f, *n >= 1, true, true),
        (C::Exact(_, p, f), false) => (p, f, true, true, true),
    };
    if makes {
        roles.push(p.named());
        if let Some(fillers) = fillers.as_deref_mut() {
            terms(o, *f, fillers);
        }
    }
    let bounded = bounded || (inside && outside);
    if inside {
        nnf(o, *f, true, bounded, seen, roles, fillers.as_deref_mut());
    }
    if outside {
        nnf(o, *f, false, bounded, seen, roles, fillers);
    }
}
