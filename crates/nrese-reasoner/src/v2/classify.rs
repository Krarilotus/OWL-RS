//! OWL 2 EL classification: every subsumption between named classes, and the classes
//! that can have no instance.
//!
//! The ontology is read from its RDF triples and normalised as CEL and ELK do: a complex
//! class expression gets a fresh name defined as equivalent to it, n-ary conjunctions
//! become binary ones, property chains of any length become chains of two. Saturation
//! then applies the completion rules (Baader, Brandt and Lutz, *Pushing the EL Envelope*,
//! 2005), per class, until nothing changes:
//!
//! | Rule | If | Then |
//! |---|---|---|
//! | CR1 | `D ∈ S(C)`, `D ⊑ E` | `E ∈ S(C)` |
//! | CR2 | `D1, D2 ∈ S(C)`, `D1 ⊓ D2 ⊑ E` | `E ∈ S(C)` |
//! | CR3 | `D ∈ S(C)`, `D ⊑ ∃r.E` | `C →r E` |
//! | CR4 | `C →r D`, `E ∈ S(D)`, `r ⊑* s`, `∃s.E ⊑ F` | `F ∈ S(C)` |
//! | CR5 | `C →r D`, `⊥ ∈ S(D)` | `⊥ ∈ S(C)` |
//! | CR6 | `C →r D →s E`, `r ⊑* r'`, `s ⊑* s'`, `r' ∘ s' ⊑ t` | `C →t E` |
//!
//! Supported: `rdfs:subClassOf`, `owl:equivalentClass`, `owl:disjointWith`,
//! `owl:AllDisjointClasses`, `owl:intersectionOf`, `owl:someValuesFrom` restrictions,
//! `owl:Thing`, `owl:Nothing`, `rdfs:subPropertyOf`, `owl:equivalentProperty`,
//! `owl:propertyChainAxiom`, `owl:TransitiveProperty`, `rdfs:domain`, and `rdfs:range`
//! (applied where a class axiom creates the edge). Axioms outside EL (unions, universal
//! restrictions, complements, cardinalities, inverses, nominals) are skipped and
//! counted: the hierarchy is then complete for the EL part only.

use std::collections::{HashMap, HashSet};

use super::ir::{OWL, RDF, RDFS, Vocabulary};

type Triple = [u64; 3];

/// A classification's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classification {
    /// `(sub, super)` for named classes, `sub != super`; equivalent classes appear both
    /// ways; `owl:Thing` is left out as a superclass.
    pub subsumptions: Vec<(u64, u64)>,
    /// Named classes that can have no instance (subclasses of `owl:Nothing`).
    pub unsatisfiable: Vec<u64>,
    /// Named classes equivalent to `owl:Thing` (superclasses of it): every class is a
    /// subclass of them, which `subsumptions` doesn't repeat for each.
    pub top: Vec<u64>,
    /// Axioms (by their predicate or class-expression kind) outside EL, skipped.
    pub skipped: Vec<(u64, &'static str)>,
}

