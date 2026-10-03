//! Rule evaluation shared by the batch executor ([`super::batch`]) and the delta executor
//! ([`super::delta`]). Everything here is generic over a [`Source`] of facts, which splits
//! them into the segments semi-naive evaluation needs.
//!
//! - [`Schema`] and [`ground`]: the schema atoms of a rule are evaluated against the facts
//!   and substituted into it (design §3.2). [`ground_delta`] grounds only through changed
//!   schema facts, so an edit to the TBox yields exactly the rule instances it adds or
//!   removes.
//! - [`Job`] and [`run_jobs`]: one variant of one rule, driven by its first atom's matches,
//!   run in parallel morsels; the other atoms are index lookups ordered by [`plan`].

use std::collections::HashMap;

use hashbrown::HashSet;
use rayon::prelude::*;

use super::ir::Triple;
use super::ir::{Atom, Guard, Head, OWL, RDF, RDFS, Rule, Term, Vocabulary};
use super::lists::Facts;

/// Driver matches per parallel work unit.
const MORSEL: usize = 4096;

/// Which facts an atom reads in a semi-naive variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    /// Facts known before the last round.
    Old,
    /// Facts the last round added.
    Delta,
    /// Both.
    All,
}

/// Facts, split into the last round's delta and the rest.
pub trait Source: Sync {
    /// Calls `f` with every fact of `seg` matching `pattern` (`None` = any term).
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple));
    /// An upper bound on the matches of `pattern` in `seg`, for planning. Zero must mean
    /// that nothing matches.
    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize;
    /// Whether `fact` is in [`Seg::All`].
    fn contains(&self, fact: Triple) -> bool;
}

/// A [`Source`]'s facts (all segments) for list instantiation.
pub struct AllFacts<'a, S: ?Sized>(pub &'a S);

impl<S: Source + ?Sized> Facts for AllFacts<'_, S> {
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64> {
        let mut out = Vec::new();
        self.0
            .scan([Some(subject), Some(predicate), None], Seg::All, &mut |t| {
                out.push(t[2])
            });
        out.sort_unstable();
        out.dedup();
        out
    }

    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        self.0
            .scan([None, Some(predicate), None], Seg::All, &mut |t| {
                out.push((t[0], t[2]))
            });
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The TBox vocabulary. Atoms over these predicates, and `rdf:type` atoms with these
/// classes as object, are grounded before evaluation.
#[derive(Clone)]
pub struct Schema {
    rdf_type: u64,
    thing: u64,
    /// Unnamed classes whose memberships aren't derived (work package W7,
    /// [`super::unnamed`]): each with whether a membership stands for `owl:Thing`.
    hidden: HashMap<u64, bool>,
    predicates: HashSet<u64>,
    classes: HashSet<u64>,
}

impl Schema {
    /// The id of `rdf:type`.
    pub fn rdf_type(&self) -> u64 {
        self.rdf_type
    }

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
            thing: vocabulary.iri(&format!("{OWL}Thing")),
            hidden: HashMap::new(),
            predicates,
            classes,
        }
    }

    /// This schema, with memberships in `hidden` classes not derived: a ground rule that
    /// would derive one derives `owl:Thing` instead where the value is `true`, else
    /// nothing ([`super::unnamed`]).
    #[must_use]
    pub fn hiding(mut self, hidden: HashMap<u64, bool>) -> Self {
        self.hidden = hidden;
        self
    }

    pub fn hidden(&self) -> &HashMap<u64, bool> {
        &self.hidden
    }

    /// `rule` with its heads' memberships in hidden classes rewritten; `None` if nothing
    /// is left to derive.
    fn rewrite_heads(&self, mut rule: Rule) -> Option<Rule> {
        if self.hidden.is_empty() {
            return Some(rule);
        }
        if let Head::Facts(atoms) = &mut rule.head {
            atoms.retain_mut(|atom| match atom.0 {
                [_, Term::Const(p), Term::Const(c)] if p == self.rdf_type => {
                    match self.hidden.get(&c) {
                        Some(true) => {
                            atom.0[2] = Term::Const(self.thing);
                            true
                        }
                        Some(false) => false,
                        None => true,
                    }
                }
                _ => true,
            });
            if atoms.is_empty() {
                return None;
            }
        }
        Some(rule)
    }

    pub fn is_schema_atom(&self, atom: &Atom) -> bool {
        match atom.0 {
            [_, Term::Const(p), _] if self.predicates.contains(&p) => true,
            [_, Term::Const(p), Term::Const(c)] => p == self.rdf_type && self.classes.contains(&c),
            _ => false,
        }
    }

    pub fn is_schema_fact(&self, [_, p, o]: Triple) -> bool {
        self.predicates.contains(&p) || (p == self.rdf_type && self.classes.contains(&o))
    }
}

