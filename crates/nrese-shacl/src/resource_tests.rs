//! Distinguishing admission/stop/lifetime probes; delegate decoding to the owner hook.
use std::borrow::Cow;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_engine::{
    EncodedQuad, Engine, EngineConfig, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId,
};
use nrese_exec::{Budget, BudgetExceeded, Envelope, POLL_UNITS, Reservation};
use nrese_rdf::{GraphNameRef, NamedNodeRef, Quad, QuadRef, Term, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ReadView, Resources};

use crate::graph::Selection;
use crate::report::{self, ValidationResult};
use crate::validate::{self as raw_validation, RawChunk, Validator};
use crate::{compile, validate};

const SHAPES: &str = "http://example.com/shapes";
const PREFIX: &str = "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .";

pub(crate) fn engine(data: &str, shapes: &str) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for (text, graph) in [
        (data, None),
        (shapes, Some(NamedNodeRef::new_unchecked(SHAPES))),
    ] {
        for quad in RdfParser::from_format(RdfFormat::Turtle)
            .for_slice(format!("{PREFIX}{text}").as_bytes())
        {
            let quad = quad.unwrap();
            tx.insert(QuadRef::new(
                &quad.subject,
                &quad.predicate,
                &quad.object,
                graph.map_or(GraphNameRef::DefaultGraph, Into::into),
            ));
        }
    }
    tx.commit().unwrap();
    engine
}
pub(crate) fn shapes(snapshot: &Snapshot) -> Selection {
    Selection::asserted(GraphSelector::Exact(
        snapshot
            .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
            .unwrap(),
    ))
}
const DATA: Selection = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
pub(crate) fn resources(limit: usize) -> Resources {
    Resources {
        memory: Envelope::new(Some(limit), None),
        ..Default::default()
    }
}

pub(crate) struct Probe<'a> {
    view: &'a Snapshot,
    scanned: AtomicUsize,
    decoded: AtomicUsize,
    class_scans: AtomicUsize,
    cancel_at: usize,
    pub(crate) cancel_decode_at: usize,
    pub(crate) token: CancellationToken,
}
impl<'a> Probe<'a> {
    pub(crate) fn new(view: &'a Snapshot) -> Self {
        Self {
            view,
            scanned: AtomicUsize::new(0),
            decoded: AtomicUsize::new(0),
            class_scans: AtomicUsize::new(0),
            cancel_at: usize::MAX,
            cancel_decode_at: usize::MAX,
            token: CancellationToken::new(),
        }
    }
}
impl<'v> ReadView for Probe<'v> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'v> {
        if pattern.predicate
            == self.view.lookup(
                NamedNodeRef::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf")
                    .into(),
            )
        {
            self.class_scans.fetch_add(1, Ordering::Relaxed);
        }
        self.view
            .quads_for_pattern_in(model, pattern)
            .inspect(move |_| {
                let n = self.scanned.fetch_add(1, Ordering::Relaxed) + 1;
                if n == self.cancel_at {
                    self.token.cancel();
                }
            })
    }
    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'v> {
        self.view.named_graphs()
    }
    fn contains_named_graph(&self, graph: TermId) -> bool {
        self.view.contains_named_graph(graph)
    }
    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.view.lookup(term)
    }
    fn decode(&self, _: TermId) -> Option<Term> {
        panic!("validation used unadmitted ordinary decode")
    }
    fn decode_within<B: Deref<Target = Budget>>(
        &self,
        id: TermId,
        held: &mut Reservation<B>,
    ) -> Result<Option<Term>, BudgetExceeded> {
        let term = <Snapshot as ReadView>::decode_within(self.view, id, held)?;
        if term.is_some() {
            let n = self.decoded.fetch_add(1, Ordering::Relaxed) + 1;
            if n == self.cancel_decode_at {
                self.token.cancel();
            }
        }
        Ok(term)
    }
    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.view.decode_quad(quad)
    }
    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self.view)
    }
    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot> {
        Cow::Borrowed(self.view)
    }
}

#[test]
fn standalone_applicable_short_circuits_without_building_validation_work() {
    let mut data = String::new();
    for n in 0..10000 {
        data.push_str(&format!("ex:a{n} ex:p ex:b ."));
    }
    let engine = engine(
        &data,
        "ex:S sh:targetSubjectsOf ex:p ; sh:nodeKind sh:IRI .",
    );
    let snapshot = engine.snapshot();
    // Enough for the two focus buffers and their growth overlap, but not an extra
    // 10,000-entry validation work list held alongside the finished focus vector.
    let resources = resources(224 * 1024);
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = resources.memory.used();
    assert!(crate::applicable(&Probe::new(&snapshot), &shapes, DATA, &resources).unwrap());
    assert_eq!(resources.memory.used(), baseline);
    println!(
        "standalone_applicable focus=10000 work_entries=0 limit={}",
        224 * 1024
    );
}

