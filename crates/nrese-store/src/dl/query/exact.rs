use super::analysis::Shape;
use super::*;
use nrese_dl::limits::{Deadline, Limit};

mod rollup;
use rollup::roll_up;

/// A position of an instantiated atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Slot {
    Const(u64),
    /// An existential variable (or blank node), by name.
    Var(String),
}

use entailment::Test;

pub(super) struct Decided {
    pub(super) verdicts: Vec<Entailed>,
    pub(super) paths: Vec<&'static str>,
    pub(super) why: Option<String>,
}

/// Decides a bounded candidate batch under the operation's remaining deadline.
/// A row the shape can't take is unresolved. Inputs/compiled clauses are shared by
/// jobs; each job owns its search state. The premise's copy, the compiled program and
/// the jobs' engines draw from the request's envelope `memory`; where it has no room,
/// the rows are unresolved.
#[expect(
    clippy::too_many_arguments,
    reason = "the request's limits travel with the batch"
)]
pub(super) fn decide_until(
    store: &StoreService,
    snapshot: &Snapshot,
    analysis: &Analysis,
    variables: &[Variable],
    rows: &[Vec<Option<Term>>],
    cancellation: &CancellationToken,
    deadline: Deadline,
    memory: &nrese_exec::Envelope,
) -> Decided {
    let workers = nrese_exec::workers::Workers::current().limited(store.config().dl.threads);
    let unresolved = |why: &str| Decided {
        verdicts: rows
            .iter()
            .map(|_| Entailed::Unknown(why.to_owned()))
            .collect(),
        paths: Vec::new(),
        why: Some(why.to_owned()),
    };
    let Some(shape) = &analysis.shape else {
        return unresolved(
            "the exact services take one basic graph pattern with filters, not this query",
        );
    };
    if cancellation.is_cancelled() {
        return unresolved("cancelled");
    }
    if deadline.passed() {
        return unresolved(&Limit::Time.reached("the deadline passed"));
    }
    let config = &store.config().dl;
    let Ok(base) = ontology_at(store, snapshot) else {
        return unresolved(nrese_dl::memory::EXHAUSTED);
    };
    let mut scratch = nrese_dl::memory::Scratch::new(Some(memory));
    if scratch
        .charge(|| nrese_dl::memory::ontology_bytes(base.value()))
        .is_err()
    {
        return unresolved(nrese_dl::memory::EXHAUSTED);
    }
    let mut ontology = (**base.value()).clone();
    let ids = Ids::of(snapshot);
    let data_properties = data_properties(&ontology);
    let mut paths = Vec::new();
    let mut tests = Vec::new();
    let mut jobs = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if cancellation.is_cancelled() {
            return unresolved("cancelled");
        }
        if deadline.passed() {
            return unresolved(&Limit::Time.reached("the deadline passed"));
        }
        if let Some(most) = config.max_candidates
            && i >= most
        {
            jobs.push(Err(Entailed::Unknown(Limit::Candidates.reached(most))));
            continue;
        }
        let built = row_tests(
            &mut ontology,
            snapshot,
            (shape, variables),
            row,
            ids,
            &data_properties,
            &mut paths,
        );
        jobs.push(built.map(|built| {
            let start = tests.len();
            tests.extend(built);
            start..tests.len()
        }));
    }
    // The tests' own names, in a private vocabulary charged to the request (dictionary
    // lifecycle V1): ids past the snapshot's, which no premise term has.
    let tx = source::private_over(store.engine(), snapshot, Arc::new(memory.part()));
    let replica = store.dl().replica();
    let fresh: Result<Option<Vec<u64>>, _> = (0..source::FRESH)
        .map(|i| {
            let iri = source::fresh_iri(i);
            let term = NamedNodeRef::new_unchecked(&iri).into();
            source::resolve(replica, &tx, term).map(|id| id.map(TermId::raw))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|ids| ids.into_iter().collect());
    let fresh = match fresh {
        Ok(Some(fresh)) => fresh,
        Ok(None) => {
            return unresolved("the entailment tests' terms haven't reached this replica yet");
        }
        // The request's memory refused the names: as any other refusal here.
        Err(_) => return unresolved(nrese_dl::memory::EXHAUSTED),
    };
    if cancellation.is_cancelled() {
        return unresolved("cancelled");
    }
    if deadline.passed() {
        return unresolved(&Limit::Time.reached("the deadline passed"));
    }
    let compiled = entailment::Batch::new(&mut ontology, &tests, Some(memory));
    let ontology = &ontology;
    let width = workers.for_items(
        jobs.iter()
            .filter(|job| job.as_ref().is_ok_and(|range| !range.is_empty()))
            .count(),
    );
    let workers = workers.limited(width);
    let budget = consistency::Budget {
        timeout: config.timeout,
        memory_bytes: config.memory_per_worker(width),
        memory: Some(memory.clone()),
        workers: workers.limited(1),
        cancel: Some(nrese_dl::tableau::Cancel::from_flag(cancellation.flag())),
        max_nodes: config.max_nodes,
        max_branch_points: config.max_branch_points,
        max_merges: config.max_merges,
        max_join: config.max_join,
        explanation_tests: config.explanation_tests,
    };
    let verdicts = workers.map(&jobs, |job| {
        let range = match job {
            Ok(range) => range,
            Err(verdict) => return verdict.clone(),
        };
        let mut answer = Entailed::Yes;
        for i in range.clone() {
            if cancellation.is_cancelled() {
                return Entailed::Unknown("cancelled".to_owned());
            }
            if deadline.passed() {
                return Entailed::Unknown(Limit::Time.reached("the deadline passed"));
            }
            let found = compiled.check(i, &tests[i], ontology, &fresh, &budget.remaining(deadline));
            match found {
                Entailed::Yes => {}
                other => {
                    answer = other;
                    if answer == Entailed::No {
                        break;
                    }
                }
            }
        }
        answer
    });
    let why = verdicts.iter().find_map(|v| match v {
        Entailed::Unknown(why) => Some(why.clone()),
        _ => None,
    });
    Decided {
        verdicts,
        paths,
        why,
    }
}