pub fn value(term: Term, bindings: &[Option<u64>]) -> Option<u64> {
    match term {
        Term::Const(c) => Some(c),
        Term::Var(v) => bindings[usize::from(v)],
    }
}

/// The atom's pattern under `bindings` (`None` for free positions).
pub fn pattern(atom: &Atom, bindings: &[Option<u64>]) -> [Option<u64>; 3] {
    atom.0.map(|t| value(t, bindings))
}

/// The atom's constants (`None` for variables).
pub fn constants(atom: &Atom) -> [Option<u64>; 3] {
    atom.0.map(|t| match t {
        Term::Const(c) => Some(c),
        Term::Var(_) => None,
    })
}

/// Binds the atom's free variables to `fact`; `None` if a repeated variable disagrees.
/// Returns the variables it bound, as a bitmask over the atom's three positions.
pub fn bind(atom: &Atom, fact: Triple, bindings: &mut [Option<u64>]) -> Option<u8> {
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
        } else if let Term::Const(c) = *term
            && c != fact[position]
        {
            unbind(atom, newly, bindings);
            return None;
        }
    }
    Some(newly)
}

pub fn unbind(atom: &Atom, newly: u8, bindings: &mut [Option<u64>]) {
    for (position, term) in atom.0.iter().enumerate() {
        if newly & (1 << position) != 0
            && let Term::Var(v) = *term
        {
            bindings[usize::from(v)] = None;
        }
    }
}

/// Guards whose terms are both bound; unbound ones are decided later.
pub fn guards_hold(guards: &[Guard], bindings: &[Option<u64>]) -> bool {
    guards
        .iter()
        .all(|guard| guard.holds(|term| value(term, bindings)))
}

/// Evaluates `order[depth..]` of `body`, calling `emit` for each complete binding.
pub fn walk<S: Source + ?Sized>(
    source: &S,
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
    source.scan(pattern(atom, bindings), seg, &mut |fact| {
        if let Some(newly) = bind(atom, fact, bindings) {
            walk(source, body, guards, order, depth + 1, bindings, emit);
            unbind(atom, newly, bindings);
        }
    });
}

/// Orders `atoms` after `first`: most bound positions first, then fewest matches.
pub fn plan<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    atoms: &[usize],
    first: usize,
    seg_of: impl Fn(usize) -> Seg,
) -> Vec<(usize, Seg)> {
    plan_bound(
        source,
        rule,
        atoms,
        first,
        &vec![false; rule.variables()],
        seg_of,
    )
}

