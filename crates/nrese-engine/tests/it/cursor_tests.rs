//! Probe cursors ([`nrese_engine::ProbeCursor`]) read what the snapshot's scans read, in
//! any order of patterns, over stacks of several runs with deletions, in both layouts;
//! their leapfrog step names the next value a brute force finds; and patterns probed in
//! ascending order search each run from the root once.

use nrese_engine::{
    EncodedQuad, EncodedTriple, Engine, EngineConfig, GraphSelector, QuadPattern, ReadModel, Seek,
    Snapshot, TermId,
};
use nrese_rdf::NamedNode;

const TERMS: u64 = 24;

const MODELS: [ReadModel; 3] = [
    ReadModel::Materialised,
    ReadModel::Asserted,
    ReadModel::Inferred,
];

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

/// An engine of several commits (one run each, no compaction): asserted quads (in a
/// named graph too with `named`), inferred triples, and deletions of both.
fn store(seed: u64, named: bool) -> (Engine, Vec<TermId>) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine");
    let ids: Vec<TermId> = {
        let tx = engine.transaction();
        (0..TERMS)
            .map(|n| {
                let iri = NamedNode::new_unchecked(format!("http://example.com/{n}"));
                tx.intern(iri.as_ref().into())
            })
            .collect()
    };
    let mut rng = Rng(seed);
    let mut asserted: Vec<EncodedQuad> = Vec::new();
    let mut inferred: Vec<EncodedTriple> = Vec::new();
    for _ in 0..6 {
        let mut tx = engine.transaction();
        for _ in 0..150 {
            let term = |rng: &mut Rng, n| ids[rng.below(n) as usize];
            let triple = EncodedTriple::new(
                term(&mut rng, TERMS),
                term(&mut rng, 4),
                term(&mut rng, TERMS),
            );
            match rng.below(10) {
                0..=4 => {
                    let quad = if named && rng.below(4) == 0 {
                        EncodedQuad::new(
                            triple.subject,
                            triple.predicate,
                            triple.object,
                            ids[TERMS as usize - 1],
                        )
                    } else {
                        triple.in_default_graph()
                    };
                    if tx.insert_encoded(quad) {
                        asserted.push(quad);
                    }
                }
                5..=7 => {
                    if tx.insert_inferred(triple) {
                        inferred.push(triple);
                    }
                }
                8 if !asserted.is_empty() => {
                    let quad = asserted.swap_remove(rng.below(asserted.len() as u64) as usize);
                    tx.remove_encoded(quad);
                }
                _ if !inferred.is_empty() => {
                    let triple = inferred.swap_remove(rng.below(inferred.len() as u64) as usize);
                    tx.remove_inferred(triple);
                }
                _ => {}
            }
        }
        tx.commit().expect("commit");
    }
    (engine, ids)
}

/// A random pattern of a shape: `bound[c]` says whether component c is bound.
fn pattern(rng: &mut Rng, ids: &[TermId], bound: [bool; 3], graph: GraphSelector) -> QuadPattern {
    let mut value =
        |c: usize| bound[c].then(|| ids[rng.below(if c == 1 { 4 } else { TERMS }) as usize]);
    QuadPattern {
        subject: value(0),
        predicate: value(1),
        object: value(2),
        graph,
    }
}

fn graphs(ids: &[TermId]) -> [GraphSelector; 3] {
    [
        GraphSelector::Any,
        GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        GraphSelector::Exact(ids[TERMS as usize - 1]),
    ]
}

fn component(quad: &EncodedQuad, c: usize) -> TermId {
    [quad.subject, quad.predicate, quad.object][c]
}

fn set(pattern: &mut QuadPattern, c: usize, value: Option<TermId>) {
    match c {
        0 => pattern.subject = value,
        1 => pattern.predicate = value,
        _ => pattern.object = value,
    }
}

/// The leapfrog step by brute force: found, the next value, or none.
fn brute_seek(snapshot: &Snapshot, model: ReadModel, pattern: &QuadPattern, c: usize) -> Seek {
    if snapshot.exists_in(model, pattern) {
        return Seek::Found;
    }
    let asked = component_of(pattern, c).expect("bound");
    let mut open = *pattern;
    set(&mut open, c, None);
    snapshot
        .quads_for_pattern_in(model, &open)
        .map(|quad| component(&quad, c).raw())
        .filter(|&v| v > asked.raw())
        .min()
        .map_or(Seek::Exhausted, Seek::Next)
}

