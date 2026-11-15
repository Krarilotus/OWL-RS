//! What an answer over a ruleset's closure promises (docs/what-it-covers.md, "What an
//! answer promises"). The closure misses what its rules leave out, so its answers are
//! `sound-only` at best; under an operator that isn't monotone (`NOT EXISTS`, `MINUS`,
//! `LIMIT` after `ORDER BY`) a missing row can keep a row the certain answers lack, so
//! they are `unsound`. Each case below is such a row: the store returns it, and its
//! status, run or planned, must not call it certain.

use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, QlRewritingMode, SparqlQueryRequest,
    StatementOp, StatementsRequest, StoreConfig, StoreService,
};

use crate::dl::{PREFIXES, insert, pipeline};

/// eq-ref (OWL 2 Profiles §4.3, table 4), which the RL rules leave out: `:rex owl:sameAs
/// :rex` is entailed, so NOT EXISTS removes `:rex` and the certain answers are none.
const SAME_AS: (&str, &str) = (
    ":Dog rdfs:subClassOf :Animal . :rex a :Dog .",
    "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
);

/// A union as a superclass, outside OWL 2 RL and QL (OWL 2 Profiles §3.2, §4.2): with
/// `:Dog` disjoint from `:Cat`, `:rex a :Pet` is entailed, so MINUS removes `:rex`.
const UNION: (&str, &str) = (
    ":Dog rdfs:subClassOf [ owl:unionOf ( :Cat :Pet ) ] . :Dog owl:disjointWith :Cat . \
     :rex a :Dog .",
    "SELECT ?x { ?x a :Dog MINUS { ?x a :Pet } }",
);

/// The same axiom under a slice of an ordered answer: the certain answers are `:abe`
/// and `:rex`, so the first is `:abe`; the closure lacks it and returns `:rex`.
const ORDERED: (&str, &str) = (
    ":Dog rdfs:subClassOf [ owl:unionOf ( :Cat :Pet ) ] . :Dog owl:disjointWith :Cat . \
     :abe a :Dog . :rex a :Pet .",
    "SELECT ?x { ?x a :Pet } ORDER BY ?x LIMIT 1",
);

/// rdfs4a (RDF 1.1 Semantics §9.2.1), which `rdfs` and the rulesets on it leave out by
/// design (docs/spec/reasoning-semantics.md): `:rex a rdfs:Resource` isn't in their regime.
const RESOURCE: (&str, &str) = (
    ":rex a :Dog .",
    "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x a rdfs:Resource } }",
);

/// The axiomatic `rdf:_1 a rdf:Property` (RDF 1.1 Semantics §8.1.1), which `rdfs-full`
/// leaves out by design (the same page): not in its regime either.
const MEMBER: (&str, &str) = (
    ":rex a :Dog .",
    "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { rdf:_1 a rdf:Property } }",
);

/// The data of [`UNIVERSAL`]: `:Dog`, `:owns` and `:age` declared; `:Animal` and `:Pet`
/// used as classes undeclared; `:ann` typed by no class.
const DATA: &str = ":Dog a owl:Class . :Dog rdfs:subClassOf :Animal . \
    :owns a owl:ObjectProperty . :age a owl:DatatypeProperty . \
    :rex a :Dog , :Pet . :ann :owns :rex ; :age 3 .";

