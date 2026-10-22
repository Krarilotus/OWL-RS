//! Regression for the small acyclic RDF graph whose repeated references expand a tree.

use super::*;
use nrese_engine::{EncodedQuad, QuadPattern};
use nrese_exec::{Budget, BudgetExceeded, Envelope, Reservation};
use nrese_rdf::{Quad, Term};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ReadView, Resources};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repeated_path(depth: usize) -> String {
    let mut text = String::new();
    for level in 1..=depth {
        let previous = if level == 1 {
            "ex:p".to_owned()
        } else {
            format!("_:p{}", level - 1)
        };
        text.push_str(&format!(
            "_:p{level} rdf:first {previous} ; rdf:rest [ rdf:first {previous} ; rdf:rest rdf:nil ] .\n"
        ));
    }
    text.push_str(&format!(
        "ex:S sh:targetNode ex:a ; sh:path _:p{depth} ; sh:minCount 1 ."
    ));
    text
}

/// Counts actual path-list reads; a stop is raised after entry, during expansion.
/// All dictionary/view operations retain the real Snapshot's ordinary contracts.
struct Observed<'a> {
    snapshot: &'a Snapshot,
    first: TermId,
    reads: AtomicUsize,
    cancel_at: usize,
    token: CancellationToken,
}

impl<'v> ReadView for Observed<'v> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'v> {
        let count = pattern.subject.is_some() && pattern.predicate == Some(self.first);
        self.snapshot
            .quads_for_pattern_in(model, pattern)
            .inspect(move |_| {
                if count && self.reads.fetch_add(1, Ordering::Relaxed) + 1 == self.cancel_at {
                    self.token.cancel();
                }
            })
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'v> {
        self.snapshot.named_graphs()
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        self.snapshot.contains_named_graph(graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.snapshot.lookup(term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        self.snapshot.decode(id)
    }

    fn decode_within<B: std::ops::Deref<Target = Budget>>(
        &self,
        id: TermId,
        held: &mut Reservation<B>,
    ) -> Result<Option<Term>, BudgetExceeded> {
        self.snapshot.decode_within(id, held)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.snapshot.decode_quad(quad)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self.snapshot)
    }

    fn evaluation_snapshot(&self) -> Cow<'_, Snapshot> {
        Cow::Borrowed(self.snapshot)
    }
}

fn observed(snapshot: &Snapshot, cancel_at: usize) -> Observed<'_> {
    Observed {
        snapshot,
        first: id(snapshot, "http://www.w3.org/1999/02/22-rdf-syntax-ns#first"),
        reads: AtomicUsize::new(0),
        cancel_at,
        token: CancellationToken::new(),
    }
}

#[test]
fn repeated_path_refuses_before_expansion_and_retains_success_until_drop() {
    // Depth ten is safe even on the old code: 40 input list quads, 1,024 leaves.
    // An 8 KiB envelope cannot own the output. Never execute the depth-27 witness.
    let repeated = engine("", &repeated_path(10));
    let snapshot = repeated.snapshot();
    let view = observed(&snapshot, usize::MAX);
    let memory = Envelope::new(Some(8 * 1024), None);
    let resources = Resources {
        memory: memory.clone(),
        ..Default::default()
    };
    let outcome = compile(&view, shapes_selection(&snapshot), &resources);
    let reads = view.reads.load(Ordering::Relaxed);
    println!("path refusal: first_reads={reads}, peak={}", memory.peak());
    assert!(
        matches!(
            outcome,
            Err(nrese_shacl::CompileError::Evaluation(
                QueryEvaluationError::MemoryLimit(_)
            ))
        ),
        "path output must be admitted before it escapes: {:?}",
        outcome.as_ref().map(Shapes::len)
    );
    assert!(reads < 3 * (1024 - 1), "refuse before full expansion");
    assert!(memory.peak() <= 8 * 1024);
    assert_eq!(memory.used(), 0);

    let small = engine("", &repeated_path(5));
    let snapshot = small.snapshot();
    let memory = Envelope::new(Some(64 * 1024), None);
    let resources = Resources {
        memory: memory.clone(),
        ..Default::default()
    };
    let shapes = compile(&snapshot, shapes_selection(&snapshot), &resources).unwrap();
    drop(resources);
    println!(
        "path success: held={}, peak={}",
        memory.used(),
        memory.peak()
    );
    assert!(memory.used() >= 31 * 2 * size_of::<usize>());
    let report = validate(&snapshot, &shapes, DEFAULT);
    assert_eq!(report.results.len(), 1);
    assert!(report.failures.is_empty());
    drop(shapes);
    assert_eq!(memory.used(), 0);

    // The error payload outlives compilation, with the original cycle diagnostic.
    let cyclic = engine(
        "",
        "ex:S sh:targetNode ex:a ; sh:path _:p . _:p rdf:first ex:p ; rdf:rest _:p .",
    );
    let snapshot = cyclic.snapshot();
    let resources = Resources {
        memory: memory.clone(),
        ..Default::default()
    };
    let error = compile(&snapshot, shapes_selection(&snapshot), &resources).unwrap_err();
    assert_eq!(
        error.to_string(),
        "shape <http://example.com/S>: sh:path is a cyclic list"
    );
    drop(resources);
    assert!(memory.used() > 0);
    drop(error);
    assert_eq!(memory.used(), 0);
}

#[test]
fn repeated_path_cancellation_interrupts_interior_expansion() {
    let engine = engine("", &repeated_path(10));
    let snapshot = engine.snapshot();
    let view = observed(&snapshot, 64);
    let memory = Envelope::new(Some(1024 * 1024), None);
    let resources = Resources {
        memory: memory.clone(),
        cancellation: Some(view.token.clone()),
        ..Default::default()
    };
    let outcome = compile(&view, shapes_selection(&snapshot), &resources);
    let reads = view.reads.load(Ordering::Relaxed);
    println!(
        "path cancellation: first_reads={reads}, peak={}",
        memory.peak()
    );
    assert!(matches!(
        outcome,
        Err(nrese_shacl::CompileError::Evaluation(
            QueryEvaluationError::Cancelled
        ))
    ));
    // The old compiler only notices after all 3,069 first reads have finished.
    assert!(
        (64..2048).contains(&reads),
        "stop inside expansion, not at compile exit: {reads}"
    );
    assert_eq!(memory.used(), 0);
}