/// A row's tests over `o`: `Err(No)` where an atom puts a reserved IRI where an individual
/// goes (no test is needed), `Err(Unknown)` where the row can't be read.
fn row_tests(
    o: &mut Ontology,
    snapshot: &Snapshot,
    (shape, variables): (&Shape, &[Variable]),
    row: &[Option<Term>],
    ids: Ids,
    data_properties: &HashSet<u64>,
    paths: &mut Vec<&'static str>,
) -> Result<Vec<Test>, Entailed> {
    let atoms = instantiate(shape, variables, row, snapshot).map_err(Entailed::Unknown)?;
    if atoms.iter().any(|a| reserved_individual(a, snapshot, ids)) {
        return Err(Entailed::No);
    }
    tests_of(o, snapshot, &atoms, ids, data_properties, paths).map_err(Entailed::Unknown)
}

/// Of the gap's `n` rows (`row(i)` decodes one), those one finished model of the premise
/// refutes: a row whose one test is `C(a)`, `C` a named class, with `C` not in `a`'s label (a
/// model with `a` outside `C` is a countermodel of `C(a)`, *Direct Semantics* §2.5). The
/// model is the consistency test's over the premise the tests read, within the store's DL
/// limits and one deadline, a quarter of what the read has left (none where it has no
/// limit); it refutes nothing unless it ended in a model, `Consistent`, nothing left out.
#[expect(
    clippy::too_many_arguments,
    reason = "the request's limits travel with the rows"
)]
pub(super) fn refuted_by_labels(
    store: &StoreService,
    snapshot: &Snapshot,
    analysis: &Analysis,
    variables: &[Variable],
    n: usize,
    row: &dyn Fn(usize) -> Vec<Option<Term>>,
    cancellation: &CancellationToken,
    deadline: Deadline,
    memory: &nrese_exec::Envelope,
) -> Vec<bool> {
    use nrese_dl::tableau::{Answer, Cancel, Label, Want};
    let config = &store.config().dl;
    let deadline = Deadline::after(deadline.within(config.timeout).map(|t| t / 4));
    let stop = || cancellation.is_cancelled() || deadline.passed();
    let none = || vec![false; n];
    let (Some(shape), Ok(base)) = (&analysis.shape, ontology_at(store, snapshot)) else {
        return none();
    };
    let mut scratch = nrese_dl::memory::Scratch::new(Some(memory));
    let atoms_bytes = n * std::mem::size_of::<Option<(u64, u64)>>();
    let premise = || nrese_dl::memory::ontology_bytes(base.value()) + atoms_bytes;
    if scratch.charge(premise).is_err() {
        return none();
    }
    let mut ontology = (**base.value()).clone();
    let (ids, data_properties) = (Ids::of(snapshot), data_properties(&ontology));
    let mut atoms = Vec::with_capacity(n);
    for i in 0..n {
        if i % 1024 == 0 && stop() {
            return none();
        }
        let tests = row_tests(
            &mut ontology,
            snapshot,
            (shape, variables),
            &row(i),
            ids,
            &data_properties,
            &mut Vec::new(),
        );
        atoms.push(match tests.as_deref() {
            Ok([Test::Axiom(Axiom::ClassAssertion(c, a))]) => match ontology.classes.get(c.0) {
                ClassExpr::Class(c) => Some((*c, *a)),
                _ => None,
            },
            _ => None,
        });
    }
    let (mut classes, mut individuals): (Vec<u64>, Vec<u64>) =
        atoms.iter().flatten().copied().unzip();
    classes.sort_unstable();
    classes.dedup();
    individuals.sort_unstable();
    individuals.dedup();
    if classes.is_empty() || stop() {
        return none();
    }
    let Some(program) = entailment::compile(&ontology, &classes, &individuals, &mut scratch) else {
        return none();
    };
    // The labels' storage while they are held: at most each class twice an individual.
    let labels = individuals.len() * (std::mem::size_of::<Option<Label>>() + 8 * classes.len());
    if stop() || scratch.charge(|| labels).is_err() {
        return none();
    }
    let mut run = config.tableau(deadline.remaining(), config.memory_per_worker(1));
    run.memory = Some(memory.clone());
    run.cancel = Some(Cancel::from_flag(cancellation.flag()));
    run.workers = Some(nrese_exec::workers::Workers::current().limited(1));
    let want = Want {
        individuals: true,
        ..Want::default()
    };
    let (out, _) = program.consistency_with_base(&run, want, false);
    let Some(labels) = out.labels.filter(|_| out.answer == Answer::Consistent) else {
        return none();
    };
    if stop() || consistency::left_out(&ontology).is_some() {
        return none();
    }
    let mut refuted = none();
    for (r, atom) in atoms.iter().enumerate() {
        if r % 1024 == 0 && stop() {
            return none();
        }
        let Some((c, a)) = atom else { continue };
        let (Ok(ci), Ok(ai)) = (classes.binary_search(c), individuals.binary_search(a)) else {
            continue;
        };
        if let Some(label) = &labels.individuals[ai] {
            refuted[r] = label.classes.binary_search(&(ci as u32)).is_err();
        }
    }
    refuted
}

