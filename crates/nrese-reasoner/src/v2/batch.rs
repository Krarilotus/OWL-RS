//! The batch executor (reasoner-v2 design §4.1): full materialisation by semi-naive
//! evaluation over a vertically partitioned working set.
//!
//! - **Working set.** One [`Relation`] per predicate: its pairs sorted by subject and by
//!   object, plus the last round's delta in both orders.
//! - **Schema grounding** (design §3.2). Atoms over the TBox vocabulary ([`Schema`]) are
//!   evaluated against the current facts and substituted into the rest of the rule. So
//!   `cax-sco` becomes one `(?x type C1) -> (?x type C2)` rule per subclass edge, and
//!   almost every atom gets a constant predicate. Grounding is redone whenever a round
//!   derives schema facts, and the rules it adds are evaluated once over all facts. That
//!   keeps specialisation exact when instance rules feed the schema (punning, class
//!   `sameAs`).
//! - **Semi-naive evaluation.** A rule with n atoms runs n variants per round: atom i over
//!   the delta, atoms before it over the old facts and atoms after it over all facts.
//!   Each variant is driven by its delta atom's matches, split into morsels that run in
//!   parallel; the other atoms are index lookups, ordered by how bound they are.
//! - **Deduplication** is a sort per round, then a merge into each relation. There's no
//!   shared hash set, and the result doesn't depend on the thread count.
//!
//! The naive evaluator ([`super::naive`]) is the oracle: both compute the same closure.

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

use super::ir::{Atom, Guard, Head, OWL, RDF, RDFS, Rule, Term, Vocabulary};
use super::lists::{Facts, ListVocabulary, instantiate};
use super::naive::{Triple, Violation};

type Pair = (u64, u64);

/// Driver matches per parallel work unit.
const MORSEL: usize = 4096;

/// The TBox vocabulary. Atoms over these predicates, and `rdf:type` atoms with these
/// classes as object, are grounded before evaluation.
pub struct Schema {
    rdf_type: u64,
    predicates: HashSet<u64>,
    classes: HashSet<u64>,
}

impl Schema {
    /// The RDFS and OWL 2 schema vocabulary. It includes the list vocabulary, so a change
    /// to a list axiom re-instantiates the list rules.
    pub fn owl(vocabulary: &mut impl Vocabulary) -> Self {
        let rdf_type = vocabulary.iri(&format!("{RDF}type"));
        let mut predicates = HashSet::new();
        for local in ["subClassOf", "subPropertyOf", "domain", "range"] {
            predicates.insert(vocabulary.iri(&format!("{RDFS}{local}")));
        }
        for local in ["first", "rest"] {
            predicates.insert(vocabulary.iri(&format!("{RDF}{local}")));
        }
        for local in [
            "equivalentClass",
            "equivalentProperty",
            "inverseOf",
            "onProperty",
            "onClass",
            "someValuesFrom",
            "allValuesFrom",
            "hasValue",
            "maxCardinality",
            "maxQualifiedCardinality",
            "disjointWith",
            "complementOf",
            "propertyDisjointWith",
            "sourceIndividual",
            "assertionProperty",
            "targetIndividual",
            "targetValue",
            "intersectionOf",
            "unionOf",
            "oneOf",
            "hasKey",
            "propertyChainAxiom",
            "members",
            "distinctMembers",
        ] {
            predicates.insert(vocabulary.iri(&format!("{OWL}{local}")));
        }
        let classes = [
            "Class",
            "ObjectProperty",
            "DatatypeProperty",
            "TransitiveProperty",
            "SymmetricProperty",
            "AsymmetricProperty",
            "FunctionalProperty",
            "InverseFunctionalProperty",
            "IrreflexiveProperty",
            "AllDifferent",
            "AllDisjointClasses",
            "AllDisjointProperties",
        ]
        .iter()
        .map(|local| vocabulary.iri(&format!("{OWL}{local}")))
        .collect();
        Self {
            rdf_type,
            predicates,
            classes,
        }
    }

