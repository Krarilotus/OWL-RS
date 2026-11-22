use nrese_engine::{EncodedTriple, Engine, EngineConfig, GraphSelector, Snapshot, Transaction};
use nrese_rdf::{NamedNode, NamedNodeRef, Quad};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{
    Selection, Shapes, ValidationReport, compile, validate, validate_changes,
    validate_shapes_change,
};
use nrese_sparql::{QueryEvaluationError, Resources};

use super::gaps::EntryPoint;
use super::observe::{Observation, Observer};
use super::stops::{Outcome, Stop, TripView};

const PREFIXES: &str = "
    @prefix ex: <http://example.com/> .
    @prefix sh: <http://www.w3.org/ns/shacl#> .
    @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
    @prefix owl: <http://www.w3.org/2002/07/owl#> .
";

pub(super) fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

pub(super) fn quads(turtle: &str) -> Vec<Quad> {
    RdfParser::from_format(RdfFormat::TriG)
        .for_slice(format!("{PREFIXES}{turtle}").as_bytes())
        .map(|quad| quad.expect("case RDF"))
        .collect()
}

fn shape_quads(turtle: &str) -> Vec<Quad> {
    quads(turtle)
        .into_iter()
        .map(|quad| Quad::new(quad.subject, quad.predicate, quad.object, ex("shapes")))
        .collect()
}

#[derive(Clone, Default)]
pub(super) struct Step {
    pub add: Vec<Quad>,
    pub remove: Vec<Quad>,
    pub infer: Vec<Quad>,
    pub retract: Vec<Quad>,
    pub shapes: Option<String>,
}

impl Step {
    pub fn insert(turtle: &str) -> Self {
        Self {
            add: quads(turtle),
            ..Self::default()
        }
    }

