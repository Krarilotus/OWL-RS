//! ABox islands (after Wandelt and Möller, *Towards ABox Modularization of semi-expressive
//! Description Logics*, Applied Ontology 2012): the assertions split into parts the
//! hypertableau decides apart, so a consistency check builds one model per island rather
//! than one of the whole ABox, which on a store's data is one connected component
//! (OWL2Bench: 3,641 of 3,644 individuals).
//!
//! **Which role assertions split.** A role assertion `R(a, b)` carries information from
//! one individual to the other only through a DL-clause that reads, beside a role atom
//! over `R` or a super-role (inverses included), something at both of its ends:
//! `A(x) ∧ R(x, y) → B(y)` (a universal), `R(x, y) ∧ C(y) → D(x)` (`∃R.C ⊑ D`), the
//! successors an at-most counts, a chain or transitivity through `y`, a disjoint or an
//! asymmetric role (both atoms over the pair). A role all of whose clauses read only one
//! end (a domain, a range, `R(x, y) ∧ A(x) → C(x)`) is *splittable*: its assertion is
//! replaced by `∃R.⊤` at `a` and `∃R⁻.⊤` at `b`, each in its own island. A role
//! inclusion passes splittability down: a role splits only if its super-roles do.
//!
//! **Sound and complete for consistency:** a model of the whole ABox is one of each island
//! (the role edge gives the existentials). Conversely the disjoint union of the islands'
//! models with the split edges added is a model: no clause reads across a split edge, so
//! adding it violates none, and its existential and domain-range conclusions hold already.
//!
//! **What connects individuals:** an assertion of an unsplittable role, `sameAs`,
//! `differentFrom`, a negative property assertion, and a key: individuals with a value in
//! common (OWL 2's identity of data values) for one of its data properties, or an object
//! of its object properties (those are unsplittable).
//!
//! **The whole ABox instead:** with a nominal in a clause's head (`C ⊑ ∃R.{o}`: an
//! individual every island may reach) or the universal property (every pair of
//! individuals). A nominal in a body only reads an individual's identity: the role atoms
//! beside it read across their edges, so their assertions join its island.
//!
//! Islands are packed into batches of about [`BATCH`] assertions (fewer where the node
//! budget is smaller: a quarter of it, so a batch fits where the whole ABox didn't), each
//! decided as one
//! ontology: the TBox, the batch's assertions, the stubs of its split edges.

use std::collections::{HashMap, HashSet};

use nrese_owl::{
    Axiom, BodyAtom, ClassExpr, ExprId, HeadAtom, ObjProp, Ontology, SafeRule, Term, Var,
};
use nrese_xsd::owl::{Datatype, Value};

use crate::tableau;
use crate::tableau::budget::{RunBudget, memory_share, workers};

#[cfg(test)]
#[path = "islands/resources_tests.rs"]
mod resources_tests;

/// About how many assertions one batch of islands holds.
pub const BATCH: usize = 2048;

/// How an ontology's ABox is decided.
pub enum Split {
    /// As one: why it can't split, or that it is one island.
    Whole(String),
    /// Batches of islands, each an ontology of its own.
    Islands(Islands),
}

/// The islands of an ABox and the batches they are decided in.
pub struct Islands {
    /// The individuals of each island, largest first.
    pub sizes: Vec<usize>,
    /// Role assertions replaced by their two existentials.
    pub split_edges: usize,
    pub batches: Vec<Ontology>,
}

/// The ABox of `ontology` in islands, or why it is decided whole.
pub fn split(ontology: &Ontology) -> Split {
    split_into(ontology, BATCH)
}

/// The batch size for `config`: [`BATCH`] assertions, at most a quarter of its node budget.
fn batch_for(config: &tableau::Config) -> usize {
    BATCH.min((config.max_nodes / 4).max(16))
}