/// The ontology's data properties (an existential value of one is `DataSomeValuesFrom`).
fn data_properties(o: &Ontology) -> HashSet<u64> {
    let mut out = HashSet::new();
    for a in &o.axioms {
        match a {
            Axiom::Declaration(nrese_owl::EntityKind::DataProperty, p)
            | Axiom::DataPropertyAssertion(p, _, _)
            | Axiom::NegativeDataPropertyAssertion(p, _, _)
            | Axiom::DataPropertyDomain(p, _)
            | Axiom::DataPropertyRange(p, _)
            | Axiom::FunctionalDataProperty(p) => {
                out.insert(*p);
            }
            Axiom::SubDataPropertyOf(a, b) => {
                out.insert(*a);
                out.insert(*b);
            }
            _ => {}
        }
    }
    out
}

/// The query's atoms with the row's values in place: answer variables bound, the others
/// (and blank nodes) existential. A filter over a variable the row doesn't bind, or a
/// term the store doesn't know, makes the row unresolvable.
fn instantiate(
    shape: &Shape,
    variables: &[Variable],
    row: &[Option<Term>],
    snapshot: &Snapshot,
) -> Result<Vec<[Slot; 3]>, String> {
    let mut bound: HashMap<&str, &Term> = HashMap::new();
    for (v, value) in variables.iter().zip(row) {
        if let Some(value) = value
            && shape.projected.contains(v)
        {
            bound.insert(v.as_str(), value);
        }
    }
    if shape
        .filtered
        .iter()
        .any(|v| !bound.contains_key(v.as_str()))
    {
        return Err("a filter reads an existential variable".to_owned());
    }
    let constant = |t: &Term| -> Result<Slot, String> {
        snapshot
            .lookup(t.as_ref())
            .map(|id| Slot::Const(id.raw()))
            .ok_or_else(|| format!("{t} isn't in the store"))
    };
    let slot = |t: &TermPattern| -> Result<Slot, String> {
        match t {
            TermPattern::Variable(v) => match bound.get(v.as_str()) {
                Some(value) => constant(value),
                None => Ok(Slot::Var(v.as_str().to_owned())),
            },
            TermPattern::BlankNode(b) => Ok(Slot::Var(format!("_:{}", b.as_str()))),
            TermPattern::NamedNode(n) => constant(&Term::NamedNode(n.clone())),
            TermPattern::Literal(l) => constant(&Term::Literal(l.clone())),
            TermPattern::Triple(_) => Err("a triple term".to_owned()),
        }
    };
    shape
        .patterns
        .iter()
        .map(|t| {
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(n) => constant(&Term::NamedNode(n.clone()))?,
                NamedNodePattern::Variable(v) => match bound.get(v.as_str()) {
                    Some(value) => constant(value)?,
                    None => return Err("a variable predicate".to_owned()),
                },
            };
            Ok([slot(&t.subject)?, predicate, slot(&t.object)?])
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) struct Ids {
    rdf_type: Option<u64>,
    same_as: Option<u64>,
    thing: Option<u64>,
}

impl Ids {
    /// The ids of the vocabulary ground atoms are read with, in `snapshot`.
    pub(crate) fn of(snapshot: &Snapshot) -> Self {
        let id = |iri: &str| {
            snapshot
                .lookup(NamedNodeRef::new_unchecked(iri).into())
                .map(TermId::raw)
        };
        Self {
            rdf_type: id(RDF_TYPE),
            same_as: id(OWL_SAME_AS),
            thing: id(OWL_THING),
        }
    }
}

/// Whether an atom puts a reserved IRI where an individual goes: the subject of `rdf:type`, the
/// subject or the object of a non-reserved property; not a class's place, nor a reserved
/// property's ends (`rdfs:label`, `owl:sameAs`, the list vocabulary). OWL 2 DL has no such
/// individual (Structural Specification §5.6.1), though U1 and the rewriting of
/// `owl:topObjectProperty` (`nrese_owl`'s `universal`) give that IRI one.
fn reserved_individual([s, p, o]: &[Slot; 3], snapshot: &Snapshot, ids: Ids) -> bool {
    use nrese_owl::vocab::{OWL, RDF, RDFS, XSD};
    let reserved = |t: &Slot| match t {
        Slot::Const(t) => match snapshot.decode(TermId::from_raw(*t)) {
            Some(Term::NamedNode(n)) => [RDF, RDFS, OWL, XSD]
                .iter()
                .any(|ns| n.as_str().starts_with(ns)),
            _ => false,
        },
        Slot::Var(_) => false,
    };
    match p {
        Slot::Const(t) if Some(*t) == ids.rdf_type => reserved(s),
        _ if reserved(p) => false,
        _ => reserved(s) || reserved(o),
    }
}

/// The tests that decide one instantiated query: each ground atom not in L, and each
/// tree of existential variables rolled up.
fn tests_of(
    o: &mut Ontology,
    snapshot: &Snapshot,
    atoms: &[[Slot; 3]],
    ids: Ids,
    data_properties: &HashSet<u64>,
    paths: &mut Vec<&'static str>,
) -> Result<Vec<Test>, String> {
    let mut note = |p: &'static str| {
        if !paths.contains(&p) {
            paths.push(p);
        }
    };
    let mut tests = Vec::new();
    let mut existential = Vec::new();
    for atom in atoms {
        match atom {
            [Slot::Const(s), Slot::Const(p), Slot::Const(o_)] => {
                let in_l = snapshot.contains_in(
                    ReadModel::Materialised,
                    &nrese_engine::EncodedTriple::new(
                        TermId::from_raw(*s),
                        TermId::from_raw(*p),
                        TermId::from_raw(*o_),
                    )
                    .in_default_graph(),
                ) || snapshot
                    .quads_for_pattern_in(
                        ReadModel::Materialised,
                        &nrese_engine::QuadPattern {
                            subject: Some(TermId::from_raw(*s)),
                            predicate: Some(TermId::from_raw(*p)),
                            object: Some(TermId::from_raw(*o_)),
                            graph: nrese_engine::GraphSelector::Any,
                        },
                    )
                    .next()
                    .is_some();
                if in_l {
                    continue;
                }
                note("exact-ground-entailment");
                tests.push(Test::Axiom(ground_axiom(o, snapshot, [*s, *p, *o_], ids)?));
            }
            other => existential.push(other.clone()),
        }
    }
    if !existential.is_empty() {
        note("exact-internalisable-cq");
        tests.extend(roll_up(o, snapshot, &existential, ids, data_properties)?);
    }
    Ok(tests)
}