    fn is_schema_atom(&self, atom: &Atom) -> bool {
        match atom.0 {
            [_, Term::Const(p), _] if self.predicates.contains(&p) => true,
            [_, Term::Const(p), Term::Const(c)] => p == self.rdf_type && self.classes.contains(&c),
            _ => false,
        }
    }

    fn is_schema_fact(&self, [_, p, o]: Triple) -> bool {
        self.predicates.contains(&p) || (p == self.rdf_type && self.classes.contains(&o))
    }
}

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates.
    pub derived: Vec<Triple>,
    /// Consistency violations on the closure, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<String>,
    pub rounds: usize,
    /// Rules after grounding, at the end.
    pub ground_rules: usize,
    /// Predicates closed by the transitive module.
    pub transitive: usize,
    /// Time per phase: grounding, rule joins, modules, merging, consistency.
    pub phases: Phases,
}

/// Time spent per phase of [`materialise`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Phases {
    pub load: std::time::Duration,
    pub grounding: std::time::Duration,
    pub joins: std::time::Duration,
    pub modules: std::time::Duration,
    pub merge: std::time::Duration,
    pub consistency: std::time::Duration,
}

/// Which facts an atom reads in a semi-naive variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    /// Facts known before the last round.
    Old,
    /// Facts the last round derived.
    Delta,
    All,
}

/// The pairs of one predicate.
#[derive(Default)]
struct Relation {
    so: Vec<Pair>,
    /// `(object, subject)` pairs.
    os: Vec<Pair>,
    delta_so: Vec<Pair>,
    delta_os: Vec<Pair>,
}

/// The pairs whose first component is `key`.
fn range(pairs: &[Pair], key: u64) -> &[Pair] {
    let start = pairs.partition_point(|p| p.0 < key);
    let len = pairs[start..].partition_point(|p| p.0 == key);
    &pairs[start..start + len]
}

/// Merges two sorted, disjoint pair lists.
fn merge(a: &[Pair], b: &[Pair]) -> Vec<Pair> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