fn component_of(pattern: &QuadPattern, c: usize) -> Option<TermId> {
    [pattern.subject, pattern.predicate, pattern.object][c]
}

#[test]
fn probe_cursors_read_what_scans_read() {
    for (seed, named) in [(1, false), (2, true), (3, true)] {
        let (engine, ids) = store(seed, named);
        let snapshot = engine.snapshot();
        let mut rng = Rng(seed * 7);
        for model in MODELS {
            for graph in graphs(&ids) {
                for shape in 0..8u8 {
                    let bound = [shape & 1 != 0, shape & 2 != 0, shape & 4 != 0];
                    let mut cursor = snapshot.probe_cursor(model);
                    let mut patterns: Vec<QuadPattern> = (0..40)
                        .map(|_| pattern(&mut rng, &ids, bound, graph))
                        .collect();
                    // Half the sequences ascending (the fast case), half in any order.
                    if rng.below(2) == 0 {
                        patterns.sort_by_key(|p| {
                            [p.subject, p.predicate, p.object].map(|t| t.map(TermId::raw))
                        });
                    }
                    for p in &patterns {
                        let expected: Vec<EncodedQuad> =
                            snapshot.quads_for_pattern_in(model, p).collect();
                        let mut got = Vec::new();
                        cursor.for_each(p, |quad| got.push(quad));
                        assert_eq!(got, expected, "{model:?} {p:?}");
                        assert_eq!(cursor.exists(p), !expected.is_empty(), "{model:?} {p:?}");
                        assert_eq!(
                            cursor.count(p),
                            snapshot.estimate_in(model, p),
                            "{model:?} {p:?}"
                        );
                        // With one position free, its values in order.
                        if let (GraphSelector::Exact(_), [free]) = (
                            p.graph,
                            &(0..3).filter(|&c| !bound[c]).collect::<Vec<_>>()[..],
                        ) {
                            let mut values = Vec::new();
                            assert!(cursor.values(p, *free, &mut values), "{p:?}");
                            let mut want: Vec<u64> =
                                expected.iter().map(|q| component(q, *free).raw()).collect();
                            want.sort_unstable();
                            assert_eq!(values, want, "{model:?} {p:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn leapfrog_seeks_name_the_next_value() {
    let mut skips = 0;
    for (seed, named) in [(4, false), (5, true)] {
        let (engine, ids) = store(seed, named);
        let snapshot = engine.snapshot();
        let mut rng = Rng(seed * 11);
        for model in MODELS {
            for graph in graphs(&ids) {
                for shape in 1..8u8 {
                    let bound = [shape & 1 != 0, shape & 2 != 0, shape & 4 != 0];
                    for c in (0..3).filter(|&c| bound[c]) {
                        let mut cursor = snapshot.probe_cursor(model);
                        let base = pattern(&mut rng, &ids, bound, graph);
                        // The values of `c` in ascending order, the rest fixed: a
                        // worst-case-optimal join's candidates.
                        for &value in &ids {
                            let mut p = base;
                            set(&mut p, c, Some(value));
                            let expected = brute_seek(&snapshot, model, &p, c);
                            match cursor.seek(&p, c) {
                                Seek::Missing => assert!(
                                    !matches!(expected, Seek::Found),
                                    "{model:?} {p:?} at {c}"
                                ),
                                got => {
                                    skips += u32::from(matches!(got, Seek::Next(_)));
                                    assert_eq!(got, expected, "{model:?} {p:?} at {c}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(skips > 100, "{skips} skips");
}

/// Probes in ascending order search each run from the root once, then gallop.
#[test]
fn ascending_probes_search_each_run_from_the_root_once() {
    let (engine, ids) = store(9, false);
    let snapshot = engine.snapshot();
    let mut cursor = snapshot.probe_cursor(ReadModel::Materialised);
    let p = ids[1];
    for &s in &ids {
        let pattern = QuadPattern {
            subject: Some(s),
            predicate: Some(p),
            object: None,
            graph: GraphSelector::Any,
        };
        cursor.for_each(&pattern, |_| {});
    }
    let stats = cursor.stats();
    // Six commits of both stacks: at most one run per commit per stack.
    assert!(stats.root_searches <= 12, "{stats:?}");
    assert!(stats.seeks >= TERMS, "{stats:?}");
}