/// [`plan`], with the variables in `bound` bound beforehand.
fn plan_bound<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    atoms: &[usize],
    first: usize,
    bound: &[bool],
    seg_of: impl Fn(usize) -> Seg,
) -> Vec<(usize, Seg)> {
    fn mark(atom: &Atom, bound: &mut [bool]) {
        for term in atom.0 {
            if let Term::Var(v) = term {
                bound[usize::from(v)] = true;
            }
        }
    }
    let mut bound = bound.to_vec();
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
                (
                    known,
                    std::cmp::Reverse(source.estimate(constants(atom), seg_of(i))),
                )
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
#[derive(Clone)]
pub struct Grounded {
    pub rule: Rule,
    /// The schema variables' values; the rest is `None`.
    pub substitution: Vec<Option<u64>>,
}

/// The rule instance for one binding of the schema variables; `None` if a guard fails.
fn instance(
    rule: &Rule,
    instance_atoms: &[usize],
    substitution: &[Option<u64>],
) -> Option<Grounded> {
    let mut guards = Vec::new();
    for guard in &rule.guards {
        match *guard {
            Guard::NotEqual(a, b) => {
                match (substitute(a, substitution), substitute(b, substitution)) {
                    (Term::Const(a), Term::Const(b)) if a == b => return None,
                    (Term::Const(_), Term::Const(_)) => {}
                    (a, b) => guards.push(Guard::NotEqual(a, b)),
                }
            }
            Guard::NotIn(term, low, high) => match substitute(term, substitution) {
                Term::Const(v) if (low..=high).contains(&v) => return None,
                Term::Const(_) => {}
                term => guards.push(Guard::NotIn(term, low, high)),
            },
            Guard::SameList(a, b, ref index) => {
                match (substitute(a, substitution), substitute(b, substitution)) {
                    (Term::Const(a), Term::Const(b)) if a >= b || !index.together(a, b) => {
                        return None;
                    }
                    (Term::Const(_), Term::Const(_)) => {}
                    (a, b) => guards.push(Guard::SameList(a, b, index.clone())),
                }
            }
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
    Some(Grounded {
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
    })
}

fn split_schema(schema: &Schema, rule: &Rule) -> (Vec<usize>, Vec<usize>) {
    (0..rule.body.len()).partition(|&i| schema.is_schema_atom(&rule.body[i]))
}

/// Grounds `rule`'s schema atoms against all of `source`: calls `emit` with every
/// instance rule. A rule without schema atoms is emitted as it is.
pub fn ground<S: Source + ?Sized>(
    source: &S,
    schema: &Schema,
    rule: &Rule,
    emit: &mut dyn FnMut(Grounded),
) {
    let (schema_atoms, instance_atoms) = split_schema(schema, rule);
    if schema_atoms.is_empty() {
        emit(Grounded {
            rule: rule.clone(),
            substitution: vec![None; rule.variables()],
        });
        return;
    }
    let first = *schema_atoms
        .iter()
        .min_by_key(|&&i| source.estimate(constants(&rule.body[i]), Seg::All))
        .expect("not empty");
    let order = plan(source, rule, &schema_atoms, first, |_| Seg::All);
    let mut bindings = vec![None; rule.variables()];
    walk(
        source,
        &rule.body,
        &rule.guards,
        &order,
        0,
        &mut bindings,
        &mut |substitution| {
            if let Some(grounded) = instance(rule, &instance_atoms, substitution) {
                emit(grounded);
            }
        },
    );
}

/// Grounds `rule` only through the delta: the instances whose schema binding uses at least
/// one fact of [`Seg::Delta`] (the semi-naive variants of the schema atoms). Each such
/// instance is emitted at least once. Rules without schema atoms emit nothing.
pub fn ground_delta<S: Source + ?Sized>(
    source: &S,
    schema: &Schema,
    rule: &Rule,
    emit: &mut dyn FnMut(Grounded),
) {
    let (schema_atoms, instance_atoms) = split_schema(schema, rule);
    for (k, &first) in schema_atoms.iter().enumerate() {
        if source.estimate(constants(&rule.body[first]), Seg::Delta) == 0 {
            continue;
        }
        let seg_of = |i: usize| {
            let position = schema_atoms
                .iter()
                .position(|&a| a == i)
                .expect("a schema atom");
            match position.cmp(&k) {
                std::cmp::Ordering::Less => Seg::Old,
                std::cmp::Ordering::Equal => Seg::Delta,
                std::cmp::Ordering::Greater => Seg::All,
            }
        };
        let order = plan(source, rule, &schema_atoms, first, seg_of);
        let mut bindings = vec![None; rule.variables()];
        walk(
            source,
            &rule.body,
            &rule.guards,
            &order,
            0,
            &mut bindings,
            &mut |substitution| {
                if let Some(grounded) = instance(rule, &instance_atoms, substitution) {
                    emit(grounded);
                }
            },
        );
    }
}

/// The fact a head atom derives under `bindings`.
pub fn instantiate_head(atom: &Atom, bindings: &[Option<u64>]) -> Triple {
    atom.0
        .map(|t| value(t, bindings).expect("safe rules bind head variables"))
}

/// One variant of one rule: the atom order (driver first) and the driver's matches.
pub struct Job<'r> {
    pub rule: &'r Rule,
    order: Vec<(usize, Seg)>,
    drivers: Vec<Triple>,
}

impl<'r> Job<'r> {
    /// The variant driven by `first` (read from `seg_of(first)`), or `None` if it can't
    /// match.
    pub fn new<S: Source + ?Sized>(
        source: &S,
        rule: &'r Rule,
        first: usize,
        seg_of: impl Fn(usize) -> Seg,
    ) -> Option<Self> {
        // An atom over a predicate without facts can't match.
        for (i, atom) in rule.body.iter().enumerate() {
            if let Term::Const(p) = atom.0[1]
                && source.estimate([None, Some(p), None], seg_of(i)) == 0
            {
                return None;
            }
        }
        let atoms: Vec<usize> = (0..rule.body.len()).collect();
        let order = plan(source, rule, &atoms, first, &seg_of);
        let mut drivers = Vec::new();
        source.scan(constants(&rule.body[first]), seg_of(first), &mut |fact| {
            drivers.push(fact)
        });
        (!drivers.is_empty()).then_some(Self {
            rule,
            order,
            drivers,
        })
    }

    /// The semi-naive variant with atom `i` on the delta: atoms before it read the old
    /// facts, atoms after it all facts.
    pub fn variant<S: Source + ?Sized>(source: &S, rule: &'r Rule, i: usize) -> Option<Self> {
        Self::new(source, rule, i, |j| match j.cmp(&i) {
            std::cmp::Ordering::Less => Seg::Old,
            std::cmp::Ordering::Equal => Seg::Delta,
            std::cmp::Ordering::Greater => Seg::All,
        })
    }

    /// The full evaluation over all facts, driven by the most selective atom.
    pub fn full<S: Source + ?Sized>(source: &S, rule: &'r Rule) -> Option<Self> {
        let first = (0..rule.body.len())
            .min_by_key(|&i| source.estimate(constants(&rule.body[i]), Seg::All))
            .expect("a rule with a body");
        Self::new(source, rule, first, |_| Seg::All)
    }

    pub fn drivers(&self) -> usize {
        self.drivers.len()
    }

    /// Runs the variant for `drivers[range]`, calling `emit` with each complete binding.
    pub fn run<S: Source + ?Sized>(
        &self,
        source: &S,
        range: std::ops::Range<usize>,
        emit: &mut dyn FnMut(&[Option<u64>]),
    ) {
        let mut bindings = vec![None; self.rule.variables()];
        let driver = &self.rule.body[self.order[0].0];
        for &fact in &self.drivers[range] {
            if let Some(newly) = bind(driver, fact, &mut bindings) {
                walk(
                    source,
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

/// Runs `jobs` in parallel morsels; returns the facts their heads derive that `keep`
/// accepts (typically: not yet in the source).
/// Asked between units of work: `true` means stop (the caller gave up). Polling it is an
/// atomic load in practice, so it is checked per morsel.
pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);

fn never() -> bool {
    false
}

/// A [`Stop`] that never fires.
pub const NEVER: Stop<'static> = &never;

/// Every fact `jobs` derive that `keep` accepts. If `stop` fires, the remaining morsels
/// are skipped and the result is incomplete: the caller must check `stop` and discard it.
pub fn run_jobs<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: &(dyn Fn(Triple) -> bool + Sync),
    stop: Stop<'_>,
) -> Vec<Triple> {
    run_jobs_with(source, jobs, keep, false, stop)
}

/// [`run_jobs`] without circular derivations: those whose head is one of their own
/// premises, such as `(?x type C) -> (?x type C)` from `C subClassOf C`. A well-founded
/// proof never uses such a step, so overdeletion can skip them; it would otherwise
/// cascade through every reflexive instance.
pub fn run_jobs_acyclic<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: &(dyn Fn(Triple) -> bool + Sync),
    stop: Stop<'_>,
) -> Vec<Triple> {
    run_jobs_with(source, jobs, keep, true, stop)
}

fn run_jobs_with<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: &(dyn Fn(Triple) -> bool + Sync),
    acyclic: bool,
    stop: Stop<'_>,
) -> Vec<Triple> {
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
            if stop() {
                return Vec::new();
            }
            let mut out = Vec::new();
            job.run(source, range.clone(), &mut |bindings| {
                for head in heads {
                    let fact = instantiate_head(head, bindings);
                    let circular = acyclic
                        && job
                            .rule
                            .body
                            .iter()
                            .any(|atom| instantiate_head(atom, bindings) == fact);
                    if !circular && keep(fact) {
                        out.push(fact);
                    }
                }
            });
            out
        })
        .flatten()
        .collect()
}

/// Calls `emit` with every binding of `rule`'s body under which its head derives `fact`,
/// reading `seg` (a backward, one-step derivation check: DRed's rederivation, and
/// explanations later).
pub fn derivations<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    fact: Triple,
    seg: Seg,
    emit: &mut dyn FnMut(&[Option<u64>]) -> bool,
) {
    let Head::Facts(heads) = &rule.head else {
        return;
    };
    let mut bindings = vec![None; rule.variables()];
    for head in heads {
        let Some(newly) = bind(head, fact, &mut bindings) else {
            continue;
        };
        let mut stop = false;
        if rule.body.is_empty() {
            stop = guards_hold(&rule.guards, &bindings) && emit(&bindings);
        } else {
            let bound: Vec<bool> = bindings.iter().map(Option::is_some).collect();
            let atoms: Vec<usize> = (0..rule.body.len()).collect();
            // Start from the atom with the most bound positions, then the fewest matches.
            let first = (0..rule.body.len())
                .max_by_key(|&i| {
                    let bound = pattern(&rule.body[i], &bindings);
                    let known = bound.iter().filter(|p| p.is_some()).count();
                    (known, std::cmp::Reverse(source.estimate(bound, seg)))
                })
                .expect("a body");
            let order = plan_bound(source, rule, &atoms, first, &bound, |_| seg);
            walk(
                source,
                &rule.body,
                &rule.guards,
                &order,
                0,
                &mut bindings,
                &mut |b| {
                    if !stop {
                        stop = emit(b);
                    }
                },
            );
        }
        unbind(head, newly, &mut bindings);
        if stop {
            return;
        }
    }
}