/// [`split`] with batches of about `batch` assertions.
fn split_into(ontology: &Ontology, batch: usize) -> Split {
    // What the tableau decides: a negative assertion over a non-simple property is a
    // universal there (`tableau::prepared`), which reads the edges the property's chains
    // compose, so the analysis must see it too.
    let prepared = tableau::prepared(ontology);
    let ontology = prepared.as_ref();
    let roles = match unsplittable(ontology) {
        Ok(roles) => roles,
        Err(why) => return Split::Whole(why),
    };
    let mut parts = Parts::default();
    let keys = keys(ontology);
    for axiom in ontology.axioms.iter().filter(|a| a.is_assertion()) {
        match axiom {
            Axiom::ClassAssertion(_, a)
            | Axiom::DataPropertyAssertion(_, a, _)
            | Axiom::NegativeDataPropertyAssertion(_, a, _) => {
                parts.add(*a);
            }
            Axiom::ObjectPropertyAssertion(p, a, b) => match roles.contains(p) {
                true => parts.join(*a, *b),
                false => {
                    parts.add(*a);
                    parts.add(*b);
                }
            },
            Axiom::NegativeObjectPropertyAssertion(_, a, b) => parts.join(*a, *b),
            Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => {
                for pair in xs.windows(2) {
                    parts.join(pair[0], pair[1]);
                }
            }
            _ => {}
        }
    }
    // A key equates individuals with a value in common: they meet in one island.
    let mut by_value: HashMap<(Term, KeyValue), Term> = HashMap::new();
    for axiom in &ontology.axioms {
        if let Axiom::DataPropertyAssertion(d, a, v) = axiom
            && keys.contains(d)
        {
            match by_value.entry((*d, key_value(ontology, *v))) {
                std::collections::hash_map::Entry::Occupied(e) => parts.join(*e.get(), *a),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(*a);
                }
            }
        }
    }
    parts.islands(ontology, &roles, batch)
}

/// Whether `ontology` is consistent: the whole ABox first, and island by island
/// ([`by_islands`]) where that gives up within its budget and the ABox splits. Deciding
/// by islands costs the split and a run per batch, more than one model of an easy ABox
/// (LUBM(1): whole 149–160 ms, by islands 167–204 ms; OWL2Bench QL-1: 189–217 against
/// 256–434 ms); it pays where one model of the whole ABox exceeds the budget.
pub fn consistency(ontology: &Ontology, config: &tableau::Config) -> tableau::Outcome {
    let budget = RunBudget::new(config);
    let whole = budget.run(config, |remaining| {
        tableau::consistency(ontology, remaining)
    });
    if !matches!(whole.answer, tableau::Answer::GaveUp(_)) {
        return whole;
    }
    let split = match budget.stage(config, || split_into(ontology, batch_for(config))) {
        Ok(split) => split,
        Err(why) => return budget.stopped(why),
    };
    match split {
        Split::Whole(_) => whole,
        Split::Islands(islands) => match decide(&islands, config, &budget, tableau::consistency) {
            Some(outcome)
                if matches!(
                    outcome.answer,
                    tableau::Answer::Consistent | tableau::Answer::Inconsistent
                ) =>
            {
                outcome
            }
            _ => whole,
        },
    }
}

/// The ABox of `ontology` decided island by island (the whole ABox where it can't split,
/// [`split`]): inconsistent as soon as an island is, consistent once every island is;
/// otherwise the first island's reason for not deciding. `config`'s timeout holds for the
/// whole check.
pub fn by_islands(ontology: &Ontology, config: &tableau::Config) -> tableau::Outcome {
    let budget = RunBudget::new(config);
    let split = match budget.stage(config, || split_into(ontology, batch_for(config))) {
        Ok(split) => split,
        Err(why) => return budget.stopped(why),
    };
    match split {
        Split::Whole(_) => budget.run(config, |remaining| {
            tableau::consistency(ontology, remaining)
        }),
        Split::Islands(islands) => decide(&islands, config, &budget, tableau::consistency)
            .unwrap_or_else(|| {
                budget.run(config, |remaining| {
                    tableau::consistency(ontology, remaining)
                })
            }),
    }
}

/// The islands' verdict (`None` without a batch: no assertion).
/// O(batches) dispatch over bounded independent checks, preserving input order.
fn decide(
    islands: &Islands,
    config: &tableau::Config,
    budget: &RunBudget,
    check: impl Fn(&Ontology, &tableau::Config) -> tableau::Outcome + Sync + Send,
) -> Option<tableau::Outcome> {
    if islands.batches.is_empty() {
        return None;
    }
    // An island has at most two portfolio variants. With fewer islands than workers,
    // retain their useful nested race; otherwise give each concurrent island one slot.
    let workers = match budget.stage(config, || {
        workers(config, islands.batches.len().saturating_mul(2))
    }) {
        Ok(workers) => workers,
        Err(why) => return Some(budget.stopped(why)),
    };
    let concurrent = workers.for_items(islands.batches.len());
    let mut batch_config = config.clone();
    batch_config.max_memory = memory_share(config.max_memory, concurrent);
    batch_config.workers = Some(workers.limited(workers.width() / concurrent));
    let outcomes = workers.limited(concurrent).map(&islands.batches, |batch| {
        budget.run(&batch_config, |remaining| check(batch, remaining))
    });
    let mut undecided: Option<tableau::Outcome> = None;
    let mut last = None;
    for outcome in outcomes {
        match outcome.answer {
            tableau::Answer::Inconsistent => return Some(outcome),
            tableau::Answer::Consistent => last = Some(outcome),
            _ => {
                undecided.get_or_insert(outcome);
            }
        }
    }
    undecided.or(last)
}

