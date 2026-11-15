//! What an answer over a ruleset's closure promises (docs/what-it-covers.md, "What an
//! answer promises"). The closure misses what its rules leave out, so its answers are
//! `sound-only` at best; under an operator that isn't monotone (`NOT EXISTS`, `MINUS`,
//! `LIMIT` after `ORDER BY`) a missing row can keep a row the certain answers lack, so
//! they are `unsound`. Each case below is such a row: the store returns it, and its
//! status, run or planned, must not call it certain.

use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{
    BulkLoadRequest, CancellationToken, DlConfig, GraphTarget, QlRewritingMode, SparqlQueryRequest,
    StatementOp, StatementsRequest, StoreConfig, StoreService,
};

use crate::dl::{PREFIXES, insert, pipeline, pipeline_with};

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

/// The data of [`MEMBERS`]: `:Dog`, `:owns` and `:age` declared; `:Animal` and `:Pet`
/// used as classes undeclared; `:ann` typed by no class.
const DATA: &str = ":Dog a owl:Class . :Dog rdfs:subClassOf :Animal . \
     :owns a owl:ObjectProperty . :age a owl:DatatypeProperty . \
     :rex a :Dog , :Pet . :ann :owns :rex ; :age 3 .";

/// Statements of the reserved vocabulary that OWL 2 entails over [`DATA`] and no path
/// holds, found one by one before the rule: the member, its read (`false`), and a query
/// whose NOT EXISTS or MINUS over it keeps the row named last, which the certain answers
/// lack. The rule denies each by construction.
const MEMBERS: [(&str, &str, &str, &str); 8] = [
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
        "C disjointWith owl:Nothing",
        "ASK { :Animal owl:disjointWith owl:Nothing }",
        "SELECT ?c { :Dog rdfs:subClassOf ?c MINUS { ?c owl:disjointWith owl:Nothing } }",
        "Animal",
    ),
    (
        "P rdfs:domain owl:Thing",
        "ASK { :owns rdfs:domain owl:Thing }",
        "SELECT ?p { ?p a owl:ObjectProperty MINUS { ?p rdfs:domain owl:Thing } }",
        "owns",
    ),
    (
        "owl:topObjectProperty is transitive",
        "ASK { owl:topObjectProperty a owl:TransitiveProperty }",
        "SELECT ?p { ?p a owl:ObjectProperty \
         FILTER NOT EXISTS { owl:topObjectProperty a owl:TransitiveProperty } }",
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

/// Each [`MEMBERS`] member in a store: its read is `false` and not `complete`, and the
/// query over it keeps its wrong row and is `unsound`.
fn member_wrongs(store: &StoreService) -> Vec<String> {
    let mut wrong = Vec::new();
    for (member, read, negated, row) in MEMBERS {
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

/// [`DATA`] under `owl2-dl`, on the RL route ([`DATA`] is in OWL 2 RL) or off it (an
/// existential superclass is outside OWL 2 RL).
fn dl(off_the_rl_route: bool) -> nrese_store::MutationPipeline {
    let dl = pipeline();
    let existential = ":Dog rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :owns ; \
                       owl:someValuesFrom :Dog ] .";
    let data = format!("{DATA} {}", if off_the_rl_route { existential } else { "" });
    insert(&dl, &data).expect("data");
    dl
}

/// No path calls a read of a [`MEMBERS`] member complete: the rewriting (`owl2-ql` by
/// default, `owl2-rl` with it on), the ruleset closure and the DL bounds.
#[test]
fn no_path_calls_a_member_complete() {
    let mut all = Vec::new();
    for (ruleset, ql) in [
        (Ruleset::Owl2Ql, QlRewritingMode::Auto),
        (Ruleset::Owl2Rl, QlRewritingMode::On),
        (Ruleset::Owl2Rl, QlRewritingMode::Auto),
        (Ruleset::Owl2Ql, QlRewritingMode::Off),
    ] {
        for w in member_wrongs(&closure(DATA, ruleset, ql)) {
            all.push(format!("{} {ql:?}: {w}", ruleset.name()));
        }
    }
    for w in member_wrongs(dl(false).store()) {
        all.push(format!("owl2-dl: {w}"));
    }
    assert!(all.is_empty(), "{all:#?}");
}

/// The rule (default deny): where an answer could be `complete`, the rewriting's and the
/// DL bounds', a reserved predicate, `rdf:type` with a reserved class, a variable predicate
/// and a variable class each withhold it. Guards: a user predicate, and `rdf:type` with a
/// user class, stay `complete`; under the rewriting a negation over them too.
#[test]
fn only_user_predicates_and_classes_can_be_complete() {
    let dl = dl(false);
    let mut all = Vec::new();
    for (name, store) in [
        (
            "owl2-ql",
            &closure(DATA, Ruleset::Owl2Ql, QlRewritingMode::Auto),
        ),
        ("owl2-dl", dl.store().as_ref()),
    ] {
        for denied in [
            "ASK { :Dog rdfs:subClassOf :Animal }",
            "ASK { :Dog a owl:Class }",
            "SELECT ?p { :ann ?p :rex }",
            "SELECT ?c { :rex a ?c }",
        ] {
            let status = answer(store, denied).1;
            if status.contains(&"complete") {
                all.push(format!("{name}: {denied}: {status:?}"));
            }
        }
        for guard in ["ASK { :ann :owns :rex }", "SELECT ?x { ?x a :Animal }"] {
            let status = answer(store, guard).1;
            if status != ["complete"; 2] {
                all.push(format!("{name}: guard {guard}: {status:?}"));
            }
        }
    }
    let store = closure(DATA, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let guard = "SELECT ?x { ?x a :Animal FILTER NOT EXISTS { ?x a :Dog } }";
    let status = answer(&store, guard).1;
    if status != ["complete"; 2] {
        all.push(format!("owl2-ql: guard {guard}: {status:?}"));
    }
    assert!(all.is_empty(), "{all:#?}");
}

/// Off the RL route U1 types the data's individuals, so the bounds prove `:ann a
/// owl:Thing`; but not `:fresh`, which is in no bound though entailed. The rule denies
/// both: `:ann`'s is an over-mark.
#[test]
fn owl2_dl_off_the_rl_route_calls_no_owl_thing_read_complete() {
    let dl = dl(true);
    for (read, holds) in [
        ("ASK { :ann a owl:Thing }", "true"),
        ("ASK { :fresh a owl:Thing }", "false"),
    ] {
        let (answer_, status) = answer(dl.store(), read);
        assert!(answer_.contains(holds), "{read}: {answer_}");
        assert!(!status.contains(&"complete"), "{read}: {status:?}");
    }
}

/// DESCRIBE reads every predicate of what it names, `:ann owl:sameAs :ann` and its
/// top-property pairs among them, which no store holds: never `complete`, run or planned.
#[test]
fn a_describe_is_never_complete() {
    let dl = dl(false);
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

/// An `owl2-dl` store whose bounds leave candidates of `?x a :D` unresolved (no branch
/// point allowed): x and w are `:D`s only through the union, which the RL closure lacks.
fn unresolved() -> nrese_store::MutationPipeline {
    let dl = pipeline_with(DlConfig {
        max_branch_points: Some(0),
        ..DlConfig::default()
    });
    insert(
        &dl,
        ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :x a :A . :y a :B . :w a [ owl:unionOf ( :B :C ) ] .",
    )
    .expect("data");
    dl
}

/// Which operators turn an answer that may miss rows into one that may be wrong, on the
/// DL path as on the others. GRAPH, SERVICE and DESCRIBE are monotone in the data: adding
/// the statements an incomplete answer lacks can only add rows or triples. LIMIT or OFFSET
/// without ORDER BY isn't: a missing row can change which rows it selects. But every row
/// it returns is one of the answer's rows, so no returned row is wrong. So an answer
/// incomplete for its own reasons (here candidates the bounds leave unresolved) is
/// `sound-only` under all four. An ordered slice differs: a missing row can take a
/// returned one's place, which is then no row of the certain slice, so it is `unsound`.
/// Changing this list means arguing with these cases.
#[test]
fn only_operators_that_can_make_a_row_wrong_label_a_dl_answer_unsound() {
    let store = unresolved();
    // x or w, as a JSON value or an N-Triples term.
    let rows = |text: &str| {
        ["x", "w"].map(|r| {
            ["\"", ">"]
                .iter()
                .any(|e| text.contains(&format!("example.com/{r}{e}")))
        })
    };
    let mut wrong = Vec::new();
    for (query, expected) in [
        ("SELECT ?x { ?x a :D } LIMIT 2 OFFSET 1", "sound-only"),
        ("SELECT ?x { GRAPH ?g { ?x a :D } }", "sound-only"),
        ("DESCRIBE ?x { ?x a :D }", "sound-only"),
        ("SELECT ?x { ?x a :D } ORDER BY ?x LIMIT 1", "unsound"),
    ] {
        let (text, status) = answer(store.store(), query);
        if status != [expected; 2] {
            wrong.push(format!("{query}: {status:?}, expected {expected}"));
        }
        // Sound under the four: only y, which the RL closure has, is returned.
        if expected == "sound-only" && rows(&text) != [false; 2] {
            wrong.push(format!("{query}: a candidate returned: {text}"));
        }
    }
    // SERVICE calls no endpoint here (no federation is set up): its plan says it.
    let service = "SELECT ?x { ?x a :D SERVICE <http://elsewhere.example/sparql> { ?x :p ?o } }";
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{service}"));
    let prepared = store.store().prepare_query(&request).expect("prepared");
    let planned = store
        .store()
        .plan_query(&prepared, &CancellationToken::new())
        .expect("planned");
    let status = planned.completeness.map(|s| s.as_str());
    if status != Some("sound-only") {
        wrong.push(format!("{service}: planned {status:?}"));
    }
    // A denied read is incomplete for its own reason too: the same two outcomes.
    let thing = dl(false);
    for (query, expected) in [
        ("SELECT ?x { ?x a owl:Thing } LIMIT 1", "sound-only"),
        (
            "SELECT ?x { ?x a owl:Thing } ORDER BY ?x LIMIT 1",
            "unsound",
        ),
    ] {
        let status = answer(thing.store(), query).1;
        if status != [expected; 2] {
            wrong.push(format!("{query}: {status:?}, expected {expected}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every place that reports a query's completeness gives the answer's status, never a
/// value of its own: the answer (the `NRESE-Completeness` header), EXPLAIN that runs it,
/// the QL rewriting's report in both (`ql.completeness`), a refusal of exact answers (the
/// 422 problem document), and `print-query`, for the store it prints for (`owl2-rl` with
/// the rewriting on). The server writes these as the store gives them. A plan (`explain=
/// plan`, its `ql` too) never claims more than the run, and claims less only where the run
/// decides, saying so (docs/spec/reasoning-semantics.md, "A plan's status"). One read per
/// status and reason kind: a plain read, a denied one (an annotation, the one part of the
/// reserved vocabulary print prints), that under NOT EXISTS, an ordered slice, and the
/// `owl:sameAs` entry, on and off the RL route.
#[test]
fn every_place_gives_a_query_the_answers_status() {
    use nrese_sparql::Completeness;
    use nrese_sparql::ql::{PrintForm, Printed};
    // Each read with the answer every store must give, where the rule fixes it: a denied
    // read is `sound-only` everywhere, and `unsound` under NOT EXISTS. Those must print.
    let reads = [
        ("ASK { :rex a :Animal }", None),
        ("SELECT ?l { :rex rdfs:label ?l }", Some("sound-only")),
        (
            "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x rdfs:label ?l } }",
            Some("unsound"),
        ),
        ("SELECT ?x { ?x a :Animal } ORDER BY ?x LIMIT 1", None),
        ("ASK { :rex owl:sameAs :ann }", None),
    ];
    let printed_for = closure(DATA, Ruleset::Owl2Rl, QlRewritingMode::On);
    let ql = closure(DATA, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let rl = closure(DATA, Ruleset::Owl2Rl, QlRewritingMode::Auto);
    let (on, off) = (dl(false), dl(true));
    let stores = [
        ("owl2-rl with the rewriting", &printed_for, true),
        ("owl2-ql", &ql, false),
        ("owl2-rl", &rl, false),
        ("owl2-dl on the RL route", on.store(), false),
        ("owl2-dl off it", off.store(), false),
    ];
    fn report(r: &Option<nrese_sparql::ql::QlReport>) -> Option<&nrese_sparql::Completeness> {
        r.as_ref().map(|r| &r.completeness)
    }
    let name = |c: Option<&Completeness>| c.map_or("none", |c| c.as_str());
    // `not-certified` claims nothing, so it is never above.
    let order = ["not-certified", "unsound", "sound-only", "complete"];
    let rank = |c: Option<&Completeness>| order.iter().position(|r| *r == name(c));
    let cancel = CancellationToken::new();
    let mut wrong = Vec::new();
    for (store_name, store, prints) in stores {
        for (query, expected) in reads {
            let at = format!("{store_name}: {query}");
            let text = format!("{PREFIXES}{RDF}{query}");
            let request = SparqlQueryRequest::all(text.clone());
            let run = store.execute_query(&request).expect("answered");
            let answer = run.completeness.as_ref();
            let prepared = store.prepare_query(&request).expect("prepared");
            let explained = store.explain_query(&prepared, &cancel).expect("explained");
            let planned = store.plan_query(&prepared, &cancel).expect("planned");
            // A `ql` report is null where the rewriting doesn't apply.
            let mut equal = vec![("EXPLAIN", explained.completeness.as_ref())];
            for (place, r) in [("ql", &run.ql), ("EXPLAIN's ql", &explained.ql)] {
                equal.extend(report(r).map(|c| (place, Some(c))));
            }
            let exact = refusal(store, &request);
            equal.extend(exact.as_ref().map(|status| ("422", Some(status))));
            let printed = prints.then(|| store.print_query(&text, PrintForm::Paths));
            match &printed {
                Some(Ok(Printed::Query { completeness, .. })) => {
                    equal.push(("print-query", Some(completeness)));
                }
                Some(other) if expected.is_some() => wrong.push(format!("{at}: {other:?}")),
                _ => {}
            }
            for (place, status) in equal {
                if name(status) != name(answer) {
                    wrong.push(format!(
                        "{at}: {place} {}, the answer {}",
                        name(status),
                        name(answer)
                    ));
                }
            }
            let mut plans = vec![("plan", planned.completeness.as_ref())];
            plans.extend(report(&planned.ql).map(|c| ("plan's ql", Some(c))));
            for (place, plan) in plans {
                let decides = plan.is_some_and(|p| {
                    p.reasons
                        .iter()
                        .any(|r| r.text.contains("decided when it runs"))
                });
                let (p, a) = (rank(plan), rank(answer));
                if p > a || (p < a && !decides) {
                    wrong.push(format!(
                        "{at}: {place} {}, the answer {}",
                        name(plan),
                        name(answer)
                    ));
                }
            }
            if expected.is_some_and(|e| e != name(answer)) {
                wrong.push(format!("{at}: the answer {}", name(answer)));
            }
        }
    }
    // None of those reads is refused yet under exact answers (D16-653): one the bounds
    // leave undecided is.
    let undecided = unresolved();
    let request = SparqlQueryRequest::all(format!("{PREFIXES}SELECT ?x {{ ?x a :D }}"));
    let answer = undecided.store().execute_query(&request).expect("answered");
    let refused = refusal(undecided.store(), &request);
    if refused.as_ref().map(|c| c.as_str()) != answer.completeness.as_ref().map(|c| c.as_str()) {
        wrong.push(format!(
            "422 {refused:?}, the answer {:?}",
            answer.completeness
        ));
    }
    let printed = printed_for.print_query(
        &format!("{PREFIXES}ASK {{ :fresh a owl:Thing }}"),
        PrintForm::Paths,
    );
    // What print can't print carries no status: `owl:Thing` is of the schema vocabulary.
    if !matches!(printed, Ok(Printed::NotExpressible(_))) {
        wrong.push(format!("print-query of owl:Thing: {printed:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The status of `request`'s refusal under exact answers (the 422 problem document's),
/// if it is refused.
fn refusal(
    store: &StoreService,
    request: &SparqlQueryRequest,
) -> Option<nrese_sparql::Completeness> {
    let mut exact = request.clone();
    exact.dl_answers = Some(nrese_store::DlAnswers::Exact);
    match store.execute_query(&exact) {
        Err(nrese_store::StoreError::Incomplete(refused)) => Some(refused.status),
        _ => None,
    }
}

/// An existential outside OWL 2 RL: a store with it is off the RL route.
const EXISTENTIAL: &str = ":Dog rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :q ; \
                           owl:someValuesFrom :Dog ] .";

/// `owl:sameAs` between two different constant IRIs is the one reserved read that may be
/// `complete`, off the RL route only, where the upper bound types both as individuals: the
/// bounds decide equality between them as they decide their other facts, when the query
/// runs (its plan says `sound-only` until then). :x has one :p, so :a = :b is entailed;
/// nothing makes :a = :x. On the RL route no upper bound is built, so the same reads are
/// denied, right but not `complete`, planned or run; and so over the rules' closure.
#[test]
fn owl2_dl_decides_same_as_between_two_constants_the_upper_bound_types() {
    let data = ":p a owl:ObjectProperty , owl:FunctionalProperty . :x :p :a , :b . \
                :a a :Dog . :x a :Dog .";
    let mut wrong = Vec::new();
    for off_the_rl_route in [false, true] {
        let dl = pipeline();
        let extra = if off_the_rl_route { EXISTENTIAL } else { "" };
        insert(&dl, &format!("{data} {extra}")).expect("data");
        for (read, holds) in [
            ("ASK { :a owl:sameAs :b }", "true"),
            ("ASK { :a owl:sameAs :x }", "false"),
        ] {
            let (text, status) = answer(dl.store(), read);
            let decided = if off_the_rl_route {
                status == ["complete", "sound-only"]
            } else {
                !status.contains(&"complete")
            };
            if !text.contains(holds) || !decided {
                wrong.push(format!("off {off_the_rl_route}: {read}: {text} {status:?}"));
            }
        }
    }
    let closure = closure(data, Ruleset::Owl2Rl, QlRewritingMode::On);
    let (text, status) = answer(&closure, "ASK { :a owl:sameAs :b }");
    if !text.contains("true") || status.contains(&"complete") {
        wrong.push(format!("owl2-rl with the rewriting: {text} {status:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// What the `owl:sameAs` entry leaves denied off the RL route, where it applies: one term
/// at both ends (eq-ref, in neither bound: `:x owl:sameAs :x` and `:fresh owl:sameAs
/// :fresh` are entailed, and the store answers `false`), and a variable end (`:a`'s own
/// pair is missing).
#[test]
fn owl2_dl_denies_same_as_of_one_term_or_a_variable() {
    let dl = pipeline();
    let data = format!(":p a owl:FunctionalProperty . :x :p :a , :b . :x a :Dog . {EXISTENTIAL}");
    insert(&dl, &data).expect("data");
    for read in [
        "ASK { :x owl:sameAs :x }",
        "ASK { :fresh owl:sameAs :fresh }",
    ] {
        let (text, status) = answer(dl.store(), read);
        assert!(text.contains("false"), "{read}: {text}");
        assert!(!status.contains(&"complete"), "{read}: {status:?}");
    }
    let (text, status) = answer(dl.store(), "SELECT ?y { :a owl:sameAs ?y }");
    assert!(text.contains("example.com/b"), "{text}");
    assert!(!status.contains(&"complete"), "{status:?}");
}

/// A term the upper bound doesn't type as an individual (outside the data, or only a class
/// name) is one it doesn't reason about: where the ontology allows one individual only
/// (`owl:Thing ⊑ {:a}`) every term is entailed equal to :a, which neither bound says for
/// such a term. So its `owl:sameAs` reads are never `complete`, off the RL route too.
/// Without such an axiom the reads of terms outside the data are `false`, and right.
#[test]
fn owl2_dl_same_as_of_a_term_the_upper_bound_doesnt_type_is_never_complete() {
    let mut wrong = Vec::new();
    for (data, one_individual) in [
        (format!(":x a :Dog . {EXISTENTIAL}"), false),
        (
            "owl:Thing rdfs:subClassOf [ owl:oneOf ( :a ) ] . :a a :Dog . :b a :Dog .".to_owned(),
            true,
        ),
    ] {
        let dl = pipeline();
        insert(&dl, &data).expect("data");
        for read in [
            "ASK { :fresh owl:sameAs :new }",
            "ASK { :x owl:sameAs :fresh }",
            // :Dog occurs only as a class.
            "ASK { :Dog owl:sameAs :a }",
        ] {
            let (text, status) = answer(dl.store(), read);
            if (!one_individual && !text.contains("false")) || status.contains(&"complete") {
                wrong.push(format!("{data}: {read}: {text} {status:?}"));
            }
        }
        // Two individuals of the data: the bounds decide, the one individual included.
        if one_individual {
            let (text, status) = answer(dl.store(), "ASK { :a owl:sameAs :b }");
            if !(text.contains("true") || !status.contains(&"complete")) {
                wrong.push(format!("{data}: :a = :b: {text} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A key on one data value in two lexical forms (`1` and `"01"` as xsd:integer) equates
/// `:a` and `:b` (OWL 2 Direct Semantics, HasKey), so `:b :p :x` is entailed. The owl2-rl
/// closure lacks it: the rule set leaves out dt-eq. Its `false` must not be labelled
/// complete at any place the read reaches (run, plan, and the rewriting's report in both),
/// with the rewriting on or not; that the other places give the same value is
/// [`every_place_gives_a_query_the_answers_status`]'s. Each status is printed.
#[test]
fn owl2_rl_calls_no_answer_a_key_misses_by_term_complete() {
    never_complete(":ssn a owl:DatatypeProperty .");
}

/// As [`owl2_rl_calls_no_answer_a_key_misses_by_term_complete`], with `:ssn` undeclared: the
/// rewriting's schema reads no assertions, so it can't tell `:ssn` carries data values.
#[test]
fn owl2_rl_calls_no_answer_a_key_on_an_undeclared_property_misses_complete() {
    never_complete("");
}

/// A maximum qualified by `owl:onDataRange rdfs:Literal`: at most no value of `:age`, and
/// `:c` has one, so the data has no model and every statement is entailed. No owl2-rl rule
/// reads `owl:onDataRange` (cls-maxqc1-4 read `owl:onClass`), so the closure misses the
/// clash: no place may call `ASK { :c a :Z }`'s `false` complete.
#[test]
fn owl2_rl_calls_no_answer_a_qualified_data_maximum_misses_complete() {
    no_place_complete(
        ":age a owl:DatatypeProperty . \
         :C a owl:Class ; rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :age ; \
           owl:maxQualifiedCardinality \"0\"^^xsd:nonNegativeInteger ; \
           owl:onDataRange rdfs:Literal ] . \
         :c a :C ; :age 5 .",
        "ASK { :c a :Z }",
    );
}

/// An existential on `:age`, which is undeclared and carries a literal, filled by `filler`.
fn data_existential(filler: &str) -> String {
    format!(
        ":Aged a owl:Class . \
         [ a owl:Restriction ; owl:onProperty :age ; owl:someValuesFrom {filler} ] \
           rdfs:subClassOf :Aged . \
         :a :age 5 ."
    )
}

/// cls-svf1 on `:age`, which is undeclared and carries a literal: `:a` is `:Aged` (OWL 2
/// Direct Semantics, DataSomeValuesFrom), whose premise (`5` in the filler) is dt-type2's,
/// which the rule set leaves out. The rewriting's schema reads no assertions, so only the
/// filler says `:age` is a data property: a datatype, or an anonymous data range (OWL 2 RL
/// admits data intersections, Profiles §4.2.4), typed `rdfs:Datatype` or not, nested in
/// a union too. 5 is an integer and a decimal.
#[test]
fn owl2_rl_calls_no_answer_an_undeclared_data_existential_misses_complete() {
    for filler in [
        "xsd:integer",
        "[ a rdfs:Datatype ; owl:intersectionOf ( xsd:integer xsd:decimal ) ]",
        "[ owl:intersectionOf ( xsd:integer xsd:decimal ) ]",
        "[ owl:unionOf ( [ owl:intersectionOf ( xsd:integer xsd:decimal ) ] xsd:string ) ]",
    ] {
        no_place_complete(&data_existential(filler), "ASK { :a a :Aged }");
    }
}

/// cls-int1 and cls-svf1: `:x` is `:B` and `:C`, so `:a :knows :x` makes `:a` `:Social`,
/// and the rules derive it. `:knows` is undeclared and the filler an intersection of
/// classes, no data range: the restriction is an object one, and with the rewriting on
/// every place calls the `true` complete (a data reading would call it so in none).
#[test]
fn owl2_rl_reads_an_undeclared_object_intersection_existential_as_object() {
    let data = ":B a owl:Class . :C a owl:Class . \
         [ a owl:Restriction ; owl:onProperty :knows ; \
           owl:someValuesFrom [ owl:intersectionOf ( :B :C ) ] ] rdfs:subClassOf :Social . \
         :a :knows :x . :x a :B , :C .";
    let mut wrong = Vec::new();
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        let (text, line, complete, _) = places(data, "ASK { :a a :Social }", ql);
        if !text.contains("true") || (ql == QlRewritingMode::On && complete != 4) {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `:a :age 5` and ∃age.rdfs:Literal ⊑ :Aged entail `:a a :Aged` (every literal is in
/// `rdfs:Literal`; no datatype rule is needed). With the rewriting on, the filler says the
/// undeclared `:age` is a data property, and the answer is the entailed `true`; on `Auto`,
/// the closure's `false` is called complete in no place.
#[test]
fn owl2_rl_answers_an_undeclared_data_existential_over_any_literal() {
    let data = data_existential("rdfs:Literal");
    let mut wrong = Vec::new();
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        let (text, line, complete, _) = places(&data, "ASK { :a a :Aged }", ql);
        let right = match ql {
            QlRewritingMode::On => text.contains("true"),
            _ => text.contains("false") && complete == 0,
        };
        if !right {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The key's case, `:ssn` declared by `declaration`: see [`no_place_complete`].
fn never_complete(declaration: &str) {
    let data = format!(
        ":Person owl:hasKey ( :ssn ) . {declaration} \
         :p a owl:ObjectProperty . :a a :Person ; :ssn 1 ; :p :x . \
         :b a :Person ; :ssn \"01\"^^xsd:integer ."
    );
    no_place_complete(&data, "ASK { :b :p :x }");
}

/// `data` under owl2-rl, and `read`, entailed but `false` over the closure: no place calls
/// it complete, with the rewriting on or not.
fn no_place_complete(data: &str, read: &str) {
    let mut wrong = Vec::new();
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        let (text, line, complete, reported) = places(data, read, ql);
        // With the rewriting on, its report is one of the places.
        if !text.contains("false") || complete > 0 || (ql == QlRewritingMode::On && !reported) {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `read` over `data`'s owl2-rl closure with the rewriting `ql`: its answer, a printed line
/// with what each place (the run, its rewriting, the plan, the plan's rewriting) calls it,
/// how many places call it complete, and whether the run has the rewriting's report.
fn places(data: &str, read: &str, ql: QlRewritingMode) -> (String, String, usize, bool) {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{read}"));
    let name = |c: Option<&nrese_sparql::Completeness>| c.map_or("none", |c| c.as_str());
    let store = closure(data, Ruleset::Owl2Rl, ql);
    let run = store.execute_query(&request).expect("answered");
    let prepared = store.prepare_query(&request).expect("prepared");
    let planned = store
        .plan_query(&prepared, &CancellationToken::new())
        .expect("planned");
    let text = String::from_utf8_lossy(&run.payload).into_owned();
    let places = [
        ("run", run.completeness.as_ref()),
        ("ql", run.ql.as_ref().map(|r| &r.completeness)),
        ("plan", planned.completeness.as_ref()),
        ("plan's ql", planned.ql.as_ref().map(|r| &r.completeness)),
    ];
    let line = format!(
        "rewriting {ql:?}: {text} {:?}",
        places.map(|(p, c)| (p, name(c)))
    );
    eprintln!("{line}");
    let complete = places
        .iter()
        .filter(|(_, c)| name(*c) == "complete")
        .count();
    (text, line, complete, run.ql.is_some())
}
