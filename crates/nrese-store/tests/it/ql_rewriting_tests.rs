//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md §6): the store's
//! answers (the `owl2-ql` or `owl2-rl` closure, queried with the tree-witness rewriting)
//! against certain answers computed apart, by a bounded chase, on random small QL
//! ontologies, data and conjunctive queries.
//!
//! The chase reads the generated axioms directly, not the store's reading of their RDF:
//! inclusions between classes and existentials, generating axioms and role inclusions with
//! inverses, applied until nothing changes, with a new anonymous element for each element
//! and generating axiom up to a depth that a match of the query never needs to exceed
//! (the generated types, then the query's variables). A query's certain answers are its
//! matches on the named individuals.
//!
//! Mixed ontologies add what OWL 2 QL leaves out on purpose: transitive properties,
//! property chains, functional and inverse functional properties, and `owl:sameAs` in the
//! data. The chase applies them too (equality by merging elements, named ones kept as
//! representatives). The store's answers must all be certain, and every query that misses
//! one must say `sound-only` (docs/design/ql-rewriting.md §7).
//!
//! `NRESE_QL_CASES` sets the number of cases (default 150 per test), `NRESE_QL_SEED` the
//! seed.

use std::collections::{BTreeSet, HashMap, HashSet};

use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::Completeness;
use nrese_store::{
    BulkLoadRequest, GraphTarget, SolutionsResultFormat, SparqlQueryRequest, StoreConfig,
    StoreService,
};

const E: &str = "http://e/";
const CLASSES: usize = 4;
const PROPERTIES: usize = 3;
const INDIVIDUALS: usize = 4;
/// Edges a chase may hold: transitivity and chains over many anonymous elements are
/// quadratic. A case past it is skipped (and counted).
const MAX_EDGES: usize = 50_000;

/// xorshift64*: deterministic cases without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// A role: a property, or its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Role {
    property: usize,
    inverse: bool,
}

impl Role {
    fn inverse(self) -> Self {
        Self {
            inverse: !self.inverse,
            ..self
        }
    }

    fn turtle(self) -> String {
        if self.inverse {
            format!("[ owl:inverseOf :P{} ]", self.property)
        } else {
            format!(":P{}", self.property)
        }
    }
}

/// A basic concept on the left of an inclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Basic {
    Class(usize),
    Exists(Role),
}

/// The TBox as the chase reads it.
#[derive(Debug, Default)]
struct Tbox {
    /// `left ⊑ class`.
    inclusions: Vec<(Basic, usize)>,
    /// `left ⊑ ∃role.filler`.
    generating: Vec<(Basic, Role, Option<usize>)>,
    /// `sub ⊑ sup`, closed under inverses.
    roles: Vec<(Role, Role)>,
    turtle: Vec<String>,
}

impl Tbox {
    fn role_inclusion(&mut self, sub: Role, sup: Role) {
        self.roles.push((sub, sup));
        self.roles.push((sub.inverse(), sup.inverse()));
    }
}

/// Axioms beyond OWL 2 QL, and equalities in the data.
#[derive(Debug, Default)]
struct Extras {
    /// `p ∘ q ⊑ r` (a transitive `p` is `p ∘ p ⊑ p`).
    chains: Vec<(usize, usize, usize)>,
    functional: Vec<usize>,
    inverse_functional: Vec<usize>,
    same: Vec<(usize, usize)>,
    turtle: Vec<String>,
}

/// Extras over the properties, mostly over `generating` ones (the roles of existentials),
/// where they meet anonymous individuals.
fn random_extras(rng: &mut Rng, generating: &[usize]) -> Extras {
    let mut e = Extras::default();
    for _ in 0..1 + rng.below(3) {
        let p = if !generating.is_empty() && rng.chance(70) {
            generating[rng.below(generating.len())]
        } else {
            rng.below(PROPERTIES)
        };
        match rng.below(5) {
            0 | 1 => {
                e.chains.push((p, p, p));
                e.turtle.push(format!(":P{p} a owl:TransitiveProperty ."));
            }
            2 => {
                let (q, r) = (rng.below(PROPERTIES), rng.below(PROPERTIES));
                e.chains.push((p, q, r));
                e.turtle
                    .push(format!(":P{r} owl:propertyChainAxiom ( :P{p} :P{q} ) ."));
            }
            3 => {
                e.functional.push(p);
                e.turtle.push(format!(":P{p} a owl:FunctionalProperty ."));
            }
            _ => {
                e.inverse_functional.push(p);
                e.turtle
                    .push(format!(":P{p} a owl:InverseFunctionalProperty ."));
            }
        }
    }
    if rng.chance(40) {
        let (a, b) = (rng.below(INDIVIDUALS), rng.below(INDIVIDUALS));
        e.same.push((a, b));
        e.turtle.push(format!(":a{a} owl:sameAs :a{b} ."));
    }
    e
}