/// The predicate `p` of a transitivity rule `(?x p ?y), (?y p ?z) -> (?x p ?z)`: `prp-trp`
/// after grounding, `scm-sco` and `scm-spo` before.
pub fn transitive_predicate(rule: &Rule) -> Option<u64> {
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

/// Identity of a ground rule, for deduplication (the name doesn't matter).
pub type RuleKey = (Vec<Atom>, Vec<Guard>, Head);

pub fn rule_key(rule: &Rule) -> RuleKey {
    (rule.body.clone(), rule.guards.clone(), rule.head.clone())
}

/// A (rule, atom) reference into a [`GroundProgram`].
type AtomRef = (u32, u8);

/// Ground rules' atoms indexed by what they can match, so a round only visits the rules
/// with an atom that can match a fact of the delta (the "dispatch tables" of design §3.2):
/// `(?x type C)` atoms by `(type, C)`, other constant-predicate atoms by predicate.
#[derive(Default, Clone)]
pub struct Dispatch {
    /// Constant predicate and object, by predicate then object.
    by_object: hashbrown::HashMap<u64, hashbrown::HashMap<u64, Vec<AtomRef>>>,
    /// Constant predicate, variable object.
    by_predicate: hashbrown::HashMap<u64, Vec<AtomRef>>,
    /// Variable predicate.
    any: Vec<AtomRef>,
}

impl Dispatch {
    fn push(&mut self, index: usize, rule: &Rule) {
        self.push_atoms(index, &rule.body);
    }

    fn push_atoms(&mut self, index: usize, atoms: &[Atom]) {
        let index = u32::try_from(index).expect("fewer than 2^32 ground rules");
        for (i, atom) in atoms.iter().enumerate() {
            let entry = (index, u8::try_from(i).expect("fewer than 256 atoms"));
            match atom.0 {
                [_, Term::Const(p), Term::Const(o)] => {
                    self.by_object
                        .entry(p)
                        .or_default()
                        .entry(o)
                        .or_default()
                        .push(entry);
                }
                [_, Term::Const(p), _] => self.by_predicate.entry(p).or_default().push(entry),
                _ => self.any.push(entry),
            }
        }
    }

    /// The (rule, atom) pairs whose atom can match `fact`.
    fn matching_fact(&self, [_, p, o]: Triple, out: &mut Vec<(usize, usize)>) {
        let entries = self
            .by_object
            .get(&p)
            .and_then(|objects| objects.get(&o))
            .into_iter()
            .flatten()
            .chain(self.by_predicate.get(&p).into_iter().flatten())
            .chain(&self.any);
        out.extend(entries.map(|&(r, a)| (r as usize, usize::from(a))));
    }

    /// The (rule, atom) pairs whose atom can match a fact of `delta`'s delta, sorted.
    pub(crate) fn matching(&self, delta: &super::batch::Store) -> Vec<(usize, usize)> {
        let mut out: Vec<AtomRef> = Vec::new();
        let mut any_delta = false;
        for (p, relation) in delta.delta_relations() {
            any_delta = true;
            if let Some(entries) = self.by_predicate.get(&p) {
                out.extend_from_slice(entries);
            }
            let Some(objects) = self.by_object.get(&p) else {
                continue;
            };
            // Probe whichever side is smaller: the indexed objects or the delta's.
            if objects.len() <= relation.delta_len() {
                for (&o, entries) in objects {
                    if relation.delta_has_object(o) {
                        out.extend_from_slice(entries);
                    }
                }
            } else {
                relation.delta_objects(&mut |o| {
                    if let Some(entries) = objects.get(&o) {
                        out.extend_from_slice(entries);
                    }
                });
            }
        }
        if any_delta {
            out.extend_from_slice(&self.any);
        }
        out.sort_unstable();
        out.dedup();
        out.into_iter()
            .map(|(r, a)| (r as usize, usize::from(a)))
            .collect()
    }
}

/// A ground program: the rule instances with a body (indexed by [`Dispatch`]), the
/// transitive predicates (closed by a module instead of joins), the facts of bodiless
/// instances, and the ground consistency rules.
#[derive(Default, Clone)]
pub struct GroundProgram {
    pub rules: Vec<Rule>,
    known: hashbrown::HashMap<RuleKey, usize>,
    dispatch: Dispatch,
    pub transitive: std::collections::BTreeSet<u64>,
    /// Facts of bodiless instances, not yet handed out ([`Self::take_facts`]).
    facts: Vec<Triple>,
    /// Facts of all bodiless instances.
    pub bodiless: HashSet<Triple>,
    /// The schema facts each rule instance was grounded on (parallel to `rules`), each
    /// bodiless fact's, and each transitive predicate's: a backward proof that uses an
    /// instance must prove them too.
    premises: Vec<Vec<Triple>>,
    bodiless_premises: hashbrown::HashMap<Triple, Vec<Triple>>,
    transitive_premises: hashbrown::HashMap<u64, Vec<Triple>>,
    /// Further sets of schema facts the same ground rule, bodiless fact or transitive
    /// predicate was grounded on (a proof needs one; support graph sets need all).
    other_premises: hashbrown::HashMap<Grounding, Vec<Vec<Triple>>>,
    /// Rules by head atom, for backward derivation checks.
    heads: Dispatch,
    /// Ground consistency rules, each with the rule it came from.
    pub consistency: Vec<(Rule, Grounded)>,
    consistency_known: HashSet<(String, Vec<Option<u64>>, RuleKey)>,
    consistency_dispatch: Dispatch,
    /// The list axioms of the program's state that weren't instantiated.
    pub list_diagnostics: Vec<super::lists::ListDiagnostic>,
}

impl std::fmt::Debug for GroundProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroundProgram")
            .field("rules", &self.rules.len())
            .field("transitive", &self.transitive.len())
            .field("bodiless", &self.bodiless.len())
            .field("consistency", &self.consistency.len())
            .finish()
    }
}