struct Names {
    sub_class_of: u64,
    equivalent_class: u64,
    disjoint_with: u64,
    all_disjoint_classes: u64,
    members: u64,
    intersection_of: u64,
    some_values_from: u64,
    on_property: u64,
    first: u64,
    rest: u64,
    nil: u64,
    thing: u64,
    nothing: u64,
    sub_property_of: u64,
    equivalent_property: u64,
    property_chain: u64,
    rdf_type: u64,
    class: u64,
    rdfs_class: u64,
    transitive: u64,
    domain: u64,
    range: u64,
    outside_el: Vec<(u64, &'static str)>,
}

impl Names {
    fn new(v: &mut impl Vocabulary) -> Self {
        let owl = |v: &mut dyn FnMut(&str) -> u64, local: &str| v(&format!("{OWL}{local}"));
        let mut iri = |text: &str| v.iri(text);
        Self {
            sub_class_of: iri(&format!("{RDFS}subClassOf")),
            equivalent_class: owl(&mut iri, "equivalentClass"),
            disjoint_with: owl(&mut iri, "disjointWith"),
            all_disjoint_classes: owl(&mut iri, "AllDisjointClasses"),
            members: owl(&mut iri, "members"),
            intersection_of: owl(&mut iri, "intersectionOf"),
            some_values_from: owl(&mut iri, "someValuesFrom"),
            on_property: owl(&mut iri, "onProperty"),
            first: iri(&format!("{RDF}first")),
            rest: iri(&format!("{RDF}rest")),
            nil: iri(&format!("{RDF}nil")),
            thing: owl(&mut iri, "Thing"),
            nothing: owl(&mut iri, "Nothing"),
            sub_property_of: iri(&format!("{RDFS}subPropertyOf")),
            equivalent_property: owl(&mut iri, "equivalentProperty"),
            property_chain: owl(&mut iri, "propertyChainAxiom"),
            rdf_type: iri(&format!("{RDF}type")),
            class: owl(&mut iri, "Class"),
            rdfs_class: iri(&format!("{RDFS}Class")),
            transitive: owl(&mut iri, "TransitiveProperty"),
            domain: iri(&format!("{RDFS}domain")),
            range: iri(&format!("{RDFS}range")),
            outside_el: [
                ("unionOf", "union"),
                ("allValuesFrom", "universal restriction"),
                ("complementOf", "complement"),
                ("oneOf", "nominal"),
                ("hasValue", "nominal"),
                ("cardinality", "cardinality"),
                ("minCardinality", "cardinality"),
                ("maxCardinality", "cardinality"),
                ("qualifiedCardinality", "cardinality"),
                ("minQualifiedCardinality", "cardinality"),
                ("maxQualifiedCardinality", "cardinality"),
                ("inverseOf", "inverse property"),
            ]
            .into_iter()
            .map(|(local, kind)| (owl(&mut iri, local), kind))
            .collect(),
        }
    }
}

/// A class expression, as far as EL has it.
enum Expr {
    Named(u64),
    And(Vec<Expr>),
    Some(u64, Box<Expr>),
}

type Concept = u32;
type Role = u32;

#[derive(Default)]
struct Tbox {
    /// Concept index of each named class (and of the fresh ones: `None` there).
    named: Vec<Option<u64>>,
    index: HashMap<u64, Concept>,
    told: Vec<Vec<Concept>>,
    /// `A ⊓ B ⊑ D`, indexed by `A` (and by `B`).
    conjunctions: Vec<Vec<(Concept, Concept)>>,
    /// `A ⊑ ∃r.B`, by `A`.
    existentials: Vec<Vec<(Role, Concept)>>,
    /// `∃r.B ⊑ D`, by `B`.
    filler_of: Vec<Vec<(Role, Concept)>>,
    roles: HashMap<u64, Role>,
    role_count: u32,
    sub_roles: Vec<(Role, Role)>,
    chains: Vec<(Role, Role, Role)>,
    ranges: Vec<(Role, Concept)>,
    /// Interned conjunctions of fresh names (`and(a, b)`), so that equal ones share one.
    ands: HashMap<(Concept, Concept), Concept>,
    somes: HashMap<(Role, Concept), Concept>,
}

impl Tbox {
    fn concept(&mut self, named: Option<u64>) -> Concept {
        if let Some(id) = named
            && let Some(&c) = self.index.get(&id)
        {
            return c;
        }
        let c = self.named.len() as Concept;
        self.named.push(named);
        self.told.push(Vec::new());
        self.conjunctions.push(Vec::new());
        self.existentials.push(Vec::new());
        self.filler_of.push(Vec::new());
        if let Some(id) = named {
            self.index.insert(id, c);
        }
        c
    }

    fn role(&mut self, id: Option<u64>) -> Role {
        if let Some(id) = id
            && let Some(&r) = self.roles.get(&id)
        {
            return r;
        }
        let r = self.role_count;
        self.role_count += 1;
        if let Some(id) = id {
            self.roles.insert(id, r);
        }
        r
    }

    fn sub(&mut self, a: Concept, b: Concept) {
        if a != b {
            self.told[a as usize].push(b);
        }
    }