#[test]
fn raw_paths_borrow_shapes_and_decoded_report_holds_overlap() {
    let message = "m".repeat(4096);
    let engine = engine(
        "ex:a ex:p ex:b . ex:b ex:q 'value' .",
        &format!(
            "ex:S sh:targetNode ex:a ; sh:path (ex:p ex:q) ; sh:maxCount 0 ; sh:message '{message}' ."
        ),
    );
    let snapshot = engine.snapshot();
    let resources = resources(1024 * 1024);
    let memory = resources.memory.clone();
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = memory.used();
    let probe = Probe::new(&snapshot);
    let raw = raw_validation::validate_raw(&probe, &shapes, DATA, &resources).unwrap();
    assert_eq!(raw.len(), 1);
    let row = raw.results().next().unwrap();
    assert!(matches!(row.path, Some(Cow::Borrowed(_))));
    assert!(std::ptr::eq(
        row.path.as_deref().unwrap(),
        shapes.shapes[row.shape].path.as_ref().unwrap()
    ));
    let raw_bytes = memory.used() - baseline;
    let report = report::decode(&probe, &shapes, raw, &resources).unwrap();
    let decoded_bytes = memory.used() - baseline;
    println!(
        "raw_report rows={} raw_bytes={raw_bytes} decoded_bytes={decoded_bytes} baseline={baseline} charged_peak={}",
        report.results().len(),
        memory.peak()
    );
    assert!(report.applicable());
    assert_eq!(report.results().len(), 1);
    assert_eq!(report.results()[0].messages[0].value(), message);
    assert!(
        memory.peak() >= baseline + raw_bytes + decoded_bytes,
        "raw and complete decoded payload must overlap under admission"
    );
    drop(resources);
    drop(shapes);
    assert!(memory.used() >= message.len());
    drop(report);
    assert_eq!(memory.used(), 0);
}

#[test]
fn partial_decode_refusal_releases_both_phases() {
    let engine = engine(
        "ex:a ex:p 'short', 'another' .",
        &format!(
            "ex:S sh:targetNode ex:a ; sh:path ex:p ; sh:maxCount 0 ; sh:message 'small', '{}' .",
            "m".repeat(4096)
        ),
    );
    let snapshot = engine.snapshot();
    let resources = resources(1024 * 1024);
    let memory = resources.memory.clone();
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = memory.used();
    let probe = Probe::new(&snapshot);
    let raw = raw_validation::validate_raw(&probe, &shapes, DATA, &resources).unwrap();
    let available = raw.len() * size_of::<ValidationResult>() + 256;
    let mut blocker = Reservation::new(raw.budget.clone());
    blocker.charge(memory.remaining() - available).unwrap();
    probe.decoded.store(0, Ordering::Relaxed);
    let error = report::decode(&probe, &shapes, raw, &resources).unwrap_err();
    assert!(matches!(error, QueryEvaluationError::MemoryLimit(_)));
    println!(
        "partial_decode successful_terms={} retained_after_refusal={} baseline={baseline} blocker={}",
        probe.decoded.load(Ordering::Relaxed),
        memory.used(),
        blocker.bytes()
    );
    assert!(
        probe.decoded.load(Ordering::Relaxed) > 0,
        "refuse after decoding has started"
    );
    assert_eq!(memory.used(), baseline + blocker.bytes());
    drop(blocker);
    assert_eq!(memory.used(), baseline);
    drop(shapes);
    assert_eq!(memory.used(), 0);
}

#[test]
fn excluded_candidates_still_reach_the_shared_stop_checkpoint() {
    let mut data = String::new();
    for n in 0..10000 {
        data.push_str(&format!("ex:a{n} ex:p ex:b ."));
    }
    let engine = engine(
        &data,
        "ex:S sh:targetSubjectsOf ex:p ; sh:nodeKind sh:IRI .",
    );
    let snapshot = engine.snapshot();
    let mut resources = resources(1024 * 1024);
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = resources.memory.used();
    let mut probe = Probe::new(&snapshot);
    probe.cancel_at = 1;
    resources.cancellation = Some(probe.token.clone());
    let data = Selection::of(GraphSelector::Any).excluding(TermId::DEFAULT_GRAPH);
    assert!(matches!(
        raw_validation::validate_raw(&probe, &shapes, data, &resources),
        Err(QueryEvaluationError::Cancelled)
    ));
    assert!((1..=POLL_UNITS as usize).contains(&probe.scanned.load(Ordering::Relaxed)));
    println!(
        "excluded_scan candidates={} checkpoint={POLL_UNITS}",
        probe.scanned.load(Ordering::Relaxed)
    );
    assert_eq!(resources.memory.used(), baseline);
}