impl GroundProgram {
    /// Files one ground fact rule; returns its index if it is new and has a body.
    pub fn add(&mut self, rule: Rule) -> Option<usize> {
        self.add_with(rule, Vec::new())
    }

    /// [`Self::add`], with the schema facts the instance was grounded on.
    fn add_with(&mut self, rule: Rule, premises: Vec<Triple>) -> Option<usize> {
        if let Some(p) = transitive_predicate(&rule) {
            self.transitive_with(p, premises);
            None
        } else if rule.body.is_empty() {
            if let Head::Facts(heads) = &rule.head {
                for head in heads {
                    let fact = instantiate_head(head, &[]);
                    if self.bodiless.insert(fact) {
                        self.facts.push(fact);
                        self.bodiless_premises.insert(fact, premises.clone());
                    } else if self.bodiless_premises.get(&fact) != Some(&premises) {
                        self.other(Grounding::Bodiless(fact), premises.clone());
                    }
                }
            }
            None
        } else {
            let key = rule_key(&rule);
            if let Some(&r) = self.known.get(&key) {
                if self.premises[r] != premises {
                    self.other(Grounding::Rule(r), premises);
                }
                return None;
            }
            self.known.insert(key, self.rules.len());
            self.dispatch.push(self.rules.len(), &rule);
            if let Head::Facts(heads) = &rule.head {
                self.heads.push_atoms(self.rules.len(), heads);
            }
            self.rules.push(rule);
            self.premises.push(premises);
            Some(self.rules.len() - 1)
        }
    }

