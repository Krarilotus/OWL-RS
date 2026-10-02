//! Equality by representatives kept in the store (`reasoner.equality = "compact"`, work
//! package W4 stage B) answers as the replicated closure: the same statements, asserted
//! and inferred, and the same query answers, after every commit: facts about identities,
//! named graphs, deletions, classes that merge and split, equalities the rules derive.

mod support;

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest, StatementPattern,
    StoreConfig, StoreService,
};
use support::in_memory_store_config;

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Replicate,
    Representatives,
    Compact,
}

fn pipeline(mode: Mode) -> MutationPipeline {
    let config = StoreConfig {
        equality_by_representatives: mode != Mode::Replicate,
        equality_compact: mode == Mode::Compact,
        ..in_memory_store_config()
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{text}: {error}"));
}

fn statements(pipeline: &MutationPipeline, infer: bool) -> Vec<String> {
    let mut quads: Vec<String> = pipeline
        .store()
        .read_statements(&StatementPattern::default(), infer)
        .expect("statements")
        .iter()
        .map(ToString::to_string)
        .collect();
    quads.sort();
    quads
}

fn answer(pipeline: &MutationPipeline, query: &str) -> String {
    let result = pipeline
        .store()
        .execute_query_str(&format!("{PREFIXES}{query}"))
        .unwrap_or_else(|error| panic!("{query}: {error}"));
    String::from_utf8(result.payload).expect("utf-8")
}

/// Queries over every shape the expansion meets: full scans, constants that are other
/// identities, joins through classes, repeated variables, counts, groups, `sameAs`
/// itself, named graphs and the merged view.
const QUERIES: &[&str] = &[
    "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o",
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
    "SELECT ?o WHERE { ex:d ex:knows ?o } ORDER BY ?o",
    "SELECT ?s WHERE { ?s ex:knows ex:kim } ORDER BY ?s",
    "SELECT ?x WHERE { ?x owl:sameAs ex:b } ORDER BY ?x",
    "SELECT (COUNT(*) AS ?n) WHERE { ?x owl:sameAs ?y }",
    "SELECT ?x ?m WHERE { ?x ex:knows ?y . ?y ex:hasMother ?m } ORDER BY ?x ?m",
    "ASK { ex:c ex:meets ex:kim }",
    "SELECT ?t (COUNT(?x) AS ?n) WHERE { ?x a ?t } GROUP BY ?t ORDER BY ?t",
    "SELECT DISTINCT ?x WHERE { ?x a ex:Agent } ORDER BY ?x",
    "SELECT ?x WHERE { ?x ?p ?x } ORDER BY ?x",
    "SELECT ?x (STR(?x) AS ?name) WHERE { ?x ex:livesIn ?c } ORDER BY ?x",
    "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s ex:likes ?o } } ORDER BY ?g ?s ?o",
    "SELECT ?s ?o WHERE { ?s ex:likes ?o } ORDER BY ?s ?o",
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ex:likes ?o }",
    "SELECT ?s WHERE { ?s ex:knows ?o FILTER(?s = ex:a) } ORDER BY ?s",
    "SELECT ?s ?o WHERE { ?s ex:knows ?o OPTIONAL { ?o ex:age ?a } } ORDER BY ?s ?o",
];

fn observe(pipeline: &MutationPipeline) -> Vec<String> {
    let mut seen = vec![
        statements(pipeline, true).join("\n"),
        statements(pipeline, false).join("\n"),
    ];
    seen.extend(QUERIES.iter().map(|query| answer(pipeline, query)));
    seen
}