#[test]
fn nested_query_refusal_is_operational_and_releases_its_copies() {
    let mut data = String::new();
    for n in 0..10000 {
        data.push_str(&format!("ex:a{n} ex:p ex:b ."));
    }
    let engine = engine(
        &data,
        "ex:S sh:targetNode ex:a0 ; sh:sparql [ sh:select 'SELECT ?this WHERE { ?s ?p ?o }' ] .",
    );
    let snapshot = engine.snapshot();
    let resources = resources(16 * 1024);
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let baseline = resources.memory.used();
    let probe = Probe::new(&snapshot);
    assert!(matches!(
        validate(&probe, &shapes, DATA, &resources),
        Err(QueryEvaluationError::MemoryLimit(_))
    ));
    assert!(
        probe.decoded.load(Ordering::Relaxed) >= 3,
        "enter the bound nested query before refusal"
    );
    println!(
        "nested_refusal bound_decodes={} retained={} baseline={baseline}",
        probe.decoded.load(Ordering::Relaxed),
        resources.memory.used()
    );
    assert_eq!(resources.memory.used(), baseline);
}

#[test]
fn multiworker_partial_raw_refusal_joins_and_releases() {
    let mut data = String::new();
    for n in 0..1000 {
        data.push_str(&format!("ex:a{n} ex:p ex:b ."));
    }
    let engine = engine(
        &data,
        "ex:S sh:targetSubjectsOf ex:p ; sh:path ex:p ; sh:maxCount 0 .",
    );
    let snapshot = engine.snapshot();
    let resources = resources(1024 * 1024);
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let probe = Probe::new(&snapshot);
    let budget = std::sync::Arc::new(resources.memory.part());
    let validator = Validator::new(&probe, &shapes, DATA, &resources, budget.clone()).unwrap();
    let mut ticker = resources.stop().ticker().unwrap();
    let (work, applies) = validator.prepare(&mut ticker).unwrap();
    assert_eq!(work.len(), 1000);
    assert!(applies);
    let baseline = resources.memory.used();
    let mut blocker = Reservation::new(budget);
    let headroom = work.len().div_ceil(32) * size_of::<RawChunk<'_>>() + 2048;
    blocker
        .charge(resources.memory.remaining() - headroom)
        .unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    assert!(matches!(
        pool.install(|| validator.validate_all(&work)),
        Err(QueryEvaluationError::MemoryLimit(_))
    ));
    assert_eq!(
        resources.memory.used(),
        baseline + blocker.bytes(),
        "all workers and partially written chunks have been joined and freed"
    );
    println!(
        "parallel_refusal focus={} workers=4 retained={} baseline={baseline} blocker={}",
        work.len(),
        resources.memory.used(),
        blocker.bytes()
    );
}

#[test]
fn applicability_reuses_the_single_class_preparation() {
    let engine = engine(
        "ex:a a ex:C . ex:C rdfs:subClassOf ex:Super .",
        "ex:S sh:targetClass ex:Super ; sh:class ex:Super .",
    );
    let snapshot = engine.snapshot();
    let resources = resources(1024 * 1024);
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let probe = Probe::new(&snapshot);
    let report = validate(&probe, &shapes, DATA, &resources).unwrap();
    assert!(report.conforms());
    assert!(report.applicable());
    assert_eq!(
        probe.class_scans.load(Ordering::Relaxed),
        2,
        "Super and C are each expanded once, without a second applicability validator"
    );
    println!(
        "class_preparation scans={}",
        probe.class_scans.load(Ordering::Relaxed)
    );
}

#[test]
fn reported_failure_remains_a_charged_failure_not_an_operational_error() {
    let engine = engine(
        "",
        "ex:S sh:targetNode ex:a ; sh:sparql [ sh:select 'SELECT ?this ?failure WHERE { BIND(true AS ?failure) }' ] .",
    );
    let snapshot = engine.snapshot();
    let resources = resources(1024 * 1024);
    let memory = resources.memory.clone();
    let shapes = compile(&snapshot, shapes(&snapshot), &resources).unwrap();
    let report = validate(&Probe::new(&snapshot), &shapes, DATA, &resources).unwrap();
    assert!(!report.conforms());
    assert!(report.results().is_empty());
    assert_eq!(report.failures().len(), 1);
    assert!(report.failures()[0].ends_with("the query reported ?failure"));
    drop(shapes);
    drop(resources);
    assert!(memory.used() >= report.failures()[0].len());
    drop(report);
    assert_eq!(memory.used(), 0);
}