fn restriction(role: Role, filler: Option<usize>) -> String {
    let filler = filler.map_or_else(|| "owl:Thing".to_owned(), |c| format!(":C{c}"));
    format!(
        "[ a owl:Restriction ; owl:onProperty {} ; owl:someValuesFrom {filler} ]",
        role.turtle()
    )
}

fn random_tbox(rng: &mut Rng) -> Tbox {
    let mut t = Tbox::default();
    let class = |rng: &mut Rng| rng.below(CLASSES);
    let role = |rng: &mut Rng| Role {
        property: rng.below(PROPERTIES),
        inverse: rng.chance(30),
    };
    let axioms = 2 + rng.below(6);
    let mut generating = 0;
    for _ in 0..axioms {
        let kind = rng.below(12);
        // At most three generating axioms keep the chase small.
        if matches!(kind, 1 | 2 | 8 | 9) && generating == 3 {
            continue;
        }
        match kind {
            0 => {
                let (a, b) = (class(rng), class(rng));
                t.inclusions.push((Basic::Class(a), b));
                t.turtle.push(format!(":C{a} rdfs:subClassOf :C{b} ."));
            }
            1 | 2 => {
                let (a, r) = (class(rng), role(rng));
                let filler = (kind == 1).then(|| class(rng));
                t.generating.push((Basic::Class(a), r, filler));
                t.turtle.push(format!(
                    ":C{a} rdfs:subClassOf {} .",
                    restriction(r, filler)
                ));
                generating += 1;
            }
            3 | 4 => {
                let (p, a) = (rng.below(PROPERTIES), class(rng));
                let r = Role {
                    property: p,
                    inverse: kind == 4,
                };
                t.inclusions.push((Basic::Exists(r), a));
                let key = if kind == 3 { "domain" } else { "range" };
                t.turtle.push(format!(":P{p} rdfs:{key} :C{a} ."));
            }
            5 => {
                let (p, q) = (rng.below(PROPERTIES), rng.below(PROPERTIES));
                t.role_inclusion(
                    Role {
                        property: p,
                        inverse: false,
                    },
                    Role {
                        property: q,
                        inverse: false,
                    },
                );
                t.turtle.push(format!(":P{p} rdfs:subPropertyOf :P{q} ."));
            }
            6 => {
                let (p, q) = (rng.below(PROPERTIES), rng.below(PROPERTIES));
                let (p_, q_) = (
                    Role {
                        property: p,
                        inverse: false,
                    },
                    Role {
                        property: q,
                        inverse: false,
                    },
                );
                t.role_inclusion(p_, q_.inverse());
                t.role_inclusion(q_.inverse(), p_);
                t.turtle.push(format!(":P{p} owl:inverseOf :P{q} ."));
            }
            7 => {
                let (r, a) = (role(rng), class(rng));
                t.inclusions.push((Basic::Exists(r), a));
                t.turtle
                    .push(format!("{} rdfs:subClassOf :C{a} .", restriction(r, None)));
            }
            8 => {
                let (a, b, c, r) = (class(rng), class(rng), class(rng), role(rng));
                t.inclusions.push((Basic::Class(a), b));
                t.generating.push((Basic::Class(a), r, Some(c)));
                t.turtle.push(format!(
                    ":C{a} rdfs:subClassOf [ owl:intersectionOf ( :C{b} {} ) ] .",
                    restriction(r, Some(c))
                ));
                generating += 1;
            }
            9 => {
                let (p, r, c) = (rng.below(PROPERTIES), role(rng), class(rng));
                let left = Role {
                    property: p,
                    inverse: false,
                };
                t.generating.push((Basic::Exists(left), r, Some(c)));
                t.turtle
                    .push(format!(":P{p} rdfs:domain {} .", restriction(r, Some(c))));
                generating += 1;
            }
            10 => {
                let (a, b) = (class(rng), class(rng));
                t.inclusions.push((Basic::Class(a), b));
                t.inclusions.push((Basic::Class(b), a));
                t.turtle.push(format!(":C{a} owl:equivalentClass :C{b} ."));
            }
            _ => {
                let p = rng.below(PROPERTIES);
                let r = Role {
                    property: p,
                    inverse: false,
                };
                t.role_inclusion(r, r.inverse());
                t.turtle.push(format!(":P{p} a owl:SymmetricProperty ."));
            }
        }
    }
    t
}