/// The axiom a ground atom states.
pub(crate) fn ground_axiom(
    o: &mut Ontology,
    snapshot: &Snapshot,
    [s, p, v]: [u64; 3],
    ids: Ids,
) -> Result<Axiom, String> {
    if Some(p) == ids.rdf_type {
        if Some(v) == ids.thing {
            return Ok(Axiom::ClassAssertion(
                ExprId(o.classes.intern(ClassExpr::Thing)),
                s,
            ));
        }
        if TermId::from_raw(v).kind() != nrese_engine::TermKind::Iri {
            return Err("a class that isn't an IRI".to_owned());
        }
        return Ok(Axiom::ClassAssertion(
            ExprId(o.classes.intern(ClassExpr::Class(v))),
            s,
        ));
    }
    if Some(p) == ids.same_as {
        return Ok(Axiom::SameIndividual(vec![s, v]));
    }
    if TermId::from_raw(v).kind().is_literal() {
        note_literal(o, snapshot, v);
        return Ok(Axiom::DataPropertyAssertion(p, s, v));
    }
    Ok(Axiom::ObjectPropertyAssertion(p, s, v))
}

/// Gives the datatype theory a literal's parts.
fn note_literal(o: &mut Ontology, snapshot: &Snapshot, v: u64) {
    if o.data.literals.contains_key(&v) {
        return;
    }
    if let Some(Term::Literal(l)) = snapshot.decode(TermId::from_raw(v)) {
        o.data.literals.insert(
            v,
            Literal {
                lexical: l.value().to_owned(),
                datatype: Some(l.datatype().as_str().to_owned()),
                language: l.language().map(str::to_owned),
            },
        );
    }
}