/// The roles whose assertions carry information between their individuals (the module
/// docs), or why the ABox can't split at all.
fn unsplittable(ontology: &Ontology) -> Result<HashSet<Term>, String> {
    let normalised = nrese_owl::normalise(&analysed(ontology));
    let top = ontology.builtin.top_object;
    let mut out: HashSet<Term> = HashSet::new();
    // `sub → sup`: a role inclusion (an atom over the same pair in the head).
    let mut inclusions: Vec<(Term, Term)> = Vec::new();
    for clause in &normalised.clauses {
        // A nominal in a head makes some term equal to the individual: what a member of
        // any island implies about it (`C ⊑ ∃R.({o} ⊓ D)`: `D(o)` if anything is a `C`).
        // In a body it only reads the individual's identity: a role atom beside it reads
        // across its edge and stays unsplittable, so its assertions join the island.
        if clause
            .head
            .iter()
            .any(|a| matches!(a, HeadAtom::Nominal(..)))
        {
            return Err("a nominal in a conclusion (an individual every island may reach)".into());
        }
        let roles = role_atoms(&clause.body, &clause.head);
        if roles.iter().any(|&(r, _, _)| Some(r) == top) {
            return Err("the ontology uses the universal property".into());
        }
        for atom in &clause.head {
            if let HeadAtom::AtMost { role, .. } = atom {
                out.insert(term_of(*role));
            }
        }
        // `R(x, y) → S(x, y)` (or `S(y, x)`): the inclusion of `R` in `S`.
        let inclusion = match clause.body.as_slice() {
            [BodyAtom::Role(r, u, v)]
                if !clause.head.is_empty()
                    && clause.head.iter().all(|h| {
                        matches!(h, HeadAtom::Role(_, a, b)
                            if (a, b) == (u, v) || (a, b) == (v, u))
                    }) =>
            {
                Some((*r, *u, *v))
            }
            _ => None,
        };
        if let Some((r, u, v)) = inclusion {
            if u == v {
                out.insert(r);
            }
            for h in &clause.head {
                if let HeadAtom::Role(s, _, _) = h {
                    inclusions.push((r, *s));
                }
            }
            continue;
        }
        let owners = owners(&clause.body, &clause.head);
        let atoms = mentions(&clause.body, &clause.head, &owners);
        for (i, &(r, u, v)) in roles.iter().enumerate() {
            if u == v {
                out.insert(r);
                continue;
            }
            // The other atoms: does one read `u` and one read `v`?
            let others = atoms
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != role_index(&atoms, i));
            let (mut at_u, mut at_v) = (false, false);
            for (_, (_, vars)) in others {
                at_u |= vars.contains(&u);
                at_v |= vars.contains(&v);
            }
            if at_u && at_v {
                out.insert(r);
            }
        }
    }
    // A key's object properties connect the individuals it may equate.
    for rule in &normalised.rules {
        out.extend(key_roles(rule));
    }
    // A role is unsplittable if a super-role is.
    loop {
        let before = out.len();
        for &(sub, sup) in &inclusions {
            if out.contains(&sup) {
                out.insert(sub);
            }
        }
        if out.len() == before {
            return Ok(out);
        }
    }
}

/// The TBox of `ontology` with each complex class an assertion uses, once: what the
/// clauses that may read role assertions come from.
fn analysed(ontology: &Ontology) -> Ontology {
    let mut seen: HashSet<ExprId> = HashSet::new();
    let mut out = Ontology {
        axioms: Vec::new(),
        sources: Vec::new(),
        ..ontology.clone()
    };
    for (axiom, sources) in ontology.axioms.iter().zip(&ontology.sources) {
        let keep = match axiom {
            Axiom::ClassAssertion(c, _) => {
                !matches!(ontology.classes.get(c.0), ClassExpr::Class(_)) && seen.insert(*c)
            }
            a => !a.is_assertion(),
        };
        if keep {
            out.axioms.push(axiom.clone());
            out.sources.push(sources.clone());
        }
    }
    out
}