/// A query term: a variable or an individual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T {
    Var(usize),
    Ind(usize),
}

#[derive(Debug, Clone)]
enum QAtom {
    Class(T, usize),
    Role(T, usize, T),
}

struct Query {
    atoms: Vec<QAtom>,
    vars: usize,
    answers: Vec<usize>,
}

impl Query {
    fn sparql(&self, distinct: bool) -> String {
        let term = |t: &T| match t {
            T::Var(v) => format!("?v{v}"),
            T::Ind(i) => format!(":a{i}"),
        };
        let body: Vec<String> = self
            .atoms
            .iter()
            .map(|a| match a {
                QAtom::Class(t, c) => format!("{} a :C{c} .", term(t)),
                QAtom::Role(s, p, o) => format!("{} :P{p} {} .", term(s), term(o)),
            })
            .collect();
        let head = if self.answers.is_empty() {
            "(1 AS ?k)".to_owned()
        } else {
            self.answers
                .iter()
                .map(|v| format!("?v{v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "PREFIX : <{E}> SELECT {}{head} WHERE {{ {} }}",
            if distinct { "DISTINCT " } else { "" },
            body.join(" ")
        )
    }
}

fn random_query(rng: &mut Rng) -> Query {
    let atoms = 1 + rng.below(4);
    let vars = 1 + rng.below(4);
    let term = |rng: &mut Rng| {
        if rng.chance(10) {
            T::Ind(rng.below(INDIVIDUALS))
        } else {
            T::Var(rng.below(vars))
        }
    };
    let atoms: Vec<QAtom> = (0..atoms)
        .map(|_| {
            if rng.chance(40) {
                QAtom::Class(term(rng), rng.below(CLASSES))
            } else {
                QAtom::Role(term(rng), rng.below(PROPERTIES), term(rng))
            }
        })
        .collect();
    let used: BTreeSet<usize> = atoms
        .iter()
        .flat_map(|a| match a {
            QAtom::Class(t, _) => vec![*t],
            QAtom::Role(s, _, o) => vec![*s, *o],
        })
        .filter_map(|t| match t {
            T::Var(v) => Some(v),
            T::Ind(_) => None,
        })
        .collect();
    let answers = used.into_iter().filter(|_| rng.chance(50)).collect();
    Query {
        atoms,
        vars,
        answers,
    }
}

/// The chase: elements `0..INDIVIDUALS` are the individuals, the others anonymous.
struct Chase {
    depth: Vec<usize>,
    types: HashSet<(usize, usize)>,
    /// `(subject, property, object)`.
    edges: HashSet<(usize, usize, usize)>,
    /// `(element, role)` for each element with an edge of the role.
    has: HashSet<(usize, Role)>,
    /// Union-find: each element's representative is the least of its class, so a class
    /// with an individual is represented by one.
    rep: Vec<usize>,
}

impl Chase {
    fn holds(&self, x: usize, basic: Basic) -> bool {
        match basic {
            Basic::Class(c) => self.types.contains(&(x, c)),
            Basic::Exists(r) => self.has.contains(&(x, r)),
        }
    }

    fn add_edge(&mut self, edge: (usize, usize, usize)) -> bool {
        let (s, p, o) = edge;
        self.has.insert((
            s,
            Role {
                property: p,
                inverse: false,
            },
        ));
        self.has.insert((
            o,
            Role {
                property: p,
                inverse: true,
            },
        ));
        self.edges.insert(edge)
    }

    fn find(&self, mut x: usize) -> usize {
        while self.rep[x] != x {
            x = self.rep[x];
        }
        x
    }

    /// Makes `a` and `b` one element; whether they were two.
    fn merge(&mut self, a: usize, b: usize) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return false;
        }
        let (low, high) = (a.min(b), a.max(b));
        self.rep[high] = low;
        self.depth[low] = self.depth[low].min(self.depth[high]);
        true
    }

    /// Every fact over representatives.
    fn canonical(&mut self) {
        let types: HashSet<(usize, usize)> =
            self.types.iter().map(|&(x, c)| (self.find(x), c)).collect();
        let edges: Vec<(usize, usize, usize)> = self
            .edges
            .iter()
            .map(|&(x, p, y)| (self.find(x), p, self.find(y)))
            .collect();
        self.types = types;
        self.edges.clear();
        self.has.clear();
        for edge in edges {
            self.add_edge(edge);
        }
    }

    /// The individuals an element stands for.
    fn names(&self, x: usize) -> Vec<usize> {
        (0..INDIVIDUALS).filter(|&i| self.find(i) == x).collect()
    }

