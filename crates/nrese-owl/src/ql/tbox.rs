//! The QL part of an ontology, compiled for tree witnesses (docs/design/ql-rewriting.md §2).
//!
//! DL-Lite's view of the axioms: inclusions between basic concepts (`A`, `∃ρ`), generating
//! axioms `B ⊑ ∃ρ.X`, and role inclusions with inverses. Each concept inclusion is marked
//! *base* when the materialisation applies it to named individuals ([`Closure`]); the
//! rewriting adds only what the others give.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::mapping::Ontology;
use crate::model::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Term};

/// A role: a property, or an object property's inverse.
pub type Role = ObjProp;

/// A basic concept: a class, an unqualified existential `∃ρ`, everything, or a concept the
/// compilation introduced for a nested filler (no individual is ever stated to be one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Basic {
    Class(Term),
    Exists(Role),
    Thing,
    Fresh(u32),
}

/// What the materialisation the rewriting runs over applies to named individuals besides
/// the hierarchies, domains, ranges, inverses, symmetry and `∃P.⊤ ⊑ A` (which every
/// ruleset the rewriting runs with applies).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Closure {
    /// The OWL 2 RL list rules: intersections on the right (`cls-int2`) and unions on the
    /// left (`cls-uni`) of a subclass axiom. `owl2-rl` has them, `owl2-ql` doesn't.
    pub lists: bool,
}

/// A generating axiom `left ⊑ ∃role.filler` (`filler` `None`: `owl:Thing`, or a data
/// value), with the type of the anonymous element it makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Generator {
    pub left: u32,
    pub ty: usize,
}

/// The type of an anonymous element: the role that reached it, its filler, and what
/// follows for it.
#[derive(Debug, Clone)]
pub(crate) struct Type {
    pub role: Role,
    pub data: bool,
    /// The basic concepts it is an instance of.
    pub concepts: HashSet<u32>,
    /// The types of its successors.
    pub children: Vec<usize>,
}

/// The compiled QL TBox.
#[derive(Debug, Default)]
pub struct Tbox {
    basics: Vec<Basic>,
    ids: HashMap<Basic, u32>,
    /// Concept inclusions `B ⊑ B'` (generating axioms as `B ⊑ ∃ρ`), as edges both ways.
    up: Vec<Vec<u32>>,
    down: Vec<Vec<u32>>,
    /// The inclusions the materialisation applies.
    base_up: Vec<Vec<u32>>,
    base_down: Vec<Vec<u32>>,
    /// Per role, the roles it is included in (itself too).
    sup: HashMap<Role, HashSet<Role>>,
    reflexive: HashSet<Term>,
    data: HashSet<Term>,
    pub(crate) generators: Vec<Generator>,
    pub(crate) types: Vec<Type>,
    /// Per role, the types whose incoming role is included in it: the elements an edge of
    /// that role from their parent reaches.
    pub(crate) reaching: HashMap<Role, Vec<usize>>,
    /// Whether some inclusion isn't one the materialisation applies.
    gaps: bool,
    thing: Option<Term>,
    /// [`Self::class_alternatives`] per class, as queries ask for them.
    alternatives: std::sync::RwLock<HashMap<Term, Option<Vec<Basic>>>>,
}

/// What makes an anonymous element's type: the role that reached it, its filler, and
/// whether it is a data value.
type TypeKey = (Role, Option<u32>, bool);

/// The axioms read, before the closures are taken.
#[derive(Default)]
struct Builder {
    tbox: Tbox,
    lists: bool,
    /// Role inclusions: (sub, super, applied by the materialisation).
    roles: Vec<(Role, Role, bool)>,
    /// Generating axioms: (left, role, filler, data).
    generating: Vec<(u32, Role, Option<u32>, bool)>,
    fresh: HashMap<ExprId, u32>,
}

impl Tbox {
    /// Compiles the QL part of `ontology` for rewriting over data closed under `closure`.
    /// `thing` is the source's id of `owl:Thing`, if it has one.
    pub fn compile(ontology: &Ontology, closure: Closure, thing: Option<Term>) -> Self {
        let mut b = Builder {
            lists: closure.lists,
            ..Builder::default()
        };
        b.tbox.thing = thing;
        b.basic(Basic::Thing);
        for axiom in &ontology.axioms {
            b.axiom(ontology, axiom);
        }
        b.finish()
    }

    /// Whether rewriting can't change any query: no generating axiom, and every concept
    /// inclusion is one the materialisation applies.
    pub fn is_empty(&self) -> bool {
        self.generators.is_empty() && !self.gaps
    }