/// Statements OWL 2 entails for every term that neither a closure nor the QL rewriting
/// holds over [`DATA`]: the member, its read (`false` over the closure), and a query
/// whose NOT EXISTS or MINUS over it keeps the row named last, which the certain answers
/// lack. eq-ref; a named individual's `owl:Thing`; the top properties; a class's or
/// property's statements of itself, `owl:Thing`, the top and the bottom: scm-cls gives a
/// class's only to declared ones, and none gives a property's to the top or the bottom;
/// and its disjointness with the bottom, which no rule gives.
const UNIVERSAL: [(&str, &str, &str, &str); 12] = [
    (
        "eq-ref",
        "ASK { :rex owl:sameAs :rex }",
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
        "rex",
    ),
    (
        "owl:Thing",
        "ASK { :ann a owl:Thing }",
        "SELECT ?x { ?x :owns ?y FILTER NOT EXISTS { ?x a owl:Thing } }",
        "ann",
    ),
    (
        "owl:topObjectProperty",
        "ASK { :rex owl:topObjectProperty :ann }",
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:topObjectProperty :ann } }",
        "rex",
    ),
    (
        "owl:topDataProperty",
        "ASK { :rex owl:topDataProperty 3 }",
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:topDataProperty 3 } }",
        "rex",
    ),
    (
        "C subClassOf C",
        "ASK { :Animal rdfs:subClassOf :Animal }",
        "SELECT ?c { :Dog rdfs:subClassOf ?c FILTER NOT EXISTS { ?c rdfs:subClassOf ?c } }",
        "Animal",
    ),
    (
        "C equivalentClass C",
        "ASK { :Animal owl:equivalentClass :Animal }",
        "SELECT ?c { :Dog rdfs:subClassOf ?c FILTER NOT EXISTS { ?c owl:equivalentClass ?c } }",
        "Animal",
    ),
    (
        "C subClassOf owl:Thing",
        "ASK { :Animal rdfs:subClassOf owl:Thing }",
        "SELECT ?c { :Dog rdfs:subClassOf ?c MINUS { ?c rdfs:subClassOf owl:Thing } }",
        "Animal",
    ),
    (
        "owl:Nothing subClassOf C",
        "ASK { owl:Nothing rdfs:subClassOf :Pet }",
        "SELECT ?c { :rex a ?c MINUS { owl:Nothing rdfs:subClassOf ?c } }",
        "Pet",
    ),
    (
        "P subPropertyOf owl:topObjectProperty",
        "ASK { :owns rdfs:subPropertyOf owl:topObjectProperty }",
        "SELECT ?p { ?p a owl:ObjectProperty \
         MINUS { ?p rdfs:subPropertyOf owl:topObjectProperty } }",
        "owns",
    ),
    (
        "owl:bottomObjectProperty subPropertyOf P",
        "ASK { owl:bottomObjectProperty rdfs:subPropertyOf :owns }",
        "SELECT ?p { ?p a owl:ObjectProperty \
         MINUS { owl:bottomObjectProperty rdfs:subPropertyOf ?p } }",
        "owns",
    ),
    (
        "C disjointWith owl:Nothing",
        "ASK { :Animal owl:disjointWith owl:Nothing }",
        "SELECT ?c { :Dog rdfs:subClassOf ?c MINUS { ?c owl:disjointWith owl:Nothing } }",
        "Animal",
    ),
    (
        "owl:bottomObjectProperty propertyDisjointWith P",
        "ASK { owl:bottomObjectProperty owl:propertyDisjointWith :owns }",
        "SELECT ?p { ?p a owl:ObjectProperty \
         MINUS { owl:bottomObjectProperty owl:propertyDisjointWith ?p } }",
        "owns",
    ),
];

const RDF: &str = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn closure(data: &str, ruleset: Ruleset, ql: QlRewritingMode) -> StoreService {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("case.ttl");
    let prefixes = PREFIXES.replace("PREFIX", "@prefix").replace("> ", "> . ");
    std::fs::write(&file, format!("{prefixes}\n{data}")).unwrap();
    let store = StoreService::new(StoreConfig {
        ql_rewriting: ql,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: Some(nrese_store::InvalidStatements::Strict),
        })
        .unwrap();
    store.rematerialise(ruleset).unwrap();
    store
}