    /// The certain answers with each element written as every individual it is.
    fn named_answers(&self, query: &Query) -> BTreeSet<Vec<usize>> {
        let mut out = BTreeSet::new();
        for tuple in self.answers(query) {
            let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
            for &x in &tuple {
                rows = rows
                    .into_iter()
                    .flat_map(|row| {
                        self.names(x).into_iter().map(move |n| {
                            let mut row = row.clone();
                            row.push(n);
                            row
                        })
                    })
                    .collect();
            }
            out.extend(rows);
        }
        out
    }

    /// Runs `tbox` and `extras` over the facts, anonymous elements to `max_depth`; `None`
    /// past `max_elements`.
    fn run(
        tbox: &Tbox,
        extras: &Extras,
        types: &[(usize, usize)],
        edges: &[(usize, usize, usize)],
        max_depth: usize,
        max_elements: usize,
    ) -> Option<Self> {
        let mut chase = Self {
            depth: vec![0; INDIVIDUALS],
            types: types.iter().copied().collect(),
            edges: HashSet::new(),
            has: HashSet::new(),
            rep: (0..INDIVIDUALS).collect(),
        };
        for &edge in edges {
            chase.add_edge(edge);
        }
        for &(a, b) in &extras.same {
            chase.merge(a, b);
        }
        chase.canonical();
        let mut generated: HashSet<(usize, usize)> = HashSet::new();
        loop {
            let mut changed = false;
            let elements = chase.depth.len();
            // Chains and transitivity, then the equalities functional properties force.
            for &(p, q, r) in &extras.chains {
                let mut out_q: HashMap<usize, Vec<usize>> = HashMap::new();
                for &(y, s, z) in &chase.edges {
                    if s == q {
                        out_q.entry(y).or_default().push(z);
                    }
                }
                let new: Vec<(usize, usize, usize)> = chase
                    .edges
                    .iter()
                    .filter(|&&(_, s, _)| s == p)
                    .flat_map(|&(x, _, y)| {
                        out_q.get(&y).into_iter().flatten().map(move |&z| (x, r, z))
                    })
                    .collect();
                for edge in new {
                    changed |= chase.add_edge(edge);
                }
                if chase.edges.len() > MAX_EDGES {
                    return None;
                }
            }
            let mut merged = false;
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            for (list, inverse) in [
                (&extras.functional, false),
                (&extras.inverse_functional, true),
            ] {
                for &p in list {
                    let mut by: HashMap<usize, usize> = HashMap::new();
                    for &(x, s, y) in &chase.edges {
                        if s == p {
                            let (key, value) = if inverse { (y, x) } else { (x, y) };
                            match by.get(&key) {
                                Some(&other) if other != value => pairs.push((other, value)),
                                Some(_) => {}
                                None => {
                                    by.insert(key, value);
                                }
                            }
                        }
                    }
                }
            }
            for (a, b) in pairs {
                merged |= chase.merge(a, b);
            }
            if merged {
                chase.canonical();
                changed = true;
            }
            let reps: Vec<usize> = (0..elements).filter(|&x| chase.rep[x] == x).collect();
            for &(left, class) in &tbox.inclusions {
                for &x in &reps {
                    if !chase.types.contains(&(x, class)) && chase.holds(x, left) {
                        chase.types.insert((x, class));
                        changed = true;
                    }
                }
            }
            for &(sub, sup) in &tbox.roles {
                let matching: Vec<(usize, usize)> = chase
                    .edges
                    .iter()
                    .filter(|&&(_, p, _)| p == sub.property)
                    .map(|&(s, _, o)| if sub.inverse { (o, s) } else { (s, o) })
                    .collect();
                for (x, y) in matching {
                    let edge = if sup.inverse {
                        (y, sup.property, x)
                    } else {
                        (x, sup.property, y)
                    };
                    changed |= chase.add_edge(edge);
                }
            }
            for (g, &(left, role, filler)) in tbox.generating.iter().enumerate() {
                for &x in &reps {
                    if chase.depth[x] < max_depth
                        && !generated.contains(&(x, g))
                        && chase.holds(x, left)
                    {
                        generated.insert((x, g));
                        let n = chase.depth.len();
                        chase.depth.push(chase.depth[x] + 1);
                        chase.rep.push(n);
                        chase.add_edge(if role.inverse {
                            (n, role.property, x)
                        } else {
                            (x, role.property, n)
                        });
                        if let Some(c) = filler {
                            chase.types.insert((n, c));
                        }
                        changed = true;
                    }
                }
            }
            if chase.depth.len() > max_elements || chase.edges.len() > MAX_EDGES {
                return None;
            }
            if !changed {
                return Some(chase);
            }
        }
    }

