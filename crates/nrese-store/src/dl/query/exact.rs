use super::analysis::Shape;
use super::*;

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
/// jobs; each job owns its search state.
pub(super) fn decide_until(
    store: &StoreService,
    snapshot: &Snapshot,
    analysis: &Analysis,
    variables: &[Variable],
    rows: &[Vec<Option<Term>>],
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Decided {
    let workers = store.runtime().workers().limited(store.config().dl.threads);
    workers.install(|| {
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
        if Instant::now() >= deadline {
            return unresolved("past dl.timeout");
        }
        let config = &store.config().dl;
        let base = ontology_at(store, snapshot);
        let mut ontology = (*base).clone();
        let rdf_type = snapshot.lookup(NamedNodeRef::new_unchecked(RDF_TYPE).into());
        let same_as = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_SAME_AS).into());
        let thing = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_THING).into());
        let data_properties = data_properties(&ontology);
        let mut paths = Vec::new();
        let mut tests = Vec::new();
        let mut jobs = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            if cancellation.is_cancelled() {
                return unresolved("cancelled");
            }
            if Instant::now() >= deadline {
                return unresolved("past dl.timeout");
            }
            if i >= config.max_candidates {
                jobs.push(Err(format!(
                    "past dl.max_candidates ({})",
                    config.max_candidates
                )));
                continue;
            }
            let instantiated = instantiate(shape, variables, row, snapshot);
            let built = instantiated.and_then(|atoms| {
                tests_of(
                    &mut ontology,
                    snapshot,
                    &atoms,
                    Ids {
                        rdf_type: rdf_type.map(TermId::raw),
                        same_as: same_as.map(TermId::raw),
                        thing: thing.map(TermId::raw),
                    },
                    &data_properties,
                    &mut paths,
                )
            });
            jobs.push(built.map(|built| {
                let start = tests.len();
                tests.extend(built);
                start..tests.len()
            }));
        }
        let tx = store.engine().speculative();
        let replica = store.dl().replica();
        let fresh: Option<Vec<u64>> = (0..source::FRESH)
            .map(|i| {
                source::resolve(
                    replica,
                    &tx,
                    NamedNodeRef::new_unchecked(&source::fresh_iri(i)).into(),
                )
                .map(TermId::raw)
            })
            .collect();
        let Some(fresh) = fresh else {
            return unresolved("the entailment tests' terms haven't reached this replica yet");
        };
        let compiled = entailment::Batch::new(&mut ontology, &tests);
        let ontology = &ontology;
        let cancel = nrese_dl::tableau::Cancel::from_flag(cancellation.flag());
        let width = workers.for_items(
            jobs.iter()
                .filter(|job| job.as_ref().is_ok_and(|range| !range.is_empty()))
                .count(),
        );
        let workers = workers.limited(width);
        let child = workers.limited(1);
        let verdicts = workers.map(&jobs, |job| {
            let range = match job {
                Ok(range) => range,
                Err(why) => return Entailed::Unknown(why.clone()),
            };
            let mut answer = Entailed::Yes;
            for i in range.clone() {
                if cancellation.is_cancelled() {
                    return Entailed::Unknown("cancelled".to_owned());
                }
                if Instant::now() >= deadline {
                    return Entailed::Unknown("past dl.timeout".to_owned());
                }
                let budget = consistency::Budget {
                    timeout: deadline.saturating_duration_since(Instant::now()),
                    memory_bytes: config.memory_per_worker(width),
                    threads: 1,
                    workers: Some(child.clone()),
                    cancel: Some(cancel.clone()),
                    max_nodes: config.max_nodes,
                    max_branch_points: config.max_branch_points,
                };
                let found = compiled.check(i, &budget).unwrap_or_else(|| {
                    let budget = consistency::Budget {
                        timeout: deadline.saturating_duration_since(Instant::now()),
                        ..budget
                    };
                    match &tests[i] {
                        Test::Axiom(axiom) => entailment::entails(ontology, axiom, &fresh, &budget),
                        Test::Nonempty(class) => entailment::nonempty(ontology, *class, &budget),
                    }
                });
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
    })
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

fn is_literal(t: u64) -> bool {
    use nrese_engine::TermKind;
    !matches!(
        TermId::from_raw(t).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
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
    if is_literal(v) {
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