    /// The number of generating axioms.
    pub fn generating_axioms(&self) -> usize {
        self.generators.len()
    }

    pub(crate) fn basic(&self, id: u32) -> Basic {
        self.basics[id as usize]
    }

    pub(crate) fn id(&self, basic: Basic) -> Option<u32> {
        self.ids.get(&basic).copied()
    }

    /// Whether `rho ⊑ s` (by the role inclusions).
    pub(crate) fn implies(&self, rho: Role, s: Role) -> bool {
        rho == s || self.sup.get(&rho).is_some_and(|sup| sup.contains(&s))
    }

    /// Whether the property is reflexive: some reflexive property is included in it or in
    /// its inverse.
    pub(crate) fn is_reflexive(&self, property: Term) -> bool {
        self.reflexive.iter().any(|&r| {
            [ObjProp::Named(r), ObjProp::Inverse(r)]
                .into_iter()
                .any(|rho| self.implies(rho, ObjProp::Named(property)))
        })
    }

    /// Whether `class` holds of the anonymous elements of type `ty`.
    pub(crate) fn type_has_class(&self, ty: usize, class: Term) -> bool {
        let ty = &self.types[ty];
        if ty.data {
            return false;
        }
        Some(class) == self.thing
            || self
                .id(Basic::Class(class))
                .is_some_and(|id| ty.concepts.contains(&id))
    }

    /// The basic concepts below `target` (itself included).
    pub(crate) fn below(&self, target: u32) -> Vec<u32> {
        reach(&self.down, [target])
    }

    /// The alternatives an atom `class(t)` gets over the closure: the classes and
    /// existentials below it whose instances the materialisation doesn't already make
    /// instances of another alternative. `None` if there is nothing to add.
    pub(crate) fn class_alternatives(&self, class: Term) -> Option<Vec<Basic>> {
        let known = self.alternatives.read().unwrap_or_else(|p| p.into_inner());
        if let Some(alternatives) = known.get(&class) {
            return alternatives.clone();
        }
        drop(known);
        let alternatives = self.compute_class_alternatives(class);
        self.alternatives
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(class, alternatives.clone());
        alternatives
    }

    fn compute_class_alternatives(&self, class: Term) -> Option<Vec<Basic>> {
        if !self.gaps && self.generators.is_empty() {
            return None;
        }
        let id = self.id(Basic::Class(class))?;
        let below = self.below(id);
        // What the materialisation already puts in the class needs no alternative.
        let covered: HashSet<u32> = reach(&self.base_down, [id]).into_iter().collect();
        if below.len() == covered.len() {
            return None;
        }
        let candidates = below
            .into_iter()
            .filter(|b| *b == id || !covered.contains(b))
            .collect();
        let alternatives = self.minimal(candidates);
        (alternatives != [Basic::Class(class)]).then_some(alternatives)
    }

    /// Whether every instance of `class` generates one of the generating axioms `axioms`:
    /// the class is below one of their left sides.
    pub(crate) fn generates(&self, class: Term, axioms: &[usize]) -> bool {
        let Some(id) = self.id(Basic::Class(class)) else {
            return false;
        };
        let above = reach(&self.up, [id]);
        axioms
            .iter()
            .any(|&g| above.contains(&self.generators[g].left))
    }

    /// The alternatives for a root that generates any of the generating axioms `axioms`:
    /// the basic concepts below their left sides, minimised as in
    /// [`Self::class_alternatives`].
    pub(crate) fn generator_alternatives(
        &self,
        axioms: impl IntoIterator<Item = usize>,
    ) -> Vec<Basic> {
        let lefts: Vec<u32> = axioms
            .into_iter()
            .map(|g| self.generators[g].left)
            .collect();
        self.minimal(reach(&self.down, lefts))
    }

    /// The members of `set` a named individual can be stated to be (classes and
    /// existentials), leaving out those whose instances the materialisation makes
    /// instances of another member (one of each set of equivalent ones kept).
    fn minimal(&self, set: Vec<u32>) -> Vec<Basic> {
        let set: Vec<u32> = set
            .into_iter()
            .filter(|&id| matches!(self.basic(id), Basic::Class(_) | Basic::Exists(_)))
            .collect();
        let members: HashSet<u32> = set.iter().copied().collect();
        let above: HashMap<u32, HashSet<u32>> = set
            .iter()
            .map(|&id| (id, reach(&self.base_up, [id]).into_iter().collect()))
            .collect();
        let mut kept: Vec<Basic> = set
            .iter()
            .filter(|&&id| {
                !above[&id].iter().any(|&other| {
                    other != id
                        && members.contains(&other)
                        && (!above[&other].contains(&id) || other < id)
                })
            })
            .map(|&id| self.basic(id))
            .collect();
        kept.sort();
        kept
    }
}