    /// The query's answers on the individuals (empty tuple for a boolean query that holds).
    /// Each connected part of the query is matched on its own (its answer variables on
    /// individuals only; a part without them until its first match), and the parts'
    /// answers combined.
    fn answers(&self, query: &Query) -> BTreeSet<Vec<usize>> {
        let mut by_property: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
        for &(x, p, y) in &self.edges {
            by_property.entry(p).or_default().push((x, y));
        }
        let mut combined: Vec<HashMap<usize, usize>> = vec![HashMap::new()];
        for part in parts(query) {
            let answers: Vec<usize> = query
                .answers
                .iter()
                .copied()
                .filter(|v| part.iter().any(|a| atom_vars(a).contains(v)))
                .collect();
            let sub = Query {
                atoms: part,
                vars: query.vars,
                answers,
            };
            let mut found = BTreeSet::new();
            let mut assignment = vec![None; query.vars];
            self.matches(&sub, &by_property, 0, &mut assignment, &mut found);
            let names = &sub.answers;
            combined = combined
                .into_iter()
                .flat_map(|row| {
                    found.iter().map(move |tuple| {
                        let mut row = row.clone();
                        row.extend(names.iter().copied().zip(tuple.iter().copied()));
                        row
                    })
                })
                .collect();
        }
        combined
            .into_iter()
            .map(|row| query.answers.iter().map(|v| row[v]).collect())
            .collect()
    }