    /// Makes `p` transitive, grounded on `premises`.
    fn transitive_with(&mut self, p: u64, premises: Vec<Triple>) {
        self.transitive.insert(p);
        match self.transitive_premises.get(&p) {
            None => {
                self.transitive_premises.insert(p, premises);
            }
            Some(first) if *first != premises => {
                self.other(Grounding::Transitive(p), premises);
            }
            Some(_) => {}
        }
    }

    /// Records a further set of schema facts for `grounding` (once each).
    fn other(&mut self, grounding: Grounding, premises: Vec<Triple>) {
        let others = self.other_premises.entry(grounding).or_default();
        if !others.contains(&premises) {
            others.push(premises);
        }
    }

    /// Every set of schema facts `grounding` was grounded on (the first, then the others).
    pub fn premise_alternatives(&self, grounding: Grounding) -> Vec<&[Triple]> {
        let first: &[Triple] = match grounding {
            Grounding::Rule(r) => &self.premises[r],
            Grounding::Bodiless(fact) => self.bodiless_premises(fact),
            Grounding::Transitive(p) => self.transitive_premises(p),
        };
        std::iter::once(first)
            .chain(
                self.other_premises
                    .get(&grounding)
                    .into_iter()
                    .flatten()
                    .map(Vec::as_slice),
            )
            .collect()
    }

    /// The schema facts that made `p` transitive (empty if it isn't).
    pub fn transitive_premises(&self, p: u64) -> &[Triple] {
        self.transitive_premises.get(&p).map_or(&[], Vec::as_slice)
    }

    /// The schema facts a bodiless fact was grounded on (empty if none, or not bodiless).
    pub fn bodiless_premises(&self, fact: Triple) -> &[Triple] {
        self.bodiless_premises.get(&fact).map_or(&[], Vec::as_slice)
    }