fn term_of(role: ObjProp) -> Term {
    match role {
        ObjProp::Named(t) | ObjProp::Inverse(t) => t,
    }
}

/// The role atoms of a clause, body then head: `(role, from, to)`.
fn role_atoms(body: &[BodyAtom], head: &[HeadAtom]) -> Vec<(Term, Var, Var)> {
    let body = body.iter().filter_map(|a| match a {
        BodyAtom::Role(r, u, v) => Some((*r, *u, *v)),
        _ => None,
    });
    let head = head.iter().filter_map(|a| match a {
        HeadAtom::Role(r, u, v) => Some((*r, *u, *v)),
        _ => None,
    });
    body.chain(head).collect()
}

/// The individual each data variable of a clause is a value of.
fn owners(body: &[BodyAtom], head: &[HeadAtom]) -> HashMap<Var, Var> {
    let mut out = HashMap::new();
    for a in body {
        if let BodyAtom::Data(_, x, v) = a {
            out.insert(*v, *x);
        }
    }
    for a in head {
        if let HeadAtom::DataRole(_, x, v) = a {
            out.insert(*v, *x);
        }
    }
    out
}

/// Every atom of a clause (body then head) with the individuals it reads (data values
/// read as their individuals); a role atom is marked so it can be left out of its own
/// check.
fn mentions(
    body: &[BodyAtom],
    head: &[HeadAtom],
    owners: &HashMap<Var, Var>,
) -> Vec<(bool, Vec<Var>)> {
    let of = |v: &Var| owners.get(v).copied().unwrap_or(*v);
    let mut out = Vec::new();
    for a in body {
        out.push(match a {
            BodyAtom::Concept(_, x) | BodyAtom::Nominal(_, x) => (false, vec![*x]),
            BodyAtom::Role(_, u, v) => (true, vec![*u, *v]),
            BodyAtom::Data(_, x, _) => (false, vec![*x]),
        });
    }
    for a in head {
        out.push(match a {
            HeadAtom::Concept(_, x) | HeadAtom::Nominal(_, x) => (false, vec![*x]),
            HeadAtom::Role(_, u, v) => (true, vec![*u, *v]),
            HeadAtom::AtLeast { var, .. }
            | HeadAtom::AtMost { var, .. }
            | HeadAtom::DataAtLeast { var, .. }
            | HeadAtom::DataAtMost { var, .. } => (false, vec![*var]),
            HeadAtom::Equal(u, v) => (false, vec![*u, *v]),
            HeadAtom::DataIn(_, v) => (false, vec![of(v)]),
            HeadAtom::DataEqual(u, v) | HeadAtom::DataUnequal(u, v) => (false, vec![of(u), of(v)]),
            HeadAtom::DataRole(_, x, _) => (false, vec![*x]),
        });
    }
    out
}

/// The index among `atoms` of the `i`-th role atom.
fn role_index(atoms: &[(bool, Vec<Var>)], i: usize) -> usize {
    atoms
        .iter()
        .enumerate()
        .filter(|(_, (role, _))| *role)
        .nth(i)
        .map_or(usize::MAX, |(j, _)| j)
}

/// The object properties of a key's rule.
fn key_roles(rule: &SafeRule) -> Vec<Term> {
    rule.body
        .iter()
        .filter_map(|a| match a {
            BodyAtom::Role(r, _, _) => Some(*r),
            _ => None,
        })
        .collect()
}