    pub fn delete(turtle: &str) -> Self {
        Self {
            remove: quads(turtle),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub(super) struct Case {
    pub asserted: Vec<Quad>,
    pub inferred: Vec<Quad>,
    pub shapes: Option<String>,
    pub steps: Vec<Step>,
    pub fresh_value: bool,
    pub equality: bool,
    pub stop: Option<(EntryPoint, Stop)>,
}

impl Case {
    pub fn new(shapes: impl Into<String>, data: &str, step: Step) -> Self {
        Self {
            asserted: quads(data),
            inferred: vec![],
            shapes: Some(shapes.into()),
            steps: vec![step],
            fresh_value: false,
            equality: false,
            stop: None,
        }
    }

    pub fn run(&self) -> Result<Vec<Evaluation>, String> {
        let engine = Engine::new(EngineConfig::default()).map_err(|error| error.to_string())?;
        let mut shapes_rdf = shape_quads(self.shapes.as_deref().unwrap_or_default());
        let mut tx = engine.transaction();
        let same_as = self.equality.then(|| {
            tx.intern(NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#sameAs").into())
        });
        // Keep the graph's name in the dictionary even for an empty set of shapes.
        tx.insert(Quad::new(ex("shapes"), ex("graph"), ex("shapes"), ex("shapes")).as_ref());
        for quad in self.asserted.iter().chain(&shapes_rdf) {
            tx.insert(quad.as_ref());
        }
        for quad in &self.inferred {
            tx.insert_inferred(inferred(&tx, quad));
        }
        tx.commit().map_err(|error| error.to_string())?;
        let read = || {
            let snapshot = engine.snapshot();
            if let Some(same_as) = same_as {
                snapshot.with_equality(Some(same_as))
            } else {
                snapshot
            }
        };
        let mut before = read();
        let shapes_id = before
            .lookup(ex("shapes").as_ref().into())
            .expect("shapes graph");
        let data = Selection::of(GraphSelector::Any).excluding(shapes_id);
        let shape_graph = Selection::asserted(GraphSelector::Exact(shapes_id));
        let compile_shapes = |snapshot: &Snapshot| {
            compile(snapshot, shape_graph).map_err(|errors| format!("case shapes: {errors:?}"))
        };
        let mut shapes = self
            .shapes
            .as_ref()
            .map(|_| compile_shapes(&before))
            .transpose()?;
        let mut evaluations = Vec::new();
        for (index, step) in self.steps.iter().enumerate() {
            let mut tx = engine.transaction();
            if let Some(turtle) = &step.shapes {
                for quad in &shapes_rdf {
                    // Transaction::remove resolves the quad and calls remove_encoded.
                    tx.remove(quad.as_ref());
                }
                shapes_rdf = shape_quads(turtle);
                for quad in &shapes_rdf {
                    tx.insert(quad.as_ref());
                }
            }
            for quad in &step.remove {
                tx.remove(quad.as_ref());
            }
            for quad in &step.add {
                tx.insert(quad.as_ref());
            }
            for quad in &step.retract {
                tx.remove_inferred(inferred(&tx, quad));
            }
            for quad in &step.infer {
                tx.insert_inferred(inferred(&tx, quad));
            }
            let changed = tx
                .inserted()
                .chain(tx.deleted())
                .chain(
                    tx.inferred_inserted()
                        .chain(tx.inferred_deleted())
                        .map(EncodedTriple::in_default_graph),
                )
                .collect::<Vec<_>>();
            tx.commit().map_err(|error| error.to_string())?;
            let after = read();
            let observer = Observer::new(&before, &after, self.fresh_value);
            let empty = Shapes::default();
            let shapes_before = shapes.as_ref().unwrap_or(&empty);
            let reference = |snapshot, shapes| {
                let report = validate(snapshot, shapes, data, &Resources::default())
                    .map_err(|error| format!("reference stopped: {error}"))?;
                observer.observe(&report)
            };
            let full_before = reference(&before, shapes_before)?;
            let full_after = reference(&after, shapes_before)?;
            let observe = |entry, check: &Check<'_>| {
                let stop = self
                    .stop
                    .filter(|(api, _)| *api == entry)
                    .map(|(_, stop)| stop);
                observe_actual(stop, &before, &after, &observer, check)
            };
            evaluations.push(Evaluation {
                step: index + 1,
                entry: EntryPoint::Data,
                before: full_before.clone(),
                after: full_after,
                actual: observe(EntryPoint::Data, &|before, after, resources| {
                    validate_changes(before, after, shapes_before, data, &changed, resources)
                })?,
            });
            if step.shapes.is_some() {
                let shapes_after = compile_shapes(&after)?;
                let full_after = reference(&after, &shapes_after)?;
                evaluations.push(Evaluation {
                    step: index + 1,
                    entry: EntryPoint::Shapes,
                    before: full_before,
                    after: full_after,
                    actual: observe(EntryPoint::Shapes, &|before, after, resources| {
                        validate_shapes_change(
                            before,
                            shapes.as_ref(),
                            after,
                            &shapes_after,
                            data,
                            resources,
                        )
                    })?,
                });
                shapes = Some(shapes_after);
            }
            before = after;
        }
        Ok(evaluations)
    }
}

type Check<'a> = dyn Fn(&TripView<'_>, &TripView<'_>, &Resources) -> Result<ValidationReport, QueryEvaluationError>
    + 'a;

fn observe_actual(
    stop: Option<Stop>,
    before: &Snapshot,
    after: &Snapshot,
    observer: &Observer<'_>,
    check: &Check<'_>,
) -> Result<Outcome, String> {
    let resources = stop.map_or_else(Resources::default, Stop::resources);
    let token = |on| resources.cancellation.as_ref().filter(|_| stop == Some(on));
    let before = TripView::new(before, token(Stop::Before));
    let after = TripView::new(after, token(Stop::After));
    match check(&before, &after, &resources) {
        Ok(report) => observer.observe(&report).map(Outcome::Report),
        Err(QueryEvaluationError::Cancelled) => Ok(Outcome::Cancelled {
            before: before.tripped(),
            after: after.tripped(),
        }),
        Err(QueryEvaluationError::MemoryLimit(_)) => Ok(Outcome::MemoryLimit),
        Err(error) => Err(error.to_string()),
    }
}

fn inferred(tx: &Transaction<'_>, quad: &Quad) -> EncodedTriple {
    let quad = quad.as_ref();
    assert!(quad.graph_name.is_default_graph());
    EncodedTriple::new(
        tx.intern(quad.subject.into()),
        tx.intern(quad.predicate.into()),
        tx.intern(quad.object),
    )
}

pub(super) struct Evaluation {
    pub step: usize,
    pub entry: EntryPoint,
    pub before: Observation,
    pub after: Observation,
    pub actual: Outcome,
}