impl Relation {
    fn contains(&self, s: u64, o: u64) -> bool {
        self.so.binary_search(&(s, o)).is_ok()
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`.
    fn scan(&self, s: Option<u64>, o: Option<u64>, seg: Seg, f: &mut dyn FnMut(u64, u64)) {
        let (so, os) = match seg {
            Seg::Delta => (&self.delta_so, &self.delta_os),
            Seg::Old | Seg::All => (&self.so, &self.os),
        };
        let old = seg == Seg::Old && !self.delta_so.is_empty();
        let keep = |s: u64, o: u64| !old || self.delta_so.binary_search(&(s, o)).is_err();
        match (s, o) {
            (Some(s), Some(o)) => {
                if so.binary_search(&(s, o)).is_ok() && keep(s, o) {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for &(_, o) in range(so, s) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, Some(o)) => {
                for &(_, s) in range(os, o) {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, None) => {
                for &(s, o) in so {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
        }
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        let (so, os) = match seg {
            Seg::Delta => (&self.delta_so, &self.delta_os),
            Seg::Old | Seg::All => (&self.so, &self.os),
        };
        match (s, o) {
            (Some(_), Some(_)) => 1,
            (Some(s), None) => range(so, s).len(),
            (None, Some(o)) => range(os, o).len(),
            (None, None) => so.len(),
        }
    }

    /// Makes `new` (sorted, deduplicated, disjoint from the relation) the delta.
    fn advance(&mut self, new: Vec<Pair>) {
        let mut new_os: Vec<Pair> = new.iter().map(|&(s, o)| (o, s)).collect();
        new_os.sort_unstable();
        if !new.is_empty() {
            self.so = merge(&self.so, &new);
            self.os = merge(&self.os, &new_os);
        }
        self.delta_so = new;
        self.delta_os = new_os;
    }
}

/// The working set: one relation per predicate.
#[derive(Default)]
struct Store {
    relations: Vec<Relation>,
    /// The predicate of each relation.
    predicates: Vec<u64>,
    index: HashMap<u64, usize>,
}

impl Store {
    /// A store holding `input`, all of it as the delta.
    fn new(input: &[Triple]) -> Self {
        let mut store = Self::default();
        store.advance(input.to_vec());
        store
    }

    fn relation(&self, p: u64) -> Option<&Relation> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    fn contains(&self, [s, p, o]: Triple) -> bool {
        self.relation(p).is_some_and(|r| r.contains(s, o))
    }

    fn scan(&self, [s, p, o]: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        match p {
            Some(p) => {
                if let Some(relation) = self.relation(p) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
            None => {
                for (relation, &p) in self.relations.iter().zip(&self.predicates) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
        }
    }

    fn estimate(&self, [s, p, o]: [Option<u64>; 3], seg: Seg) -> usize {
        match p {
            Some(p) => self.relation(p).map_or(0, |r| r.estimate(s, o, seg)),
            None => self.relations.iter().map(|r| r.estimate(s, o, seg)).sum(),
        }
    }

    /// Adds `candidates` and makes the new ones the delta; returns the new ones.
    fn advance(&mut self, mut candidates: Vec<Triple>) -> Vec<Triple> {
        candidates.par_sort_unstable_by_key(|&[s, p, o]| (p, s, o));
        candidates.dedup();
        // Group by predicate, keeping only facts not already known.
        let mut groups: Vec<(usize, Vec<Pair>)> = Vec::new();
        for chunk in candidates.chunk_by(|a, b| a[1] == b[1]) {
            let p = chunk[0][1];
            let index = match self.index.get(&p) {
                Some(&i) => i,
                None => {
                    self.index.insert(p, self.relations.len());
                    self.relations.push(Relation::default());
                    self.predicates.push(p);
                    self.relations.len() - 1
                }
            };
            let relation = &self.relations[index];
            let pairs: Vec<Pair> = chunk
                .iter()
                .map(|&[s, _, o]| (s, o))
                .filter(|&(s, o)| !relation.contains(s, o))
                .collect();
            groups.push((index, pairs));
        }
        let mut news: Vec<Vec<Pair>> = self.relations.iter().map(|_| Vec::new()).collect();
        for (index, pairs) in groups {
            news[index] = pairs;
        }
        let mut delta = Vec::new();
        for (pairs, &p) in news.iter().zip(&self.predicates) {
            delta.extend(pairs.iter().map(|&(s, o)| [s, p, o]));
        }
        self.relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .for_each(|(relation, pairs)| relation.advance(pairs));
        delta
    }
}

impl Facts for Store {
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64> {
        self.relation(predicate)
            .map(|r| range(&r.so, subject).iter().map(|&(_, o)| o).collect())
            .unwrap_or_default()
    }

    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)> {
        self.relation(predicate)
            .map(|r| r.so.clone())
            .unwrap_or_default()
    }
}

fn value(term: Term, bindings: &[Option<u64>]) -> Option<u64> {
    match term {
        Term::Const(c) => Some(c),
        Term::Var(v) => bindings[usize::from(v)],
    }
}

/// The atom's pattern under `bindings` (`None` for free positions).
fn pattern(atom: &Atom, bindings: &[Option<u64>]) -> [Option<u64>; 3] {
    atom.0.map(|t| value(t, bindings))
}

/// The atom's constants (`None` for variables).
fn constants(atom: &Atom) -> [Option<u64>; 3] {
    atom.0.map(|t| match t {
        Term::Const(c) => Some(c),
        Term::Var(_) => None,
    })
}

/// Binds the atom's free variables to `fact`; `None` if a repeated variable disagrees.
/// Returns the variables it bound, as a bitmask over the atom's three positions.
fn bind(atom: &Atom, fact: Triple, bindings: &mut [Option<u64>]) -> Option<u8> {
    let mut newly = 0u8;
    for (position, term) in atom.0.iter().enumerate() {
        if let Term::Var(v) = *term {
            match bindings[usize::from(v)] {
                Some(existing) if existing != fact[position] => {
                    unbind(atom, newly, bindings);
                    return None;
                }
                Some(_) => {}
                None => {
                    bindings[usize::from(v)] = Some(fact[position]);
                    newly |= 1 << position;
                }
            }
        }
    }
    Some(newly)
}

fn unbind(atom: &Atom, newly: u8, bindings: &mut [Option<u64>]) {
    for (position, term) in atom.0.iter().enumerate() {
        if newly & (1 << position) != 0
            && let Term::Var(v) = *term
        {
            bindings[usize::from(v)] = None;
        }
    }
}

/// Guards whose terms are both bound; unbound ones are decided later.
fn guards_hold(guards: &[Guard], bindings: &[Option<u64>]) -> bool {
    guards.iter().all(|guard| match guard {
        Guard::NotEqual(a, b) => match (value(*a, bindings), value(*b, bindings)) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        },
    })
}

/// Evaluates `order[depth..]` of `body`, calling `emit` for each complete binding.
fn walk(
    store: &Store,
    body: &[Atom],
    guards: &[Guard],
    order: &[(usize, Seg)],
    depth: usize,
    bindings: &mut [Option<u64>],
    emit: &mut dyn FnMut(&[Option<u64>]),
) {
    let Some(&(index, seg)) = order.get(depth) else {
        if guards_hold(guards, bindings) {
            emit(bindings);
        }
        return;
    };
    let atom = &body[index];
    store.scan(pattern(atom, bindings), seg, &mut |fact| {
        if let Some(newly) = bind(atom, fact, bindings) {
            walk(store, body, guards, order, depth + 1, bindings, emit);
            unbind(atom, newly, bindings);
        }
    });
}

/// Orders the atoms after `first`: most bound positions first, then fewest matches.
fn plan(
    store: &Store,
    rule: &Rule,
    atoms: &[usize],
    first: usize,
    seg_of: impl Fn(usize) -> Seg,
) -> Vec<(usize, Seg)> {
    let mut bound = vec![false; rule.variables()];
    fn mark(atom: &Atom, bound: &mut [bool]) {
        for term in atom.0 {
            if let Term::Var(v) = term {
                bound[usize::from(v)] = true;
            }
        }
    }
    let mut order = vec![(first, seg_of(first))];
    mark(&rule.body[first], &mut bound);
    let mut rest: Vec<usize> = atoms.iter().copied().filter(|&i| i != first).collect();
    while !rest.is_empty() {
        let (k, _) = rest
            .iter()
            .enumerate()
            .max_by_key(|&(_, &i)| {
                let atom = &rule.body[i];
                let known = atom
                    .0
                    .iter()
                    .filter(|t| match t {
                        Term::Const(_) => true,
                        Term::Var(v) => bound[usize::from(*v)],
                    })
                    .count();
                let consts = constants(atom);
                (known, std::cmp::Reverse(store.estimate(consts, seg_of(i))))
            })
            .expect("rest is not empty");
        let i = rest.remove(k);
        mark(&rule.body[i], &mut bound);
        order.push((i, seg_of(i)));
    }
    order
}

fn substitute(term: Term, substitution: &[Option<u64>]) -> Term {
    match term {
        Term::Var(v) => substitution[usize::from(v)].map_or(term, Term::Const),
        constant => constant,
    }
}

fn substitute_atom(atom: &Atom, substitution: &[Option<u64>]) -> Atom {
    Atom(atom.0.map(|t| substitute(t, substitution)))
}

/// A rule after grounding its schema atoms.
struct Grounded {
    rule: Rule,
    /// The schema variables' values; the rest is `None`.
    substitution: Vec<Option<u64>>,
}

/// Grounds `rule`'s schema atoms against `store`: calls `emit` with every instance rule.
fn ground(store: &Store, schema: &Schema, rule: &Rule, emit: &mut dyn FnMut(Grounded)) {
    let variables = rule.variables();
    let (schema_atoms, instance_atoms): (Vec<usize>, Vec<usize>) =
        (0..rule.body.len()).partition(|&i| schema.is_schema_atom(&rule.body[i]));
    if schema_atoms.is_empty() {
        emit(Grounded {
            rule: rule.clone(),
            substitution: vec![None; variables],
        });
        return;
    }
    let first = *schema_atoms
        .iter()
        .min_by_key(|&&i| {
            let consts = constants(&rule.body[i]);
            store.estimate(consts, Seg::All)
        })
        .expect("not empty");
    let order = plan(store, rule, &schema_atoms, first, |_| Seg::All);
    let mut bindings = vec![None; variables];
    walk(
        store,
        &rule.body,
        &rule.guards,
        &order,
        0,
        &mut bindings,
        &mut |substitution| {
            let mut guards = Vec::new();
            for guard in &rule.guards {
                let Guard::NotEqual(a, b) = *guard;
                match (substitute(a, substitution), substitute(b, substitution)) {
                    (Term::Const(a), Term::Const(b)) if a == b => return,
                    (Term::Const(_), Term::Const(_)) => {}
                    (a, b) => guards.push(Guard::NotEqual(a, b)),
                }
            }
            let head = match &rule.head {
                Head::Facts(atoms) => Head::Facts(
                    atoms
                        .iter()
                        .map(|a| substitute_atom(a, substitution))
                        .collect(),
                ),
                Head::Inconsistent => Head::Inconsistent,
            };
            emit(Grounded {
                rule: Rule {
                    name: rule.name.clone(),
                    body: instance_atoms
                        .iter()
                        .map(|&i| substitute_atom(&rule.body[i], substitution))
                        .collect(),
                    guards,
                    head,
                },
                substitution: substitution.to_vec(),
            });
        },
    );
}

/// The facts a rule's head derives under `bindings`.
fn instantiate_head(atom: &Atom, bindings: &[Option<u64>]) -> Triple {
    atom.0
        .map(|t| value(t, bindings).expect("safe rules bind head variables"))
}

/// One variant of one rule: the atom order (driver first) and the driver's matches.
struct Job<'r> {
    rule: &'r Rule,
    order: Vec<(usize, Seg)>,
    drivers: Vec<Triple>,
}

impl<'r> Job<'r> {
    /// The variant driven by `first` (read from `seg_of(first)`), or `None` if it can't match.
    fn new(
        store: &Store,
        rule: &'r Rule,
        first: usize,
        seg_of: impl Fn(usize) -> Seg,
    ) -> Option<Self> {
        // An atom over a predicate without facts can't match.
        for (i, atom) in rule.body.iter().enumerate() {
            if let Term::Const(p) = atom.0[1] {
                let empty = store.relation(p).is_none_or(|r| match seg_of(i) {
                    Seg::Delta => r.delta_so.is_empty(),
                    Seg::Old | Seg::All => r.so.is_empty(),
                });
                if empty {
                    return None;
                }
            }
        }
        let atoms: Vec<usize> = (0..rule.body.len()).collect();
        let order = plan(store, rule, &atoms, first, &seg_of);
        let mut drivers = Vec::new();
        let atom = &rule.body[first];
        let consts = constants(atom);
        store.scan(consts, seg_of(first), &mut |fact| drivers.push(fact));
        (!drivers.is_empty()).then_some(Self {
            rule,
            order,
            drivers,
        })
    }

    /// Runs the variant for `drivers[range]`, calling `emit` with each complete binding.
    fn run(
        &self,
        store: &Store,
        range: std::ops::Range<usize>,
        emit: &mut dyn FnMut(&[Option<u64>]),
    ) {
        let mut bindings = vec![None; self.rule.variables()];
        let driver = &self.rule.body[self.order[0].0];
        for &fact in &self.drivers[range] {
            if let Some(newly) = bind(driver, fact, &mut bindings) {
                walk(
                    store,
                    &self.rule.body,
                    &self.rule.guards,
                    &self.order,
                    1,
                    &mut bindings,
                    emit,
                );
                unbind(driver, newly, &mut bindings);
            }
        }
    }
}

/// Runs `jobs` in parallel morsels; returns the derived facts not yet in `store`.
fn run_jobs(store: &Store, jobs: &[Job<'_>]) -> Vec<Triple> {
    let tasks: Vec<(usize, std::ops::Range<usize>)> = jobs
        .iter()
        .enumerate()
        .flat_map(|(j, job)| {
            (0..job.drivers.len())
                .step_by(MORSEL)
                .map(move |start| (j, start..(start + MORSEL).min(job.drivers.len())))
        })
        .collect();
    tasks
        .par_iter()
        .map(|(j, range)| {
            let job = &jobs[*j];
            let Head::Facts(heads) = &job.rule.head else {
                return Vec::new();
            };
            let mut out = Vec::new();
            job.run(store, range.clone(), &mut |bindings| {
                for head in heads {
                    let fact = instantiate_head(head, bindings);
                    if !store.contains(fact) {
                        out.push(fact);
                    }
                }
            });
            out
        })
        .flatten()
        .collect()
}

/// The predicate `p` of a transitivity rule `(?x p ?y), (?y p ?z) -> (?x p ?z)`: `prp-trp`
/// after grounding, `scm-sco` and `scm-spo` before.
fn transitive_predicate(rule: &Rule) -> Option<u64> {
    let ([a, b], Head::Facts(head)) = (rule.body.as_slice(), &rule.head) else {
        return None;
    };
    let [Atom([hx, Term::Const(p), hz])] = head.as_slice() else {
        return None;
    };
    if !rule.guards.is_empty() {
        return None;
    }
    for (first, second) in [(a, b), (b, a)] {
        let (Atom([x, Term::Const(p1), y]), Atom([y2, Term::Const(p2), z])) = (first, second)
        else {
            continue;
        };
        let distinct = matches!((x, y, z), (Term::Var(x), Term::Var(y), Term::Var(z)) if x != y && y != z && x != z);
        if distinct && p1 == p && p2 == p && y == y2 && x == hx && z == hz {
            return Some(*p);
        }
    }
    None
}

/// The transitive module (design §4.4): for each transitive predicate, its closure by SCC
/// condensation instead of the transitivity rule's joins, which cost O(k³) on a k-clique.
/// It is recomputed in any round after other rules added facts over the predicate.
#[derive(Default)]
struct Transitive {
    /// Each predicate with whether it needs a recomputation.
    predicates: std::collections::BTreeMap<u64, bool>,
    /// New facts each predicate's last recomputation produced.
    produced: HashMap<u64, usize>,
}

impl Transitive {
    fn register(&mut self, p: u64) {
        self.predicates.entry(p).or_insert(true);
    }

    /// The closure facts of every predicate that needs it, not yet in `store`.
    fn run(&mut self, store: &Store) -> Vec<Triple> {
        self.produced.clear();
        let mut out = Vec::new();
        for (&p, dirty) in &mut self.predicates {
            if !std::mem::take(dirty) {
                continue;
            }
            let Some(relation) = store.relation(p) else {
                continue;
            };
            let before = out.len();
            out.extend(
                nrese_exec::graph::transitive_closure(&relation.so)
                    .into_iter()
                    .filter(|&(s, o)| !relation.contains(s, o))
                    .map(|(s, o)| [s, p, o]),
            );
            self.produced.insert(p, out.len() - before);
        }
        out
    }

    /// After a round: a predicate needs recomputing if other rules added facts over it.
    fn observe(&mut self, store: &Store) {
        for (p, dirty) in &mut self.predicates {
            let added = store.relation(*p).map_or(0, |r| r.delta_so.len());
            *dirty |= added > self.produced.get(p).copied().unwrap_or(0);
        }
    }
}

/// Identity of a ground rule, for deduplication (the name doesn't matter).
type RuleKey = (Vec<Atom>, Vec<Guard>, Head);

/// The closure of `input` under `rules` (plus the list rules when `lists` is given).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    let clock = std::time::Instant::now();
    let mut phases = Phases::default();
    let mut store = Store::new(input);
    phases.load = clock.elapsed();
    let mut result = Materialisation::default();
    let mut derived: Vec<Triple> = Vec::new();
    let mut program: Vec<Rule> = Vec::new();
    let mut known: HashSet<RuleKey> = HashSet::new();
    let mut regrounding = true;
    let mut transitive = Transitive::default();
    loop {
        result.rounds += 1;
        let mut candidates = Vec::new();
        let mut fresh = Vec::new();
        let clock = std::time::Instant::now();
        if regrounding {
            let mut source: Vec<Rule> = rules
                .iter()
                .filter(|r| r.head != Head::Inconsistent)
                .cloned()
                .collect();
            if let Some(vocabulary) = lists {
                let (list_rules, diagnostics) = instantiate(vocabulary, &store);
                source.extend(
                    list_rules
                        .into_iter()
                        .filter(|r| r.head != Head::Inconsistent),
                );
                result.diagnostics = diagnostics;
            }
            for rule in &source {
                if let Some(p) = transitive_predicate(rule) {
                    transitive.register(p);
                    continue;
                }
                ground(&store, schema, rule, &mut |grounded| {
                    let rule = grounded.rule;
                    if let Some(p) = transitive_predicate(&rule) {
                        transitive.register(p);
                    } else if rule.body.is_empty() {
                        let Head::Facts(heads) = &rule.head else {
                            return;
                        };
                        candidates.extend(heads.iter().map(|h| instantiate_head(h, &[])));
                    } else if known.insert((
                        rule.body.clone(),
                        rule.guards.clone(),
                        rule.head.clone(),
                    )) {
                        fresh.push(rule);
                    }
                });
            }
        }
        phases.grounding += clock.elapsed();
        let clock = std::time::Instant::now();
        // Semi-naive variants of the rules already evaluated, full evaluation of new ones.
        let mut jobs = Vec::new();
        for rule in &program {
            for i in 0..rule.body.len() {
                let seg_of = |j: usize| match j.cmp(&i) {
                    std::cmp::Ordering::Less => Seg::Old,
                    std::cmp::Ordering::Equal => Seg::Delta,
                    std::cmp::Ordering::Greater => Seg::All,
                };
                jobs.extend(Job::new(&store, rule, i, seg_of));
            }
        }
        for rule in &fresh {
            let first = (0..rule.body.len())
                .min_by_key(|&i| {
                    let consts = constants(&rule.body[i]);
                    store.estimate(consts, Seg::All)
                })
                .expect("fresh rules have a body");
            jobs.extend(Job::new(&store, rule, first, |_| Seg::All));
        }
        candidates.extend(run_jobs(&store, &jobs));
        drop(jobs);
        phases.joins += clock.elapsed();
        let clock = std::time::Instant::now();
        candidates.extend(transitive.run(&store));
        phases.modules += clock.elapsed();
        let clock = std::time::Instant::now();
        program.extend(fresh);
        let delta = store.advance(candidates);
        phases.merge += clock.elapsed();
        if delta.is_empty() {
            break;
        }
        transitive.observe(&store);
        regrounding = delta.iter().any(|&t| schema.is_schema_fact(t));
        derived.extend(delta);
    }
    result.ground_rules = program.len();
    result.transitive = transitive.predicates.len();
    let clock = std::time::Instant::now();
    result.violations = violations(&store, rules, lists, schema);
    phases.consistency = clock.elapsed();
    result.phases = phases;
    derived.par_sort_unstable();
    result.derived = derived;
    result
}

/// The consistency rules' violations on the closure in `store`.
fn violations(
    store: &Store,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Vec<Violation> {
    let mut source: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head == Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, _) = instantiate(vocabulary, store);
        source.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head == Head::Inconsistent),
        );
    }
    let mut found = HashSet::new();
    for rule in &source {
        let variables = rule.variables();
        ground(store, schema, rule, &mut |grounded| {
            let record = |bindings: &[Option<u64>], found: &mut HashSet<Violation>| {
                let bindings = (0..variables)
                    .map(|v| grounded.substitution[v].or(bindings[v]).unwrap_or(0))
                    .collect();
                found.insert(Violation {
                    rule: rule.name.clone(),
                    bindings,
                });
            };
            let ground_rule = &grounded.rule;
            if ground_rule.body.is_empty() {
                if guards_hold(&ground_rule.guards, &grounded.substitution) {
                    record(&vec![None; variables], &mut found);
                }
                return;
            }
            let first = (0..ground_rule.body.len())
                .min_by_key(|&i| store.estimate(constants(&ground_rule.body[i]), Seg::All))
                .expect("not empty");
            if let Some(job) = Job::new(store, ground_rule, first, |_| Seg::All) {
                job.run(store, 0..job.drivers.len(), &mut |bindings| {
                    record(bindings, &mut found)
                });
            }
        });
    }
    let mut violations: Vec<Violation> = found.into_iter().collect();
    violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    violations
}