    /// Matches the atoms from `at` on; true once a part without answer variables matched.
    fn matches(
        &self,
        query: &Query,
        by_property: &HashMap<usize, Vec<(usize, usize)>>,
        at: usize,
        assignment: &mut Vec<Option<usize>>,
        out: &mut BTreeSet<Vec<usize>>,
    ) -> bool {
        if at == query.atoms.len() {
            out.insert(
                query
                    .answers
                    .iter()
                    .map(|&v| assignment[v].expect("answer variables are bound"))
                    .collect(),
            );
            return query.answers.is_empty();
        }
        let value = |t: T, assignment: &[Option<usize>]| match t {
            T::Ind(i) => Some(self.find(i)),
            T::Var(v) => assignment[v],
        };
        let answer = |v: usize| query.answers.contains(&v);
        let bind = |t: T, x: usize, assignment: &mut Vec<Option<usize>>| -> Option<Option<usize>> {
            match t {
                T::Ind(i) => (self.find(i) == x).then_some(None),
                T::Var(v) => match assignment[v] {
                    Some(y) => (y == x).then_some(None),
                    None if answer(v) && x >= INDIVIDUALS => None,
                    None => {
                        assignment[v] = Some(x);
                        Some(Some(v))
                    }
                },
            }
        };
        match query.atoms[at] {
            QAtom::Class(t, c) => {
                let candidates: Vec<usize> = match value(t, assignment) {
                    Some(x) => vec![x],
                    None => (0..self.depth.len()).collect(),
                };
                for x in candidates {
                    if self.types.contains(&(x, c))
                        && let Some(bound) = bind(t, x, assignment)
                    {
                        let done = self.matches(query, by_property, at + 1, assignment, out);
                        if let Some(v) = bound {
                            assignment[v] = None;
                        }
                        if done {
                            return true;
                        }
                    }
                }
            }
            QAtom::Role(s, p, o) => {
                for &(x, y) in by_property.get(&p).into_iter().flatten() {
                    let Some(first) = bind(s, x, assignment) else {
                        continue;
                    };
                    let mut done = false;
                    if let Some(second) = bind(o, y, assignment) {
                        done = self.matches(query, by_property, at + 1, assignment, out);
                        if let Some(v) = second {
                            assignment[v] = None;
                        }
                    }
                    if let Some(v) = first {
                        assignment[v] = None;
                    }
                    if done {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn atom_vars(atom: &QAtom) -> Vec<usize> {
    let terms = match atom {
        QAtom::Class(t, _) => vec![*t],
        QAtom::Role(s, _, o) => vec![*s, *o],
    };
    terms
        .into_iter()
        .filter_map(|t| match t {
            T::Var(v) => Some(v),
            T::Ind(_) => None,
        })
        .collect()
}

/// The query's atoms in parts connected through variables, each in an order where every
/// atom after the first shares a variable with one before it.
fn parts(query: &Query) -> Vec<Vec<QAtom>> {
    let mut left: Vec<QAtom> = query.atoms.clone();
    let mut out = Vec::new();
    while !left.is_empty() {
        let mut part = vec![left.remove(0)];
        let mut vars: HashSet<usize> = atom_vars(&part[0]).into_iter().collect();
        while let Some(at) = left
            .iter()
            .position(|a| atom_vars(a).iter().any(|v| vars.contains(v)))
        {
            let atom = left.remove(at);
            vars.extend(atom_vars(&atom));
            part.push(atom);
        }
        out.push(part);
    }
    out
}

/// The rows of a query's TSV results, as tuples of individuals (`k` for the boolean form).
fn store_rows(store: &StoreService, query: &str) -> Vec<Vec<usize>> {
    store_answer(store, query).0
}

/// [`store_rows`], and whether the store says they are complete (with its reasons).
fn store_answer(store: &StoreService, query: &str) -> (Vec<Vec<usize>>, Completeness) {
    let mut request = SparqlQueryRequest::all(query);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let result = store
        .execute_query(&request)
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    let completeness = result
        .ql
        .map(|report| report.completeness)
        .unwrap_or_default();
    let text = String::from_utf8(result.payload).unwrap();
    let rows = text
        .lines()
        .skip(1)
        .map(|line| {
            line.split('\t')
                .filter(|cell| cell.starts_with('<'))
                .map(|cell| {
                    let name = cell.trim_matches(['<', '>']).trim_start_matches(E);
                    name.trim_start_matches('a').parse().unwrap_or_else(|_| {
                        panic!("{query}: an answer that isn't an individual: {cell}")
                    })
                })
                .collect()
        })
        .collect();
    (rows, completeness)
}

fn store(turtle: &str, dir: &std::path::Path, ruleset: Ruleset, ql: bool) -> StoreService {
    let file = dir.join("case.ttl");
    std::fs::write(&file, turtle).unwrap();
    let store = StoreService::new(StoreConfig {
        ql_rewriting: ql,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })
        .unwrap();
    store.rematerialise(ruleset).unwrap();
    store
}

#[test]
fn rewritten_answers_are_the_certain_answers_of_random_ql_cases() {
    let cases: usize = std::env::var("NRESE_QL_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(150);
    let dir = tempfile::tempdir().unwrap();
    let seed = std::env::var("NRESE_QL_SEED")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut rng = Rng(seed);
    let (mut checked, mut skipped, mut gained) = (0, 0, 0);
    for case in 0..cases {
        let tbox = random_tbox(&mut rng);
        let mut types = Vec::new();
        let mut edges = Vec::new();
        let mut data = Vec::new();
        for _ in 0..1 + rng.below(5) {
            if rng.chance(50) {
                let (a, c) = (rng.below(INDIVIDUALS), rng.below(CLASSES));
                types.push((a, c));
                data.push(format!(":a{a} a :C{c} ."));
            } else {
                let (a, p, b) = (
                    rng.below(INDIVIDUALS),
                    rng.below(PROPERTIES),
                    rng.below(INDIVIDUALS),
                );
                edges.push((a, p, b));
                data.push(format!(":a{a} :P{p} :a{b} ."));
            }
        }
        let queries: Vec<Query> = (0..4).map(|_| random_query(&mut rng)).collect();
        let kinds: HashSet<(Role, Option<usize>)> =
            tbox.generating.iter().map(|&(_, r, f)| (r, f)).collect();
        let max_vars = queries.iter().map(|q| q.vars).max().unwrap_or(1);
        let Some(chase) = Chase::run(
            &tbox,
            &Extras::default(),
            &types,
            &edges,
            kinds.len() + max_vars + 1,
            20_000,
        ) else {
            skipped += 1;
            continue;
        };
        let ruleset = if case % 2 == 0 {
            Ruleset::Owl2Ql
        } else {
            Ruleset::Owl2Rl
        };
        let turtle = format!(
            "@prefix : <{E}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             {}\n{}",
            tbox.turtle.join("\n"),
            data.join("\n")
        );
        let with = store(&turtle, dir.path(), ruleset, true);
        let without = store(&turtle, dir.path(), ruleset, false);
        for query in &queries {
            let certain = chase.answers(query);
            let (rows, completeness) = store_answer(&with, &query.sparql(true));
            let distinct: BTreeSet<Vec<usize>> = rows.into_iter().collect();
            // Pure QL: nothing the rewriting doesn't follow.
            assert!(
                completeness.complete,
                "case {case}: {completeness:?}\n{turtle}\n{}",
                query.sparql(true)
            );
            assert_eq!(
                distinct,
                certain,
                "case {case} ({}):\n{turtle}\n{}",
                ruleset.name(),
                query.sparql(true)
            );
            // Bags: the materialised rows, and each further answer once.
            let bag = store_rows(&with, &query.sparql(false));
            let plain = store_rows(&without, &query.sparql(false));
            let plain_set: BTreeSet<Vec<usize>> = plain.iter().cloned().collect();
            let added = certain.difference(&plain_set).count();
            let mut expected = plain.clone();
            expected.extend(certain.difference(&plain_set).cloned());
            let mut bag_sorted = bag.clone();
            bag_sorted.sort();
            expected.sort();
            if query.answers.is_empty() {
                // A boolean query: one row, materialised or not.
                assert_eq!(
                    bag_sorted.is_empty(),
                    certain.is_empty(),
                    "case {case}: {}",
                    query.sparql(false)
                );
            } else {
                assert_eq!(
                    bag_sorted,
                    expected,
                    "case {case} ({}):\n{turtle}\n{}",
                    ruleset.name(),
                    query.sparql(false)
                );
            }
            gained += added;
            checked += 1;
        }
    }
    eprintln!(
        "QL differential: {checked} queries checked, {gained} answers only through the rewriting, {skipped} cases skipped (chase too large)"
    );
    assert!(checked >= cases * 3, "too many cases skipped: {skipped}");
    assert!(gained > 0, "no case exercised the rewriting");
}

#[test]
fn mixed_ontologies_answer_soundly_and_say_when_they_may_miss() {
    let cases: usize = std::env::var("NRESE_QL_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(150);
    let dir = tempfile::tempdir().unwrap();
    let seed = std::env::var("NRESE_QL_SEED")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0x2545_f491_4f6c_dd1d);
    let mut rng = Rng(seed);
    let (mut checked, mut skipped) = (0, 0);
    // Queries that missed answers (all flagged), flagged ones that missed none, complete ones.
    let (mut missed, mut cautious, mut complete) = (0, 0, 0);
    // The misses by the hazards their reasons name.
    let mut by_hazard: std::collections::BTreeMap<&'static str, usize> = Default::default();
    for case in 0..cases {
        let tbox = random_tbox(&mut rng);
        let roles: Vec<usize> = tbox.generating.iter().map(|g| g.1.property).collect();
        let extras = random_extras(&mut rng, &roles);
        let mut types = Vec::new();
        let mut edges = Vec::new();
        let mut data = Vec::new();
        for _ in 0..1 + rng.below(8) {
            if rng.chance(50) {
                let (a, c) = (rng.below(INDIVIDUALS), rng.below(CLASSES));
                types.push((a, c));
                data.push(format!(":a{a} a :C{c} ."));
            } else {
                let (a, p, b) = (
                    rng.below(INDIVIDUALS),
                    rng.below(PROPERTIES),
                    rng.below(INDIVIDUALS),
                );
                edges.push((a, p, b));
                data.push(format!(":a{a} :P{p} :a{b} ."));
            }
        }
        let queries: Vec<Query> = (0..4).map(|_| random_query(&mut rng)).collect();
        let kinds: HashSet<(Role, Option<usize>)> =
            tbox.generating.iter().map(|&(_, r, f)| (r, f)).collect();
        let max_vars = queries.iter().map(|q| q.vars).max().unwrap_or(1);
        let Some(chase) = Chase::run(
            &tbox,
            &extras,
            &types,
            &edges,
            kinds.len() + max_vars + 2,
            5_000,
        ) else {
            skipped += 1;
            continue;
        };
        let turtle = format!(
            "@prefix : <{E}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             {}\n{}\n{}",
            tbox.turtle.join("\n"),
            extras.turtle.join("\n"),
            data.join("\n")
        );
        let store = store(&turtle, dir.path(), Ruleset::Owl2Rl, true);
        for query in &queries {
            let certain = chase.named_answers(query);
            let (rows, completeness) = store_answer(&store, &query.sparql(true));
            let answered: BTreeSet<Vec<usize>> = rows.into_iter().collect();
            let context = || {
                format!(
                    "case {case}:\n{turtle}\n{}\n{completeness:?}",
                    query.sparql(true)
                )
            };
            // Sound: every answer is certain.
            let wrong: Vec<_> = answered.difference(&certain).collect();
            assert!(
                wrong.is_empty(),
                "answers that aren't certain {wrong:?}: {}",
                context()
            );
            // Never silent: a miss says so.
            let misses = certain.difference(&answered).count();
            match (misses, completeness.complete) {
                (0, true) => complete += 1,
                (0, false) => cautious += 1,
                (_, false) => {
                    missed += 1;
                    for kind in hazard_kinds(&completeness) {
                        *by_hazard.entry(kind).or_default() += 1;
                    }
                }
                (_, true) => {
                    panic!("{misses} answers missed without saying so: {}", context())
                }
            }
            checked += 1;
        }
    }
    eprintln!(
        "QL mixed: {checked} queries checked, {skipped} cases skipped (chase too large); \
         {missed} missed answers and said sound-only, {cautious} said sound-only and missed \
         none, {complete} complete; the misses by hazard: {by_hazard:?}"
    );
    assert!(checked >= cases * 3, "too many cases skipped: {skipped}");
    assert!(
        missed > 0,
        "no case missed an answer: the flag went untested"
    );
}

/// The kinds of hazard a status's reasons name.
fn hazard_kinds(completeness: &Completeness) -> Vec<&'static str> {
    let kinds = [
        ("is transitive", "transitive"),
        ("property chain", "chain"),
        ("functional", "functional"),
        ("on the left", "left"),
        ("bound", "bound"),
    ];
    kinds
        .iter()
        .filter(|(text, _)| completeness.reasons.iter().any(|r| r.text.contains(text)))
        .map(|&(_, kind)| kind)
        .collect()
}

/// The research review's case: existentials feeding a transitive role. `Engine ⊑
/// ∃partOf.Car`, `Car ⊑ ∃partOf.Fleet`, `partOf` transitive, `e1 a Engine`; `SELECT ?x
/// { ?x partOf ?f . ?f a Fleet }` has the certain answer `e1` (through two anonymous
/// individuals and the transitive shortcut), which neither the RL closure nor the
/// tree-witness rewriting over it finds. The answer must say `sound-only`, naming the
/// transitive role; a miss without the flag fails. Here `C0`, `C1`, `C2` are Engine, Car
/// and Fleet, `P0` is partOf, `a0` is e1.
#[test]
fn existentials_feeding_a_transitive_role_miss_with_the_flag() {
    let (engine, car, fleet, part_of) = (0, 1, 2, 0);
    let role = Role {
        property: part_of,
        inverse: false,
    };
    let tbox = Tbox {
        generating: vec![
            (Basic::Class(engine), role, Some(car)),
            (Basic::Class(car), role, Some(fleet)),
        ],
        turtle: vec![
            format!(
                ":C{engine} rdfs:subClassOf {} .",
                restriction(role, Some(car))
            ),
            format!(
                ":C{car} rdfs:subClassOf {} .",
                restriction(role, Some(fleet))
            ),
        ],
        ..Tbox::default()
    };
    let extras = Extras {
        chains: vec![(part_of, part_of, part_of)],
        turtle: vec![format!(":P{part_of} a owl:TransitiveProperty .")],
        ..Extras::default()
    };
    let query = Query {
        atoms: vec![
            QAtom::Role(T::Var(0), part_of, T::Var(1)),
            QAtom::Class(T::Var(1), fleet),
        ],
        vars: 2,
        answers: vec![0],
    };
    let chase = Chase::run(&tbox, &extras, &[(0, engine)], &[], 6, 1_000).expect("a small chase");
    let certain = chase.named_answers(&query);
    assert_eq!(certain, BTreeSet::from([vec![0]]), "e1 is a certain answer");
    let turtle = format!(
        "@prefix : <{E}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         {}\n{}\n:a0 a :C{engine} .",
        tbox.turtle.join("\n"),
        extras.turtle.join("\n")
    );
    let dir = tempfile::tempdir().unwrap();
    for ruleset in [Ruleset::Owl2Rl, Ruleset::Owl2Ql] {
        let store = store(&turtle, dir.path(), ruleset, true);
        let (rows, completeness) = store_answer(&store, &query.sparql(true));
        let answered: BTreeSet<Vec<usize>> = rows.into_iter().collect();
        assert!(
            answered.is_subset(&certain),
            "{}: {answered:?}",
            ruleset.name()
        );
        if answered != certain {
            assert!(
                !completeness.complete,
                "{}: e1 missed without saying so",
                ruleset.name()
            );
        }
        // Today it is missed, and said so, naming the transitive role.
        assert!(answered.is_empty(), "{}: {answered:?}", ruleset.name());
        assert_eq!(completeness.as_str(), "sound-only", "{}", ruleset.name());
        assert!(
            completeness
                .reasons
                .iter()
                .any(|r| r.source == "ql" && r.text.contains("P0> is transitive")),
            "{}: {completeness:?}",
            ruleset.name()
        );
    }
}