    /// Files one ground consistency rule (from `source`); returns its index if new.
    fn add_consistency(&mut self, source: &Rule, grounded: Grounded) -> Option<usize> {
        let key = (
            source.name.clone(),
            grounded.substitution.clone(),
            rule_key(&grounded.rule),
        );
        if !self.consistency_known.insert(key) {
            return None;
        }
        self.consistency_dispatch
            .push(self.consistency.len(), &grounded.rule);
        self.consistency.push((source.clone(), grounded));
        Some(self.consistency.len() - 1)
    }

    /// Grounds `rules` (fact and consistency rules) over all of `source`.
    pub fn ground<S: Source + ?Sized>(&mut self, source: &S, schema: &Schema, rules: &[Rule]) {
        for rule in rules {
            self.ground_one(source, schema, rule, false, &[]);
        }
    }

    /// [`Self::ground`] for rules that come with premises of their own (list rules and the
    /// list facts they were instantiated from).
    pub fn ground_with_premises<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
        premises: &[Vec<Triple>],
    ) {
        for (rule, extra) in rules.iter().zip(premises) {
            self.ground_one(source, schema, rule, false, extra);
        }
    }

    /// Grounds `rules` through the delta of `source` only (rules without schema atoms, list
    /// rules among them, in full); returns the indices of the new fact rules and
    /// consistency rules.
    pub fn ground_delta<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
    ) -> (Vec<usize>, Vec<usize>) {
        let (mut facts, mut checks) = (Vec::new(), Vec::new());
        for rule in rules {
            let (f, c) = self.ground_one(source, schema, rule, true, &[]);
            facts.extend(f);
            checks.extend(c);
        }
        (facts, checks)
    }

    fn ground_one<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rule: &Rule,
        delta: bool,
        extra: &[Triple],
    ) -> (Vec<usize>, Vec<usize>) {
        let (mut facts, mut checks) = (Vec::new(), Vec::new());
        // Source-level transitivity rules (`scm-sco`, `scm-spo`) don't depend on the
        // delta: full grounding registers them, delta grounding leaves them alone.
        if let Some(p) = transitive_predicate(rule) {
            if !delta {
                // With the facts it came from (a property chain `p ∘ p ⊑ p` from a list
                // axiom); vocabulary rules (`scm-sco`, `eq-trans`) come with none.
                self.transitive_with(p, extra.to_vec());
            }
            return (facts, checks);
        }
        let mut grounded = Vec::new();
        let has_schema_atoms = rule.body.iter().any(|a| schema.is_schema_atom(a));
        if delta && has_schema_atoms {
            ground_delta(source, schema, rule, &mut |g| grounded.push(g));
        } else {
            ground(source, schema, rule, &mut |g| grounded.push(g));
        }
        for g in grounded {
            if rule.head == Head::Inconsistent {
                checks.extend(self.add_consistency(rule, g));
            } else {
                let premises = rule
                    .body
                    .iter()
                    .filter(|a| schema.is_schema_atom(a))
                    .map(|a| instantiate_head(a, &g.substitution))
                    .chain(extra.iter().copied())
                    .collect();
                if let Some(rule) = schema.rewrite_heads(g.rule) {
                    facts.extend(self.add_with(rule, premises));
                }
            }
        }
        (facts, checks)
    }

    /// Whether bodiless instances filed facts since the last [`Self::take_facts`].
    pub fn has_pending_facts(&self) -> bool {
        !self.facts.is_empty()
    }

    /// The facts of bodiless instances filed since the last call.
    pub fn take_facts(&mut self) -> Vec<Triple> {
        std::mem::take(&mut self.facts)
    }

    /// The (rule, atom) variants that can match the delta of `delta`.
    pub(crate) fn variants(&self, delta: &super::batch::Store) -> Vec<(usize, usize)> {
        self.dispatch.matching(delta)
    }

    /// The (consistency rule, atom) variants that can match the delta of `delta`.
    pub(crate) fn consistency_variants(&self, delta: &super::batch::Store) -> Vec<(usize, usize)> {
        self.consistency_dispatch.matching(delta)
    }

    /// Every fact rule, with the transitive predicates as transitivity rules.
    pub fn with_transitivity(&self) -> Vec<Rule> {
        let mut rules = self.rules.clone();
        rules.extend(self.transitive.iter().map(|&p| transitivity(p)));
        rules
    }

    /// The facts that one-atom rule instances derive from `fact` (symmetry, inverses,
    /// subproperties, class hierarchy steps, ...).
    pub fn one_step_images(&self, fact: Triple) -> Vec<Triple> {
        let mut refs = Vec::new();
        self.dispatch.matching_fact(fact, &mut refs);
        let mut out = Vec::new();
        for (r, _) in refs {
            let rule = &self.rules[r];
            let (1, Head::Facts(heads)) = (rule.body.len(), &rule.head) else {
                continue;
            };
            let mut bindings = vec![None; rule.variables()];
            if bind(&rule.body[0], fact, &mut bindings).is_some()
                && guards_hold(&rule.guards, &bindings)
            {
                out.extend(heads.iter().map(|h| instantiate_head(h, &bindings)));
            }
        }
        out
    }

    /// The bodies (instantiated premises, plus the schema facts the instance was grounded
    /// on) of up to `limit` one-step derivations of `fact` over `source`, by rules whose
    /// head matches it and transitivity. Bodiless instances are in [`Self::bodiless`].
    pub fn derivation_bodies<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
    ) -> Vec<Vec<Triple>> {
        self.named_derivations(source, fact, limit)
            .into_iter()
            .map(|(_, body)| body)
            .collect()
    }

    /// [`Self::derivation_bodies`], each with the name of the rule it instantiates
    /// (`prp-trp` for transitivity): what explanations show.
    pub fn named_derivations<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
    ) -> Vec<(String, Vec<Triple>)> {
        let mut producers = Vec::new();
        self.heads.matching_fact(fact, &mut producers);
        producers.sort_unstable();
        producers.dedup_by_key(|(r, _)| *r);
        let transitive = self
            .transitive
            .contains(&fact[1])
            .then(|| transitivity(fact[1]));
        let none: &[Triple] = &[];
        let transitive_premises = self
            .transitive_premises
            .get(&fact[1])
            .map_or(none, Vec::as_slice);
        let rules = producers
            .into_iter()
            .map(|(r, _)| (&self.rules[r], self.premises[r].as_slice()))
            .chain(transitive.as_ref().map(|rule| (rule, transitive_premises)));
        let mut bodies = Vec::new();
        for (rule, premises) in rules {
            derivations(source, rule, fact, Seg::All, &mut |bindings| {
                let mut body: Vec<Triple> = rule
                    .body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings))
                    .collect();
                body.extend_from_slice(premises);
                bodies.push((rule.name.clone(), body));
                bodies.len() >= limit
            });
            if bodies.len() >= limit {
                break;
            }
        }
        bodies
    }

    /// Whether `fact` has a one-step derivation from the facts of `source` (`Seg::All`):
    /// a bodiless instance, a rule whose head matches it, or transitivity.
    pub fn derivable<S: Source + ?Sized>(&self, source: &S, fact: Triple) -> bool {
        self.derivable_with(source, fact, true)
    }

    /// [`Self::derivable`], optionally without transitivity (whose closure the caller
    /// recomputes).
    pub fn derivable_with<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        transitivity_too: bool,
    ) -> bool {
        if self.bodiless.contains(&fact) {
            return true;
        }
        let mut producers = Vec::new();
        self.heads.matching_fact(fact, &mut producers);
        producers.sort_unstable();
        producers.dedup_by_key(|(r, _)| *r);
        let mut found = false;
        for (r, _) in producers {
            derivations(source, &self.rules[r], fact, Seg::All, &mut |_| {
                found = true;
                true
            });
            if found {
                return true;
            }
        }
        if transitivity_too && self.transitive.contains(&fact[1]) {
            derivations(source, &transitivity(fact[1]), fact, Seg::All, &mut |_| {
                found = true;
                true
            });
        }
        found
    }
}