#[test]
fn compact_equality_answers_as_replication() {
    let modes = [Mode::Replicate, Mode::Representatives, Mode::Compact];
    let pipelines = modes.map(pipeline);
    let steps = [
        "INSERT DATA {
           ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c . ex:d owl:sameAs ex:a .
           ex:hasMother a owl:FunctionalProperty . ex:email a owl:InverseFunctionalProperty .
           ex:kim ex:hasMother ex:m1 , ex:m2 . ex:p1 ex:email \"x@y\" . ex:p2 ex:email \"x@y\" .
           ex:a ex:knows ex:kim . ex:p1 ex:age 30 . ex:c a ex:Person .
           ex:Person rdfs:subClassOf ex:Agent .
           ex:knows owl:sameAs ex:meets . ex:meets a owl:SymmetricProperty .
           ex:m2 ex:livesIn ex:berlin . ex:berlin owl:sameAs ex:berlinCity .
           ex:p2 ex:knows ex:p2 }",
        // A fact about an identity: no class changes.
        "INSERT DATA { ex:d ex:knows ex:x . ex:x ex:age 7 }",
        // Named graphs: a fact asserted there is not in the default graph, its copies are.
        "INSERT DATA { GRAPH ex:g { ex:a ex:likes ex:y . ex:z ex:likes ex:y } }",
        "DELETE DATA { ex:d ex:knows ex:x }",
        // A merge, then a split.
        "INSERT DATA { ex:e owl:sameAs ex:x }",
        "INSERT DATA { ex:x owl:sameAs ex:a }",
        "DELETE DATA { ex:b owl:sameAs ex:c }",
        // An equality the functional property derives.
        "INSERT DATA { ex:kim ex:hasMother ex:m3 }",
        "DELETE WHERE { ex:c ?p ?o }",
        // The same fact in the default graph and a named graph.
        "INSERT DATA { ex:b ex:likes ex:y . GRAPH ex:g { ex:b ex:likes ex:y } }",
    ];
    for step in steps {
        for pipeline in &pipelines {
            update(pipeline, step);
        }
        let reference = observe(&pipelines[0]);
        for (mode, pipeline) in modes.iter().zip(&pipelines).skip(1) {
            let seen = observe(pipeline);
            for (i, (got, want)) in seen.iter().zip(&reference).enumerate() {
                assert_eq!(got, want, "{mode:?}, observation {i}, after: {step}");
            }
        }
    }
    // The compact store keeps fewer statements than it shows.
    let compact = &pipelines[2];
    let stored = compact.store().engine_stats().inferred;
    let shown = statements(compact, true).len() - statements(compact, false).len();
    assert!(stored < shown as u64, "stored {stored}, shown {shown}");
}