/// Everything reachable from `start` along `edges`, `start` included.
fn reach(edges: &[Vec<u32>], start: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<u32> = start.into_iter().filter(|&s| seen.insert(s)).collect();
    let mut out = Vec::new();
    while let Some(id) = queue.pop_front() {
        out.push(id);
        for &next in &edges[id as usize] {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    out
}

impl Builder {
    fn basic(&mut self, basic: Basic) -> u32 {
        if let Some(&id) = self.tbox.ids.get(&basic) {
            return id;
        }
        let id = self.tbox.basics.len() as u32;
        self.tbox.basics.push(basic);
        self.tbox.ids.insert(basic, id);
        self.tbox.up.push(Vec::new());
        self.tbox.down.push(Vec::new());
        self.tbox.base_up.push(Vec::new());
        self.tbox.base_down.push(Vec::new());
        id
    }

    fn include(&mut self, sub: u32, sup: u32, base: bool) {
        if sub == sup {
            return;
        }
        if !self.tbox.up[sub as usize].contains(&sup) {
            self.tbox.up[sub as usize].push(sup);
            self.tbox.down[sup as usize].push(sub);
        }
        if base && !self.tbox.base_up[sub as usize].contains(&sup) {
            self.tbox.base_up[sub as usize].push(sup);
            self.tbox.base_down[sup as usize].push(sub);
        }
    }

    fn axiom(&mut self, o: &Ontology, axiom: &Axiom) {
        match axiom {
            Axiom::SubClassOf(sub, sup) => self.subclass(o, *sub, *sup),
            Axiom::EquivalentClasses(classes) => {
                for &a in classes {
                    for &b in classes {
                        if a != b {
                            self.subclass(o, a, b);
                        }
                    }
                }
            }
            Axiom::DisjointUnion(class, parts) => {
                let class = self.basic(Basic::Class(*class));
                for &part in parts {
                    for (left, _) in self.lefts(o, part) {
                        self.include(left, class, false);
                    }
                }
            }
            Axiom::SubObjectPropertyOf(chain, sup) if chain.len() == 1 => {
                self.role(chain[0], *sup, named(chain[0]) && named(*sup));
            }
            Axiom::EquivalentObjectProperties(properties) => {
                for &a in properties {
                    for &b in properties {
                        if a != b {
                            self.role(a, b, named(a) && named(b));
                        }
                    }
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                let base = named(*a) && named(*b);
                self.role(*a, b.inverse(), base);
                self.role(b.inverse(), *a, base);
            }
            Axiom::ObjectPropertyDomain(property, class) => {
                let base = named(*property);
                let left = self.basic(Basic::Exists(*property));
                self.superclass(o, left, *class, base);
            }
            Axiom::ObjectPropertyRange(property, class) => {
                let base = named(*property);
                let left = self.basic(Basic::Exists(property.inverse()));
                self.superclass(o, left, *class, base);
            }
            Axiom::ObjectCharacteristic(Characteristic::Symmetric, property) => {
                self.role(*property, property.inverse(), named(*property));
            }
            Axiom::ObjectCharacteristic(Characteristic::Reflexive, property) => {
                self.tbox.reflexive.insert(property.named());
            }
            Axiom::SubDataPropertyOf(a, b) => {
                self.tbox.data.extend([*a, *b]);
                self.role(ObjProp::Named(*a), ObjProp::Named(*b), true);
            }
            Axiom::EquivalentDataProperties(properties) => {
                self.tbox.data.extend(properties.iter().copied());
                for &a in properties {
                    for &b in properties {
                        if a != b {
                            self.role(ObjProp::Named(a), ObjProp::Named(b), true);
                        }
                    }
                }
            }
            Axiom::DataPropertyDomain(property, class) => {
                self.tbox.data.insert(*property);
                let left = self.basic(Basic::Exists(ObjProp::Named(*property)));
                self.superclass(o, left, *class, true);
            }
            _ => {}
        }
    }

    /// A role inclusion; `base` if the materialisation applies it: one stated between
    /// named properties (`prp-spo1`, `prp-inv1/2`, `prp-symp`, `prp-eqp1/2`), not one
    /// through an inverse expression.
    fn role(&mut self, sub: Role, sup: Role, base: bool) {
        self.roles.push((sub, sup, base));
    }

    fn subclass(&mut self, o: &Ontology, sub: ExprId, sup: ExprId) {
        for (left, base) in self.lefts(o, sub) {
            self.superclass(o, left, sup, base);
        }
    }

    /// The basic concepts a subclass expression is read as, each with whether the
    /// materialisation applies an inclusion from it (`cax-sco`, `cls-svf2`, `cls-uni`).
    fn lefts(&mut self, o: &Ontology, expr: ExprId) -> Vec<(u32, bool)> {
        match o.class(expr) {
            ClassExpr::Class(a) => vec![(self.basic(Basic::Class(*a)), true)],
            ClassExpr::Thing => vec![(self.basic(Basic::Thing), false)],
            ClassExpr::Some(property, filler) if matches!(o.class(*filler), ClassExpr::Thing) => {
                vec![(self.basic(Basic::Exists(*property)), named(*property))]
            }
            ClassExpr::Min(n, property, filler)
                if *n >= 1 && matches!(o.class(*filler), ClassExpr::Thing) =>
            {
                vec![(self.basic(Basic::Exists(*property)), false)]
            }
            ClassExpr::DataSome(property, _) => {
                self.tbox.data.insert(*property);
                vec![(self.basic(Basic::Exists(ObjProp::Named(*property))), false)]
            }
            ClassExpr::Or(parts) => {
                let lists = self.lists;
                parts
                    .clone()
                    .into_iter()
                    .flat_map(|part| self.lefts(o, part))
                    .map(|(left, base)| (left, base && lists))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// `left ⊑ expr` for a superclass expression.
    fn superclass(&mut self, o: &Ontology, left: u32, expr: ExprId, base: bool) {
        match o.class(expr) {
            ClassExpr::Class(a) => {
                let sup = self.basic(Basic::Class(*a));
                self.include(left, sup, base);
            }
            ClassExpr::And(parts) => {
                let base = base && self.lists;
                for part in parts.clone() {
                    self.superclass(o, left, part, base);
                }
            }
            ClassExpr::Some(property, filler) => self.generates(o, left, *property, *filler),
            ClassExpr::Min(n, property, filler) | ClassExpr::Exact(n, property, filler)
                if *n >= 1 =>
            {
                self.generates(o, left, *property, *filler);
            }
            ClassExpr::DataSome(property, _) => self.generates_data(left, *property),
            ClassExpr::DataMin(n, property, _) | ClassExpr::DataExact(n, property, _)
                if *n >= 1 =>
            {
                self.generates_data(left, *property);
            }
            _ => {}
        }
    }

    fn generates(&mut self, o: &Ontology, left: u32, role: Role, filler: ExprId) {
        let filler = match o.class(filler) {
            ClassExpr::Thing => None,
            ClassExpr::Class(a) => Some(self.basic(Basic::Class(*a))),
            _ => Some(match self.fresh.get(&filler) {
                Some(&id) => id,
                None => {
                    let id = self.basic(Basic::Fresh(self.fresh.len() as u32));
                    self.fresh.insert(filler, id);
                    self.superclass(o, id, filler, false);
                    id
                }
            }),
        };
        self.generating.push((left, role, filler, false));
    }

    fn generates_data(&mut self, left: u32, property: Term) {
        self.tbox.data.insert(property);
        self.generating
            .push((left, ObjProp::Named(property), None, true));
    }

    fn finish(mut self) -> Tbox {
        let data = self.tbox.data.clone();
        let object = |r: &Role| !data.contains(&r.named());
        // Role inclusions hold between the inverses too.
        let mut edges: HashMap<Role, Vec<(Role, bool)>> = HashMap::new();
        for &(sub, sup, base) in &self.roles {
            edges.entry(sub).or_default().push((sup, base));
            if object(&sub) && object(&sup) {
                edges
                    .entry(sub.inverse())
                    .or_default()
                    .push((sup.inverse(), base));
            }
        }
        let mut roles: HashSet<Role> = edges.keys().copied().collect();
        roles.extend(edges.values().flatten().map(|&(r, _)| r));
        roles.extend(self.generating.iter().map(|&(_, r, _, _)| r));
        roles.extend(self.tbox.basics.iter().filter_map(|b| match b {
            Basic::Exists(r) => Some(*r),
            _ => None,
        }));
        roles.extend(
            self.tbox
                .reflexive
                .iter()
                .flat_map(|&r| [ObjProp::Named(r), ObjProp::Inverse(r)]),
        );
        let closure = |start: Role, base_only: bool| -> HashSet<Role> {
            let mut seen = HashSet::from([start]);
            let mut queue = vec![start];
            while let Some(r) = queue.pop() {
                for &(next, base) in edges.get(&r).into_iter().flatten() {
                    if (base || !base_only) && seen.insert(next) {
                        queue.push(next);
                    }
                }
            }
            seen
        };
        let mut sorted: Vec<Role> = roles.into_iter().filter(object_or_named(&data)).collect();
        sorted.sort();
        for &rho in &sorted {
            let sup = closure(rho, false);
            let base_sup = closure(rho, true);
            let from = self.basic(Basic::Exists(rho));
            let mut targets: Vec<Role> = sup.iter().copied().filter(|&s| s != rho).collect();
            targets.sort();
            for sigma in targets {
                let to = self.basic(Basic::Exists(sigma));
                self.include(from, to, base_sup.contains(&sigma));
            }
            self.tbox.sup.insert(rho, sup);
        }
        // Generating axioms: `B ⊑ ∃ρ`, never applied by the materialisation.
        let mut types: HashMap<TypeKey, usize> = HashMap::new();
        let generating = std::mem::take(&mut self.generating);
        for &(left, role, filler, data) in &generating {
            let exists = self.basic(Basic::Exists(role));
            self.include(left, exists, false);
            let next = types.len();
            let ty = *types.entry((role, filler, data)).or_insert(next);
            let generator = Generator { left, ty };
            if !self.tbox.generators.contains(&generator) {
                self.tbox.generators.push(generator);
            }
        }
        let mut keyed: Vec<(TypeKey, usize)> = types.into_iter().collect();
        keyed.sort_by_key(|&(_, ty)| ty);
        let thing = self.basic(Basic::Thing);
        let reflexive: Vec<u32> = {
            let mut r: Vec<Term> = self.tbox.reflexive.iter().copied().collect();
            r.sort();
            r.into_iter()
                .flat_map(|p| [ObjProp::Named(p), ObjProp::Inverse(p)])
                .map(|rho| self.basic(Basic::Exists(rho)))
                .collect()
        };
        let tbox = &mut self.tbox;
        tbox.types = keyed
            .into_iter()
            .map(|((role, filler, data), _)| {
                let concepts = if data {
                    HashSet::new()
                } else {
                    let mut start = vec![thing];
                    start.extend(filler);
                    if let Some(&inverse) = tbox.ids.get(&Basic::Exists(role.inverse())) {
                        start.push(inverse);
                    }
                    start.extend(reflexive.iter().copied());
                    reach(&tbox.up, start).into_iter().collect()
                };
                Type {
                    role,
                    data,
                    concepts,
                    children: Vec::new(),
                }
            })
            .collect();
        for ty in 0..tbox.types.len() {
            let mut children: Vec<usize> = tbox
                .generators
                .iter()
                .filter(|g| tbox.types[ty].concepts.contains(&g.left))
                .map(|g| g.ty)
                .collect();
            children.sort_unstable();
            children.dedup();
            tbox.types[ty].children = children;
            let role = tbox.types[ty].role;
            let mut sup: Vec<Role> = tbox
                .sup
                .get(&role)
                .map_or_else(|| vec![role], |s| s.iter().copied().collect());
            sup.sort();
            for sigma in sup {
                tbox.reaching.entry(sigma).or_default().push(ty);
            }
        }
        tbox.gaps = tbox
            .up
            .iter()
            .zip(&tbox.base_up)
            .enumerate()
            .any(|(id, (up, base))| {
                !matches!(tbox.basics[id], Basic::Fresh(_) | Basic::Thing)
                    && up.iter().any(|sup| {
                        !base.contains(sup) && !matches!(tbox.basics[*sup as usize], Basic::Thing)
                    })
            });
        self.tbox
    }
}

/// Whether a role is a property, not an inverse expression.
fn named(role: Role) -> bool {
    matches!(role, ObjProp::Named(_))
}

/// Roles that exist: data properties only as themselves.
fn object_or_named(data: &HashSet<Term>) -> impl Fn(&Role) -> bool + '_ {
    move |r| matches!(r, ObjProp::Named(_)) || !data.contains(&r.named())
}