impl GroundProgram {
    /// Whether one of the rules `only` marks derives `fact` from `source` in one step.
    pub fn derivable_by<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        only: &[bool],
    ) -> bool {
        let mut producers = Vec::new();
        self.heads.matching_fact(fact, &mut producers);
        producers.sort_unstable();
        producers.dedup_by_key(|(r, _)| *r);
        let mut found = false;
        for (r, _) in producers {
            if !only.get(r).copied().unwrap_or(false) {
                continue;
            }
            derivations(source, &self.rules[r], fact, Seg::All, &mut |_| {
                found = true;
                true
            });
            if found {
                return true;
            }
        }
        false
    }
}

/// What a ground program grounded from schema facts: a rule (by index), a bodiless fact
/// or a transitive predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grounding {
    Rule(usize),
    Bodiless(Triple),
    Transitive(u64),
}

/// The transitivity rule over `p`, for evaluation where the module doesn't apply
/// (overdeletion, rederivation).
pub fn transitivity(p: u64) -> Rule {
    let (x, y, z) = (Term::Var(0), Term::Var(1), Term::Var(2));
    Rule {
        name: "prp-trp".to_owned(),
        body: vec![Atom([x, Term::Const(p), y]), Atom([y, Term::Const(p), z])],
        guards: Vec::new(),
        head: Head::Facts(vec![Atom([x, Term::Const(p), z])]),
    }
}