    fn and_sub(&mut self, a: Concept, b: Concept, d: Concept) {
        self.conjunctions[a as usize].push((b, d));
        self.conjunctions[b as usize].push((a, d));
    }

    /// A concept equivalent to `a ⊓ b`.
    fn and(&mut self, a: Concept, b: Concept) -> Concept {
        if a == b {
            return a;
        }
        let key = (a.min(b), a.max(b));
        if let Some(&c) = self.ands.get(&key) {
            return c;
        }
        let c = self.concept(None);
        self.sub(c, a);
        self.sub(c, b);
        self.and_sub(a, b, c);
        self.ands.insert(key, c);
        c
    }

    /// A concept equivalent to `∃r.b`.
    fn some(&mut self, r: Role, b: Concept) -> Concept {
        if let Some(&c) = self.somes.get(&(r, b)) {
            return c;
        }
        let c = self.concept(None);
        self.existentials[c as usize].push((r, b));
        self.filler_of[b as usize].push((r, c));
        self.somes.insert((r, b), c);
        c
    }

    fn name(&mut self, expr: &Expr) -> Concept {
        match expr {
            Expr::Named(id) => self.concept(Some(*id)),
            Expr::And(parts) => {
                let concepts: Vec<Concept> = parts.iter().map(|p| self.name(p)).collect();
                let (&first, rest) = concepts.split_first().expect("a conjunction has parts");
                rest.iter().fold(first, |acc, &c| self.and(acc, c))
            }
            Expr::Some(role, filler) => {
                let r = self.role(Some(*role));
                let b = self.name(filler);
                self.some(r, b)
            }
        }
    }
}

/// The triples of an ontology, indexed by subject.
struct Graph<'a> {
    by_subject: HashMap<u64, Vec<(u64, u64)>>,
    names: &'a Names,
}

impl Graph<'_> {
    fn objects(&self, subject: u64, predicate: u64) -> impl Iterator<Item = u64> + '_ {
        self.by_subject
            .get(&subject)
            .into_iter()
            .flatten()
            .filter(move |(p, _)| *p == predicate)
            .map(|(_, o)| *o)
    }

    fn one(&self, subject: u64, predicate: u64) -> Option<u64> {
        self.objects(subject, predicate).next()
    }

    fn list(&self, mut node: u64) -> Option<Vec<u64>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        while node != self.names.nil {
            if !seen.insert(node) || out.len() > 10_000 {
                return None;
            }
            out.push(self.one(node, self.names.first)?);
            node = self.one(node, self.names.rest)?;
        }
        Some(out)
    }

    /// The class expression `node` is; `Err` names what EL lacks.
    fn expr(
        &self,
        node: u64,
        named: &dyn Fn(u64) -> bool,
        depth: usize,
    ) -> Result<Expr, &'static str> {
        if depth > 64 {
            return Err("too deeply nested");
        }
        if named(node) {
            return Ok(Expr::Named(node));
        }
        if let Some(list) = self.one(node, self.names.intersection_of) {
            let members = self.list(list).ok_or("malformed list")?;
            if members.is_empty() {
                return Err("empty intersection");
            }
            return members
                .into_iter()
                .map(|m| self.expr(m, named, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Expr::And);
        }
        if let (Some(filler), Some(role)) = (
            self.one(node, self.names.some_values_from),
            self.one(node, self.names.on_property),
        ) {
            if !named(role) {
                return Err("inverse property");
            }
            return Ok(Expr::Some(
                role,
                Box::new(self.expr(filler, named, depth + 1)?),
            ));
        }
        for &(predicate, kind) in &self.names.outside_el {
            if self.one(node, predicate).is_some() {
                return Err(kind);
            }
        }
        Err("unknown class expression")
    }
}