/// The data properties of the ontology's keys.
fn keys(ontology: &Ontology) -> HashSet<Term> {
    ontology
        .axioms
        .iter()
        .filter_map(|a| match a {
            Axiom::HasKey(_, _, data) => Some(data.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// A key's value as a key compares it: OWL 2's identity of data values, or, for a literal
/// whose value can't be read, one class for all such (never kept apart wrongly).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum KeyValue {
    Value(Value),
    Unread,
}

fn key_value(ontology: &Ontology, literal: Term) -> KeyValue {
    let Some(l) = ontology.data.literals.get(&literal) else {
        return KeyValue::Unread;
    };
    let datatype = l.datatype.as_deref().and_then(Datatype::from_iri);
    match datatype.map(|d| Value::parse(&l.lexical, d, l.language.as_deref())) {
        Some(Ok(v)) => KeyValue::Value(v),
        _ => KeyValue::Unread,
    }
}

/// Individuals in islands (a union-find).
#[derive(Default)]
struct Parts {
    index: HashMap<Term, usize>,
    parent: Vec<usize>,
}

impl Parts {
    fn add(&mut self, a: Term) -> usize {
        let next = self.parent.len();
        let i = *self.index.entry(a).or_insert(next);
        if i == next {
            self.parent.push(i);
        }
        i
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: Term, b: Term) {
        let (a, b) = (self.add(a), self.add(b));
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }

    /// The islands of the assertions, packed into batches.
    fn islands(mut self, ontology: &Ontology, roles: &HashSet<Term>, batch: usize) -> Split {
        let n = self.parent.len();
        let roots: Vec<usize> = (0..n).map(|i| self.find(i)).collect();
        let mut island_of: HashMap<usize, usize> = HashMap::new();
        for &r in &roots {
            let next = island_of.len();
            island_of.entry(r).or_insert(next);
        }
        let count = island_of.len();
        if count <= 1 {
            return Split::Whole("the ABox is one island".into());
        }
        let island = |t: &Term| island_of[&roots[self.index[t]]];
        let mut sizes = vec![0usize; count];
        for &r in &roots {
            sizes[island_of[&r]] += 1;
        }
        // Each island's assertions, a split edge's stubs on each side.
        let mut base = Ontology {
            axioms: Vec::new(),
            sources: Vec::new(),
            ..ontology.clone()
        };
        let thing = ExprId(base.classes.intern(ClassExpr::Thing));
        let stub = |base: &mut Ontology, p: ObjProp| {
            ExprId(base.classes.intern(ClassExpr::Some(p, thing)))
        };
        let mut per: Vec<Vec<Axiom>> = vec![Vec::new(); count];
        let mut split_edges = 0;
        let mut tbox: Vec<(Axiom, Vec<nrese_owl::Source>)> = Vec::new();
        for (axiom, sources) in ontology.axioms.iter().zip(&ontology.sources) {
            if !axiom.is_assertion() {
                tbox.push((axiom.clone(), sources.clone()));
                continue;
            }
            match axiom {
                Axiom::ObjectPropertyAssertion(p, a, b)
                    if !roles.contains(p) && island(a) != island(b) =>
                {
                    split_edges += 1;
                    let out = stub(&mut base, ObjProp::Named(*p));
                    let into = stub(&mut base, ObjProp::Inverse(*p));
                    per[island(a)].push(Axiom::ClassAssertion(out, *a));
                    per[island(b)].push(Axiom::ClassAssertion(into, *b));
                }
                Axiom::ClassAssertion(_, a)
                | Axiom::DataPropertyAssertion(_, a, _)
                | Axiom::NegativeDataPropertyAssertion(_, a, _)
                | Axiom::ObjectPropertyAssertion(_, a, _)
                | Axiom::NegativeObjectPropertyAssertion(_, a, _) => {
                    per[island(a)].push(axiom.clone());
                }
                Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => {
                    if let Some(a) = xs.first() {
                        per[island(a)].push(axiom.clone());
                    }
                }
                _ => {}
            }
        }
        // Largest islands first, packed into batches of about `batch` assertions.
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(per[i].len()));
        let mut batches = Vec::new();
        let mut current: Vec<Axiom> = Vec::new();
        let flush = |current: &mut Vec<Axiom>, batches: &mut Vec<Ontology>| {
            if current.is_empty() {
                return;
            }
            let mut o = base.clone();
            let mut all: Vec<(Axiom, Vec<nrese_owl::Source>)> = tbox.clone();
            all.extend(current.drain(..).map(|a| (a, Vec::new())));
            all.sort_by(|a, b| a.0.cmp(&b.0));
            all.dedup_by(|a, b| a.0 == b.0);
            for (a, s) in all {
                o.axioms.push(a);
                o.sources.push(s);
            }
            batches.push(o);
        };
        for i in order {
            if !current.is_empty() && current.len() + per[i].len() > batch {
                flush(&mut current, &mut batches);
            }
            current.append(&mut per[i]);
        }
        flush(&mut current, &mut batches);
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        Split::Islands(Islands {
            sizes,
            split_edges,
            batches,
        })
    }
}