/// A compact store on disk reopens with its closure current and reads it expanded; under
/// another equality mode the closure is not current (it has another form).
#[test]
fn compact_equality_survives_a_restart() {
    let dir = tempfile::tempdir().expect("dir");
    let config = |compact: bool| StoreConfig {
        equality_compact: compact,
        ..StoreConfig::on_disk(dir.path())
    };
    let open = |compact: bool| {
        MutationPipeline::new(
            Arc::new(StoreService::new(config(compact)).expect("store")),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        )
    };
    let before = {
        let pipeline = open(true);
        update(
            &pipeline,
            "INSERT DATA { ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c . ex:a ex:knows ex:kim .
                           ex:c a ex:Person . ex:Person rdfs:subClassOf ex:Agent }",
        );
        observe(&pipeline)
    };
    let pipeline = open(true);
    assert!(
        pipeline
            .store()
            .reasoning_is_current(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
    );
    assert_eq!(observe(&pipeline), before);
    drop(pipeline);
    let other = open(false);
    assert!(
        !other
            .store()
            .reasoning_is_current(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
    );
}

/// A small deterministic generator (the tests need no randomness crate).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

/// Random data with equality (asserted `sameAs`, a functional and an inverse-functional
/// property, a `sameAs` between properties, named graphs), random commits, and queries of
/// every shape over random constants: the compact store answers as the replicated one.
#[test]
fn compact_equality_answers_as_replication_on_random_data() {
    for seed in 1..=12u64 {
        let mut random = Lcg(seed);
        let entity = |i: usize| format!("ex:e{i}");
        let predicates = ["ex:p", "ex:q", "ex:r", "ex:f", "ex:i"];
        let fact = |random: &mut Lcg| {
            format!(
                "{} {} {} .",
                entity(random.below(12)),
                predicates[random.below(predicates.len())],
                entity(random.below(12))
            )
        };
        let mut data = String::from(
            "ex:f a owl:FunctionalProperty . ex:i a owl:InverseFunctionalProperty .
             ex:q rdfs:subPropertyOf ex:p . ex:e0 a ex:C . ex:C rdfs:subClassOf ex:D . ",
        );
        for _ in 0..20 {
            data.push_str(&fact(&mut random));
        }
        for _ in 0..random.below(5) {
            data.push_str(&format!(
                "{} owl:sameAs {} .",
                entity(random.below(12)),
                entity(random.below(12))
            ));
        }
        if random.below(3) == 0 {
            data.push_str("ex:r owl:sameAs ex:p .");
        }
        let named = format!(
            "GRAPH ex:g {{ {} {} }}",
            fact(&mut random),
            fact(&mut random)
        );
        let commits = [
            format!("INSERT DATA {{ {data} {named} }}"),
            format!(
                "INSERT DATA {{ {} {} }}",
                fact(&mut random),
                fact(&mut random)
            ),
            format!(
                "INSERT DATA {{ {} owl:sameAs {} }}",
                entity(random.below(12)),
                entity(random.below(12))
            ),
            format!("DELETE WHERE {{ {} ?p ?o }}", entity(random.below(12))),
            format!("DELETE DATA {{ {} }}", fact(&mut random)),
        ];
        let pipelines = [Mode::Replicate, Mode::Compact].map(pipeline);
        for commit in &commits {
            let results: Vec<_> = pipelines
                .iter()
                .map(|pipeline| {
                    pipeline.apply(
                        MutationCommand::Update(SparqlUpdateRequest::new(format!(
                            "{PREFIXES}{commit}"
                        ))),
                        &MutationTicket::new(),
                    )
                })
                .collect();
            // Both reject (an inconsistency) or both commit.
            assert_eq!(
                results[0].is_ok(),
                results[1].is_ok(),
                "seed {seed}: {commit}: {:?}",
                results
                    .iter()
                    .map(|r| r.as_ref().err().map(ToString::to_string))
                    .collect::<Vec<_>>()
            );
            let (a, b) = (entity(random.below(12)), entity(random.below(12)));
            let p = predicates[random.below(predicates.len())];
            let queries = [
                "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o".to_owned(),
                format!("SELECT ?o WHERE {{ {a} {p} ?o }} ORDER BY ?o"),
                format!("SELECT ?s WHERE {{ ?s {p} {b} }} ORDER BY ?s"),
                format!("SELECT ?p WHERE {{ {a} ?p {b} }} ORDER BY ?p"),
                format!("ASK {{ {a} {p} {b} }}"),
                format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s {p} ?o }}"),
                format!("SELECT (COUNT(*) AS ?n) WHERE {{ {a} ?p ?o }}"),
                format!("SELECT ?x ?z WHERE {{ ?x {p} ?y . ?y ex:q ?z }} ORDER BY ?x ?z"),
                "SELECT ?x ?y WHERE { ?x ex:p ?y . ?y ex:p ?x } ORDER BY ?x ?y".to_owned(),
                "SELECT ?x WHERE { ?x ?p ?x } ORDER BY ?x".to_owned(),
                format!("SELECT ?x WHERE {{ ?x owl:sameAs {a} }} ORDER BY ?x"),
                "SELECT ?y (COUNT(?x) AS ?n) WHERE { ?x ex:p ?y } GROUP BY ?y ORDER BY ?y"
                    .to_owned(),
                "SELECT DISTINCT ?x WHERE { ?x a ex:D } ORDER BY ?x".to_owned(),
                format!("SELECT ?g ?o WHERE {{ GRAPH ?g {{ {a} ?p ?o }} }} ORDER BY ?g ?o"),
                format!("SELECT ?s ?o WHERE {{ ?s {p} ?o FILTER(?s != {a}) }} ORDER BY ?s ?o"),
                format!(
                    "SELECT ?s ?o WHERE {{ ?s ex:p ?o MINUS {{ ?s ex:q ?o }} }} ORDER BY ?s ?o"
                ),
            ];
            for query in &queries {
                let want = answer(&pipelines[0], query);
                let got = answer(&pipelines[1], query);
                assert_eq!(got, want, "seed {seed}, after {commit}: {query}");
            }
            assert_eq!(
                statements(&pipelines[1], false),
                statements(&pipelines[0], false),
                "seed {seed}, asserted, after {commit}"
            );
        }
    }
}