/// Classifies the EL part of the ontology in `triples`. `named` tells IRIs (named classes
/// and properties) from blank nodes (class expressions, lists).
pub fn classify(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
) -> Classification {
    let names = Names::new(vocabulary);
    let mut by_subject: HashMap<u64, Vec<(u64, u64)>> = HashMap::new();
    for &[s, p, o] in triples {
        by_subject.entry(s).or_default().push((p, o));
    }
    let graph = Graph {
        by_subject,
        names: &names,
    };
    let mut tbox = Tbox::default();
    let thing = tbox.concept(Some(names.thing));
    let nothing = tbox.concept(Some(names.nothing));
    let mut skipped = Vec::new();
    let expr = |node: u64, predicate: u64, skipped: &mut Vec<(u64, &'static str)>| match graph
        .expr(node, named, 0)
    {
        Ok(e) => Some(e),
        Err(kind) => {
            skipped.push((predicate, kind));
            None
        }
    };
    // Every declared class is classified, also one no axiom names.
    for &[s, p, o] in triples {
        if p == names.rdf_type && named(s) && (o == names.class || o == names.rdfs_class) {
            let _ = tbox.concept(Some(s));
        }
    }
    for &[s, p, o] in triples {
        if p == names.sub_class_of || p == names.equivalent_class || p == names.disjoint_with {
            let (Some(a), Some(b)) = (expr(s, p, &mut skipped), expr(o, p, &mut skipped)) else {
                continue;
            };
            let (a, b) = (tbox.name(&a), tbox.name(&b));
            if p == names.disjoint_with {
                tbox.and_sub(a, b, nothing);
            } else {
                tbox.sub(a, b);
                if p == names.equivalent_class {
                    tbox.sub(b, a);
                }
            }
        } else if p == names.rdf_type && o == names.all_disjoint_classes {
            let Some(members) = graph.one(s, names.members).and_then(|l| graph.list(l)) else {
                continue;
            };
            let concepts: Vec<Concept> = members
                .iter()
                .filter_map(|&m| expr(m, p, &mut skipped))
                .map(|e| tbox.name(&e))
                .collect();
            for (i, &a) in concepts.iter().enumerate() {
                for &b in &concepts[i + 1..] {
                    tbox.and_sub(a, b, nothing);
                }
            }
        } else if (p == names.sub_property_of || p == names.equivalent_property)
            && named(s)
            && named(o)
        {
            let (r, t) = (tbox.role(Some(s)), tbox.role(Some(o)));
            tbox.sub_roles.push((r, t));
            if p == names.equivalent_property {
                tbox.sub_roles.push((t, r));
            }
        } else if p == names.rdf_type && o == names.transitive && named(s) {
            let r = tbox.role(Some(s));
            tbox.chains.push((r, r, r));
        } else if p == names.property_chain && named(s) {
            let Some(links) = graph.list(o) else {
                skipped.push((p, "malformed list"));
                continue;
            };
            if links.len() < 2 || !links.iter().all(|&l| named(l)) {
                skipped.push((p, "chain of inverse or fewer than two properties"));
                continue;
            }
            let target = tbox.role(Some(s));
            let mut roles: Vec<Role> = links.iter().map(|&l| tbox.role(Some(l))).collect();
            // r1 ∘ r2 ∘ … ∘ rn ⊑ s as (((r1 ∘ r2) ∘ r3) …) with fresh roles.
            while roles.len() > 2 {
                let fresh = tbox.role(None);
                tbox.chains.push((roles[0], roles[1], fresh));
                roles.splice(0..2, [fresh]);
            }
            tbox.chains.push((roles[0], roles[1], target));
        } else if p == names.domain && named(s) {
            let Some(c) = expr(o, p, &mut skipped) else {
                continue;
            };
            let (r, c) = (tbox.role(Some(s)), tbox.name(&c));
            let some = tbox.some(r, thing);
            tbox.sub(some, c);
        } else if p == names.range && named(s) {
            let Some(c) = expr(o, p, &mut skipped) else {
                continue;
            };
            let (r, c) = (tbox.role(Some(s)), tbox.name(&c));
            tbox.ranges.push((r, c));
        }
    }
    saturate(&mut tbox, thing, nothing, skipped)
}

fn saturate(
    tbox: &mut Tbox,
    thing: Concept,
    nothing: Concept,
    skipped: Vec<(u64, &'static str)>,
) -> Classification {
    let roles = tbox.role_count as usize;
    // Every role's super-roles (reflexive, transitive).
    let mut supers: Vec<Vec<Role>> = (0..roles as Role).map(|r| vec![r]).collect();
    loop {
        let mut changed = false;
        for &(r, s) in &tbox.sub_roles {
            let add: Vec<Role> = supers[s as usize].clone();
            for t in add {
                if !supers[r as usize].contains(&t) {
                    supers[r as usize].push(t);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Ranges apply to every edge of a sub-role: `A ⊑ ∃r.B` becomes `A ⊑ ∃r.(B ⊓ R)`.
    if !tbox.ranges.is_empty() {
        let ranges = tbox.ranges.clone();
        for c in 0..tbox.named.len() {
            let edges = std::mem::take(&mut tbox.existentials[c]);
            let mut updated = Vec::with_capacity(edges.len());
            for (r, b) in edges {
                let mut target = b;
                for &(ranged, class) in &ranges {
                    if supers[r as usize].contains(&ranged) {
                        target = tbox.and(target, class);
                    }
                }
                updated.push((r, target));
            }
            tbox.existentials[c] = updated;
        }
    }
    let mut chains_first: Vec<Vec<(Role, Role)>> = vec![Vec::new(); roles];
    let mut chains_second: Vec<Vec<(Role, Role)>> = vec![Vec::new(); roles];
    for &(r1, r2, t) in &tbox.chains {
        chains_first[r1 as usize].push((r2, t));
        chains_second[r2 as usize].push((r1, t));
    }
    let n = tbox.named.len();
    let mut subsumers: Vec<HashSet<Concept>> = vec![HashSet::new(); n];
    let mut succ: Vec<Vec<(Role, Concept)>> = vec![Vec::new(); n];
    let mut pred: Vec<Vec<(Concept, Role)>> = vec![Vec::new(); n];
    let mut links: HashSet<(Concept, Role, Concept)> = HashSet::new();
    let mut initialised = vec![false; n];
    enum Item {
        Sub(Concept, Concept),
        Link(Concept, Role, Concept),
    }
    let mut todo: Vec<Item> = Vec::new();
    let init = |c: Concept, initialised: &mut Vec<bool>, todo: &mut Vec<Item>| {
        if !initialised[c as usize] {
            initialised[c as usize] = true;
            todo.push(Item::Sub(c, c));
            todo.push(Item::Sub(c, thing));
        }
    };
    for c in 0..n as Concept {
        if tbox.named[c as usize].is_some() {
            init(c, &mut initialised, &mut todo);
        }
    }
    while let Some(item) = todo.pop() {
        match item {
            Item::Sub(c, d) => {
                if !subsumers[c as usize].insert(d) {
                    continue;
                }
                for &e in &tbox.told[d as usize] {
                    todo.push(Item::Sub(c, e));
                }
                for &(b, e) in &tbox.conjunctions[d as usize] {
                    if subsumers[c as usize].contains(&b) {
                        todo.push(Item::Sub(c, e));
                    }
                }
                for &(r, b) in &tbox.existentials[d as usize] {
                    todo.push(Item::Link(c, r, b));
                }
                for &(from, r) in &pred[c as usize] {
                    if d == nothing {
                        todo.push(Item::Sub(from, nothing));
                    }
                    for &(s, f) in &tbox.filler_of[d as usize] {
                        if supers[r as usize].contains(&s) {
                            todo.push(Item::Sub(from, f));
                        }
                    }
                }
            }
            Item::Link(c, r, d) => {
                if !links.insert((c, r, d)) {
                    continue;
                }
                init(d, &mut initialised, &mut todo);
                succ[c as usize].push((r, d));
                pred[d as usize].push((c, r));
                for &e in &subsumers[d as usize] {
                    if e == nothing {
                        todo.push(Item::Sub(c, nothing));
                    }
                    for &(s, f) in &tbox.filler_of[e as usize] {
                        if supers[r as usize].contains(&s) {
                            todo.push(Item::Sub(c, f));
                        }
                    }
                }
                // Chains with this edge first: c →r d →r2 e.
                for &s1 in &supers[r as usize] {
                    for &(r2, t) in &chains_first[s1 as usize] {
                        for &(r2_edge, e) in &succ[d as usize] {
                            if supers[r2_edge as usize].contains(&r2) {
                                todo.push(Item::Link(c, t, e));
                            }
                        }
                    }
                }
                // ... and second: b →r1 c →r d.
                for &s2 in &supers[r as usize] {
                    for &(r1, t) in &chains_second[s2 as usize] {
                        for &(b, r1_edge) in &pred[c as usize] {
                            if supers[r1_edge as usize].contains(&r1) {
                                todo.push(Item::Link(b, t, d));
                            }
                        }
                    }
                }
            }
        }
    }
    let mut result = Classification {
        skipped,
        ..Classification::default()
    };
    let named_ids: Vec<(Concept, u64)> = tbox
        .named
        .iter()
        .enumerate()
        .filter_map(|(c, id)| id.map(|id| (c as Concept, id)))
        .collect();
    let top = &subsumers[thing as usize];
    for &(d, id) in &named_ids {
        if d != thing && d != nothing && top.contains(&d) {
            result.top.push(id);
        }
    }
    for &(c, id) in &named_ids {
        if c == thing || c == nothing {
            continue;
        }
        let set = &subsumers[c as usize];
        if set.contains(&nothing) {
            result.unsatisfiable.push(id);
            continue;
        }
        for &(d, super_id) in &named_ids {
            if d != c && d != thing && set.contains(&d) {
                result.subsumptions.push((id, super_id));
            }
        }
    }
    result.subsumptions.sort_unstable();
    result.unsatisfiable.sort_unstable();
    result.top.sort_unstable();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::vocabulary::LocalVocabulary;

    const EX: &str = "http://example.com/";

    /// Loads `s p o` lines of prefixed names (`ex:`, `rdf:`, `rdfs:`, `owl:`, `_:b`).
    fn load(v: &mut LocalVocabulary, text: &str) -> Vec<Triple> {
        let mut term = |t: &str| -> u64 {
            if t.starts_with("_:") {
                return v.term(t);
            }
            let (prefix, local) = t.split_once(':').unwrap();
            let ns = match prefix {
                "ex" => EX,
                "rdf" => RDF,
                "rdfs" => RDFS,
                "owl" => OWL,
                other => panic!("{other}"),
            };
            v.iri(&format!("{ns}{local}"))
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                [term(parts[0]), term(parts[1]), term(parts[2])]
            })
            .collect()
    }

    fn run(text: &str) -> (LocalVocabulary, Classification) {
        let mut v = LocalVocabulary::default();
        let triples = load(&mut v, text);
        let snapshot = v.clone();
        let named = move |id: u64| snapshot.text(id).starts_with('<');
        let result = classify(&triples, &mut v, &named);
        (v, result)
    }

    fn holds(v: &mut LocalVocabulary, result: &Classification, sub: &str, sup: &str) -> bool {
        let (a, b) = (v.iri(&format!("{EX}{sub}")), v.iri(&format!("{EX}{sup}")));
        result.subsumptions.binary_search(&(a, b)).is_ok()
    }

    /// The textbook example (Baader et al.): pericarditis is a heart disease, through an
    /// existential, a conjunction and a property chain.
    #[test]
    fn pericarditis_is_a_heart_disease() {
        let (mut v, result) = run("ex:Pericardium rdfs:subClassOf _:i1
             _:i1 owl:intersectionOf _:l1
             _:l1 rdf:first ex:Tissue
             _:l1 rdf:rest _:l2
             _:l2 rdf:first _:r1
             _:l2 rdf:rest rdf:nil
             _:r1 owl:onProperty ex:partOf
             _:r1 owl:someValuesFrom ex:Heart
             ex:Pericarditis owl:equivalentClass _:i2
             _:i2 owl:intersectionOf _:m1
             _:m1 rdf:first ex:Inflammation
             _:m1 rdf:rest _:m2
             _:m2 rdf:first _:r2
             _:m2 rdf:rest rdf:nil
             _:r2 owl:onProperty ex:hasLocation
             _:r2 owl:someValuesFrom ex:Pericardium
             ex:Inflammation rdfs:subClassOf ex:Disease
             ex:HeartDisease owl:equivalentClass _:i3
             _:i3 owl:intersectionOf _:n1
             _:n1 rdf:first ex:Disease
             _:n1 rdf:rest _:n2
             _:n2 rdf:first _:r3
             _:n2 rdf:rest rdf:nil
             _:r3 owl:onProperty ex:hasLocation
             _:r3 owl:someValuesFrom ex:Heart
             ex:hasLocation owl:propertyChainAxiom _:c1
             _:c1 rdf:first ex:hasLocation
             _:c1 rdf:rest _:c2
             _:c2 rdf:first ex:partOf
             _:c2 rdf:rest rdf:nil");
        assert!(holds(&mut v, &result, "Pericarditis", "HeartDisease"));
        assert!(holds(&mut v, &result, "Pericarditis", "Disease"));
        assert!(holds(&mut v, &result, "Pericardium", "Tissue"));
        assert!(!holds(&mut v, &result, "HeartDisease", "Pericarditis"));
        assert!(result.unsatisfiable.is_empty());
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
    }

    #[test]
    fn roles_domains_ranges_disjointness_and_transitivity() {
        let (mut v, result) = run("ex:hasParent rdfs:subPropertyOf ex:hasAncestor
             ex:hasAncestor rdf:type owl:TransitiveProperty
             ex:hasParent rdfs:domain ex:Person
             ex:hasParent rdfs:range ex:Person
             ex:Child rdfs:subClassOf _:r1
             _:r1 owl:onProperty ex:hasParent
             _:r1 owl:someValuesFrom ex:Parent
             ex:Parent rdfs:subClassOf _:r2
             _:r2 owl:onProperty ex:hasParent
             _:r2 owl:someValuesFrom ex:Elder
             ex:GrandChild owl:equivalentClass _:r3
             _:r3 owl:onProperty ex:hasAncestor
             _:r3 owl:someValuesFrom _:r4
             _:r4 owl:onProperty ex:hasAncestor
             _:r4 owl:someValuesFrom ex:Elder
             ex:PersonDescendant owl:equivalentClass _:r5
             _:r5 owl:onProperty ex:hasParent
             _:r5 owl:someValuesFrom ex:Person
             ex:Stone owl:disjointWith ex:Person
             ex:Golem rdfs:subClassOf ex:Stone
             ex:Golem rdfs:subClassOf ex:Child
             ex:Top owl:equivalentClass owl:Thing
             ex:Anything rdfs:subClassOf ex:Top");
        // Domain: a child has a parent, so it is a person.
        assert!(holds(&mut v, &result, "Child", "Person"));
        // Range: the parent is a person, so a child is a person-descendant.
        assert!(holds(&mut v, &result, "Child", "PersonDescendant"));
        // Transitivity: hasParent ⊑ hasAncestor, ancestor of an ancestor.
        assert!(holds(&mut v, &result, "Child", "GrandChild"));
        assert!(!holds(&mut v, &result, "Parent", "GrandChild"));
        // Disjointness: a golem would be a stone and a person.
        let golem = v.iri(&format!("{EX}Golem"));
        assert_eq!(result.unsatisfiable, vec![golem]);
        // owl:Thing is no superclass in the output; an equivalent of it is, and is
        // reported as one.
        assert!(holds(&mut v, &result, "Anything", "Top"));
        let top = v.iri(&format!("{EX}Top"));
        assert_eq!(result.top, vec![top]);
    }

    #[test]
    fn axioms_outside_el_are_skipped_and_counted() {
        let (mut v, result) = run("ex:A rdfs:subClassOf _:u
             _:u owl:unionOf _:l
             _:l rdf:first ex:B
             _:l rdf:rest rdf:nil
             ex:C rdfs:subClassOf _:a
             _:a owl:onProperty ex:p
             _:a owl:allValuesFrom ex:D
             ex:E rdfs:subClassOf ex:F");
        assert_eq!(result.skipped.len(), 2, "{:?}", result.skipped);
        assert!(holds(&mut v, &result, "E", "F"));
    }
}