/// The rows of `query` and its statuses, as the answer and its plan give them.
fn answer(store: &StoreService, query: &str) -> (String, [&'static str; 2]) {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{RDF}{query}"));
    let result = store.execute_query(&request).expect("answered");
    let prepared = store.prepare_query(&request).expect("prepared");
    let planned = store
        .plan_query(&prepared, &CancellationToken::new())
        .expect("planned");
    let status = |c: Option<nrese_sparql::Completeness>| c.map_or("none", |c| c.as_str());
    (
        String::from_utf8(result.payload).unwrap(),
        [status(result.completeness), status(planned.completeness)],
    )
}

/// Each case's wrong row is returned and labelled `unsound`; what else happens, as text.
fn wrong(store: impl Fn(&str) -> StoreService, cases: &[(&str, &str)]) -> Vec<String> {
    let mut wrong = Vec::new();
    for &(data, query) in cases {
        let (rows, status) = answer(&store(data), query);
        if !rows.contains("example.com/rex") || status != ["unsound"; 2] {
            wrong.push(format!(
                "{query}: {status:?}, rex returned: {}",
                rows.contains("rex")
            ));
        }
    }
    wrong
}

#[test]
fn owl2_rl_by_default_labels_a_negated_missing_inference_unsound() {
    let store = |data: &str| closure(data, Ruleset::Owl2Rl, QlRewritingMode::Auto);
    let wrong = wrong(store, &[SAME_AS, UNION, ORDERED]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn owl2_rl_and_owl2_ql_without_the_rewriting_label_it_unsound() {
    let mut all = Vec::new();
    for ruleset in [Ruleset::Owl2Rl, Ruleset::Owl2Ql] {
        let store = |data: &str| closure(data, ruleset, QlRewritingMode::Off);
        for w in wrong(store, &[SAME_AS, UNION]) {
            all.push(format!("{}: {w}", ruleset.name()));
        }
    }
    assert!(all.is_empty(), "{all:#?}");
}

/// Under `rdfs` and `custom` (rdfs-full, rdfs-plus, owl-horst) the regime is the closure's
/// own (`nrese_sparql::Regime`): what the rules leave out by design isn't entailed, so a
/// NOT EXISTS over it removes nothing it should, and the answers stay `sound-only`.
#[test]
fn the_rdfs_rulesets_stay_sound_only_under_negation() {
    let mut wrong = Vec::new();
    for (ruleset, (data, query)) in [
        (Ruleset::Rdfs, RESOURCE),
        (Ruleset::RdfsPlus, RESOURCE),
        (Ruleset::OwlHorst, RESOURCE),
        (Ruleset::RdfsFull, MEMBER),
    ] {
        let (rows, status) = answer(&closure(data, ruleset, QlRewritingMode::Off), query);
        if !rows.contains("example.com/rex") || status != ["sound-only"; 2] {
            wrong.push(format!("{}: {query}: {status:?}", ruleset.name()));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each [`UNIVERSAL`] member in a store: its read is `false` and not `complete`, and
/// the query over it keeps its wrong row and is `unsound`.
fn universal_wrongs(store: &StoreService) -> Vec<String> {
    let mut wrong = Vec::new();
    for (member, read, negated, row) in UNIVERSAL {
        let (answer_, status) = answer(store, read);
        if !answer_.contains("false") || status.contains(&"complete") {
            wrong.push(format!("{member}: {read}: {status:?}"));
        }
        let (rows, status) = answer(store, negated);
        if !rows.contains(&format!("example.com/{row}")) || status != ["unsound"; 2] {
            wrong.push(format!("{member}: {negated}: {status:?}"));
        }
    }
    wrong
}

/// Where the QL rewriting runs (`owl2-ql` by default, `owl2-rl` with it on), it holds
/// none of [`UNIVERSAL`] either. Guards (hold on the base too): reads that no member
/// matches, between two distinct constants (`owl:Thing ⊑ :Dog` isn't entailed), and a
/// negation over what the closure is complete for, stay `complete`.
#[test]
fn the_rewriting_path_doesnt_call_a_universal_statement_complete() {
    let mut all = Vec::new();
    for (ruleset, ql) in [
        (Ruleset::Owl2Ql, QlRewritingMode::Auto),
        (Ruleset::Owl2Rl, QlRewritingMode::On),
    ] {
        let store = closure(DATA, ruleset, ql);
        for w in universal_wrongs(&store) {
            all.push(format!("{} {ql:?}: {w}", ruleset.name()));
        }
        for guard in [
            "ASK { :Dog rdfs:subClassOf :Animal }",
            "ASK { owl:Thing rdfs:subClassOf :Dog }",
            "SELECT ?x { ?x a :Animal FILTER NOT EXISTS { ?x a :Dog } }",
        ] {
            let status = answer(&store, guard).1;
            if status != ["complete"; 2] {
                all.push(format!(
                    "{} {ql:?}: guard {guard}: {status:?}",
                    ruleset.name()
                ));
            }
        }
    }
    assert!(all.is_empty(), "{all:#?}");
}

#[test]
fn the_ruleset_path_doesnt_call_a_universal_statement_complete() {
    let mut all = Vec::new();
    for (ruleset, ql) in [
        (Ruleset::Owl2Rl, QlRewritingMode::Auto),
        (Ruleset::Owl2Ql, QlRewritingMode::Off),
    ] {
        for w in universal_wrongs(&closure(DATA, ruleset, ql)) {
            all.push(format!("{} {ql:?}: {w}", ruleset.name()));
        }
    }
    assert!(all.is_empty(), "{all:#?}");
}

/// Under `owl2-dl` on the RL route ([`DATA`] is in OWL 2 RL), `x a owl:Thing` is in
/// neither bound.
#[test]
fn owl2_dl_doesnt_call_a_universal_statement_complete() {
    let dl = pipeline();
    insert(&dl, DATA).expect("data");
    let wrong = universal_wrongs(dl.store());
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Off the RL route (an existential superclass is outside OWL 2 RL), the upper bound
/// holds `:ann a owl:Thing` and the bounds prove it: the answer is `complete`, and
/// `dl-answers=exact` gives it.
#[test]
fn owl2_dl_off_the_rl_route_proves_an_owl_thing_read_complete() {
    let dl = pipeline();
    let existential = ":Dog rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :owns ; \
                       owl:someValuesFrom :Dog ] .";
    insert(&dl, &format!("{DATA} {existential}")).expect("data");
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ :ann a owl:Thing }}"));
    request.dl_answers = Some(nrese_store::DlAnswers::Exact);
    let result = dl.store().execute_query(&request).expect("proved");
    assert!(String::from_utf8(result.payload).unwrap().contains("true"));
    assert_eq!(result.completeness.map(|c| c.as_str()), Some("complete"));
}

/// DESCRIBE reads every predicate of what it names, `:ann owl:sameAs :ann` and its
/// top-property pairs among them, which no store holds: never `complete`, run or planned.
#[test]
fn a_describe_is_never_complete() {
    let dl = pipeline();
    insert(&dl, DATA).expect("data");
    let mut wrong = Vec::new();
    for (name, store) in [
        (
            "owl2-ql",
            &closure(DATA, Ruleset::Owl2Ql, QlRewritingMode::Auto),
        ),
        (
            "owl2-rl with the rewriting",
            &closure(DATA, Ruleset::Owl2Rl, QlRewritingMode::On),
        ),
        (
            "owl2-rl",
            &closure(DATA, Ruleset::Owl2Rl, QlRewritingMode::Auto),
        ),
        ("owl2-dl", dl.store().as_ref()),
    ] {
        let (graph, status) = answer(store, "DESCRIBE :ann");
        if !graph.contains("example.com/owns") || status.contains(&"complete") {
            wrong.push(format!("{name}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Under `owl2-dl`, a read of the inferred statements alone reads the rules' closure
/// without the bounds: `:rex a :Animal` is there, `:rex owl:sameAs :rex` isn't.
#[test]
fn owl2_dl_reading_the_inferred_statements_alone_labels_it_unsound() {
    let dl = pipeline();
    insert(&dl, SAME_AS.0).expect("data");
    let query = "SELECT ?x FROM <http://www.ontotext.com/implicit> \
                 { ?x a :Animal FILTER NOT EXISTS { ?x owl:sameAs ?x } }";
    let (rows, status) = answer(dl.store(), query);
    assert!(rows.contains("example.com/rex"), "{rows}");
    assert_eq!(status, ["unsound"; 2]);
}

/// Under `owl2-dl`, a read inside a transaction with additions pending reads the
/// committed closure without the bounds.
#[test]
fn owl2_dl_reading_inside_a_transaction_labels_it_unsound() {
    let dl = pipeline();
    insert(&dl, SAME_AS.0).expect("data");
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{}", SAME_AS.1)))
        .expect("prepared");
    let add = StatementOp::Add {
        data: nrese_store::RdfPayload {
            payload: b"<http://example.com/tom> <http://example.com/p> <http://example.com/o> ."
                .to_vec()
                .into(),
            format: nrese_store::GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    };
    let (mut out, mut status) = (Vec::new(), None);
    store
        .run_query_pending_reporting(
            &StatementsRequest::new(vec![add]),
            &prepared,
            &CancellationToken::new(),
            &mut out,
            |s| status = s,
        )
        .expect("answered");
    assert!(String::from_utf8(out).unwrap().contains("example.com/rex"));
    assert_eq!(status.map(|s| s.as_str()), Some("unsound"));
}

/// Guards (hold on the base too): what can only miss rows stays `sound-only`. A monotone
/// read, and LIMIT, OFFSET and GRAPH over one: each row they return is one of the
/// closure's answers, which are entailed.
#[test]
fn a_read_that_can_only_miss_rows_stays_sound_only() {
    let store = closure(UNION.0, Ruleset::Owl2Rl, QlRewritingMode::Auto);
    for query in [
        "SELECT ?x { ?x a :Dog }",
        "SELECT ?x { ?x a :Dog } LIMIT 1 OFFSET 0",
        "SELECT ?x { GRAPH ?g { ?x a :Dog } }",
    ] {
        assert_eq!(answer(&store, query).1, ["sound-only"; 2], "{query}");
    }
}
