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
use crate::ql_rewriting_tests::Rng;

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
    dl_in(pipeline(), off_the_rl_route)
}

/// [`dl`] in `dl`'s configuration.
fn dl_in(
    dl: nrese_store::MutationPipeline,
    off_the_rl_route: bool,
) -> nrese_store::MutationPipeline {
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
/// `owl:sameAs` entry, on and off the RL route; under OWL 2 DL also with user rules and
/// with `dl.consistency = "off"`, whose reasons the run adds whatever path decides.
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
    // The two reasons the DL run adds whatever path decides: user rules (one that reads
    // nothing of DATA) and a consistency that isn't checked.
    let rules = || {
        let rule = "{ ?x a <http://example.com/Special> } => { ?x a <http://example.com/Rare> } .";
        crate::user_rules_tests::pipeline(nrese_reasoner::ReasoningMode::Owl2Dl, rule)
    };
    let unchecked = || {
        pipeline_with(DlConfig {
            consistency: nrese_store::DlConsistency::Off,
            ..DlConfig::default()
        })
    };
    let (rules_on, rules_off) = (dl_in(rules(), false), dl_in(rules(), true));
    let (unchecked_on, unchecked_off) = (dl_in(unchecked(), false), dl_in(unchecked(), true));
    let stores = [
        ("owl2-rl with the rewriting", &printed_for, true),
        ("owl2-ql", &ql, false),
        ("owl2-rl", &rl, false),
        ("owl2-dl on the RL route", on.store(), false),
        ("owl2-dl off it", off.store(), false),
        (
            "owl2-dl with user rules on the RL route",
            rules_on.store(),
            false,
        ),
        ("owl2-dl with user rules off it", rules_off.store(), false),
        (
            "owl2-dl, consistency off, on the RL route",
            unchecked_on.store(),
            false,
        ),
        (
            "owl2-dl, consistency off, off it",
            unchecked_off.store(),
            false,
        ),
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

/// `:a0`'s value of `:d1` is the integer 2, written `"2"`; `"+2"` is another lexical form of
/// the same value (XML Schema Datatypes, `xsd:integer`'s lexical space).
const PLUS_TWO: &str = ":d1 a owl:DatatypeProperty . :a0 :d1 \"2\"^^xsd:integer .";

/// Each `(data, read, certain)` loaded and asked, run and planned: the answer holds every
/// IRI of `certain`, or neither status says `complete`. The wrong ones, as lines.
fn misses_and_says_complete(
    store: impl Fn(&str) -> StoreService,
    cases: &[(&str, &str, &[&str])],
) -> Vec<String> {
    let mut wrong = Vec::new();
    for &(data, read, certain) in cases {
        let (rows, status) = answer(&store(data), read);
        let missing: Vec<_> = certain
            .iter()
            .filter(|t| !rows.contains(&format!("example.com/{t}\"")))
            .collect();
        eprintln!("{read}: missing {missing:?}, {status:?}");
        if !missing.is_empty() && status.contains(&"complete") {
            wrong.push(format!("{data} / {read}: missing {missing:?}, {status:?}"));
        }
    }
    wrong
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

/// A max cardinality in plain Turtle (`1`, an xsd:integer): at most one `:p` equates `:x`
/// and `:y` (OWL 2 Direct Semantics), but the owl2-rl rules read only the mapping's
/// spelling (`"1"^^xsd:nonNegativeInteger`). Its `false` must not be labelled complete at
/// any place, as the key's: neither the equality's nor that of `:y :q :z`, which follows
/// from it and reads no equality.
#[test]
fn owl2_rl_calls_no_answer_a_plain_max_cardinality_misses_complete() {
    let data = ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
           owl:maxCardinality 1 ] . \
         :p a owl:ObjectProperty . :q a owl:ObjectProperty . \
         :c a :C ; :p :x , :y . :x :q :z .";
    for read in ["ASK { :x owl:sameAs :y }", "ASK { :y :q :z }"] {
        no_place_complete(data, read);
    }
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

/// An existential on `:knows`, which is undeclared and carries an individual of `:B`, `:C`
/// and `:D`, filled by the class expression `classes`.
fn object_existential(classes: &str) -> String {
    format!(
        ":B a owl:Class . :C a owl:Class . :D a owl:Class . \
         [ a owl:Restriction ; owl:onProperty :knows ; owl:someValuesFrom {classes} ] \
           rdfs:subClassOf :Social . \
         :a :knows :x . :x a :B , :C , :D ."
    )
}

/// What the four places (run, rewriting, plan, plan's rewriting) call an answer: `complete`
/// everywhere with the rewriting on, or `sound-only`; the rewriting's reports are there with
/// it on only.
fn matrix(complete: bool, ql: QlRewritingMode) -> [&'static str; 4] {
    match ql {
        QlRewritingMode::On if complete => ["complete"; 4],
        QlRewritingMode::On => ["sound-only"; 4],
        _ => ["sound-only", "none", "sound-only", "none"],
    }
}

/// An undeclared property's existential, read by its filler alone (the rewriting's schema
/// reads no assertions). The rows are OWL 2 RL's data ranges (Profiles §4.2.4, grammar in
/// §6.3: `DataRange := Datatype | DataIntersectionOf`): a datatype of RL (§4.2.1;
/// `rdfs:Literal`'s answer is the entailed one, see
/// [`owl2_rl_answers_an_undeclared_data_existential_over_any_literal`]), one a
/// DatatypeDefinition names, an intersection typed `rdfs:Datatype` or not, one nested, and
/// one whose operands are one shared intersection (`_:r` twice, typed or not: a shared
/// expression the reader reads, no cycle); then two outside RL: a union with an
/// intersection in it, and an untyped datatype restriction. 5 is in each, so `:a :age 5`
/// makes `:a` `:Aged` (OWL 2 Direct Semantics, DataSomeValuesFrom), by dt-type2, which the
/// rule set leaves out: the closure's `false` is `sound-only` at the run and the plan, and
/// at both rewriting reports with the rewriting on (`none` without it). Each row's control
/// fills the existential on `:knows` with classes of the same shape, an object restriction
/// the rules derive (cls-int1, cls-uni, cls-svf1): `true`, `sound-only` at the run and the
/// plan without the rewriting, and with it `complete` in every place (a data reading would
/// call it so in none).
#[test]
fn owl2_rl_reads_an_undeclared_existential_by_its_filler() {
    // (the data range's definitions, the data range, the control's definitions, the control)
    let rows = [
        ("", "xsd:integer", "", ":B"),
        (
            ":Number a rdfs:Datatype ; owl:equivalentClass xsd:integer .",
            ":Number",
            "",
            ":B",
        ),
        (
            "",
            "[ a rdfs:Datatype ; owl:intersectionOf ( xsd:integer xsd:decimal ) ]",
            "",
            "[ a owl:Class ; owl:intersectionOf ( :B :C ) ]",
        ),
        (
            "",
            "[ owl:intersectionOf ( xsd:integer xsd:decimal ) ]",
            "",
            "[ owl:intersectionOf ( :B :C ) ]",
        ),
        (
            "",
            "[ owl:intersectionOf ( [ owl:intersectionOf ( xsd:integer xsd:decimal ) ] xsd:int ) ]",
            "",
            "[ owl:intersectionOf ( [ owl:intersectionOf ( :B :C ) ] :D ) ]",
        ),
        (
            "_:r owl:intersectionOf ( xsd:integer xsd:decimal ) .",
            "[ owl:intersectionOf ( _:r _:r ) ]",
            "_:o owl:intersectionOf ( :B :C ) .",
            "[ owl:intersectionOf ( _:o _:o ) ]",
        ),
        (
            "_:r a rdfs:Datatype ; owl:intersectionOf ( xsd:integer xsd:decimal ) .",
            "[ owl:intersectionOf ( _:r _:r ) ]",
            "_:o a owl:Class ; owl:intersectionOf ( :B :C ) .",
            "[ owl:intersectionOf ( _:o _:o ) ]",
        ),
        (
            "",
            "[ owl:unionOf ( [ owl:intersectionOf ( xsd:integer xsd:decimal ) ] xsd:string ) ]",
            "",
            "[ owl:unionOf ( [ owl:intersectionOf ( :B :C ) ] :D ) ]",
        ),
        (
            "",
            "[ owl:onDatatype xsd:integer ; owl:withRestrictions ( [ xsd:minInclusive 1 ] ) ]",
            "",
            ":B",
        ),
    ];
    let mut wrong = Vec::new();
    let mut check = |name: String, data: &str, read: &str, answer: &str| {
        eprintln!("{name}:");
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let (text, line, statuses, _) = places(data, read, ql);
            if !text.contains(answer) || statuses != matrix(answer == "true", ql) {
                wrong.push(format!("{name}: {line}"));
            }
        }
    };
    for (definitions, range, defined, classes) in rows {
        let data = format!("{definitions} {}", data_existential(range));
        let filler = format!("filler {}", [definitions, range].join(" ").trim());
        check(filler, &data, "ASK { :a a :Aged }", "false");
        let control = format!("{defined} {}", object_existential(classes));
        let name = format!("control {}", [defined, classes].join(" ").trim());
        check(name, &control, "ASK { :a a :Social }", "true");
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
        let (text, line, statuses, _) = places(&data, "ASK { :a a :Aged }", ql);
        let right = match ql {
            QlRewritingMode::On => text.contains("true"),
            _ => text.contains("false") && !statuses.contains(&"complete"),
        };
        if !right {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The generated tests' seed, in their names.
const SEED: u64 = 0x5eed_da7a;

/// Fillers in Turtle, from a seed. Each composite node is written in place (`[ … ]`) or as
/// a shared blank node defined once (`_:sN …`), which later nodes may name again, twice in
/// one list too; a node names only earlier ones, so there is no cycle.
struct Fillers {
    random: Rng,
    definitions: Vec<String>,
    shared: Vec<String>,
}

impl Fillers {
    /// A fresh generator from [`SEED`].
    fn new() -> Self {
        Self {
            random: Rng(SEED),
            definitions: Vec::new(),
            shared: Vec::new(),
        }
    }

    /// The node of `po` (its predicates and objects), in place or shared.
    fn node(&mut self, po: String) -> String {
        if !self.random.chance(40) {
            return format!("[ {po} ]");
        }
        let name = format!("_:s{}", self.definitions.len());
        self.definitions.push(format!("{name} {po} ."));
        self.shared.push(name.clone());
        name
    }

    /// Now and then a shared node already defined, named again.
    fn again(&mut self) -> Option<String> {
        (!self.shared.is_empty() && self.random.chance(30))
            .then(|| self.shared[self.random.below(self.shared.len())].clone())
    }

    /// The shared nodes' definitions so far, and a fresh start for the next filler.
    fn take(&mut self) -> String {
        self.shared.clear();
        std::mem::take(&mut self.definitions).join(" ")
    }

    /// A data range of depth at most `depth`: a datatype, an enumeration of literals or a
    /// datatype restriction, or a complement, intersection or union of data ranges; typed
    /// `rdfs:Datatype` or not.
    fn data(&mut self, depth: usize) -> String {
        if let Some(name) = self.again() {
            return name;
        }
        let typed = if self.random.chance(30) {
            "a rdfs:Datatype ; "
        } else {
            ""
        };
        let po = match self.random.below(if depth == 0 { 3 } else { 6 }) {
            0 => {
                let datatypes = ["xsd:integer", "xsd:decimal", "xsd:int", "xsd:string"];
                return datatypes[self.random.below(4)].to_owned();
            }
            1 => format!("{typed}owl:oneOf ( 5 7 )"),
            2 => format!(
                "{typed}owl:onDatatype xsd:integer ; owl:withRestrictions ( [ xsd:minInclusive 1 ] )"
            ),
            3 => format!("{typed}owl:datatypeComplementOf {}", self.data(depth - 1)),
            kind => {
                let operands: Vec<String> = (0..2).map(|_| self.data(depth - 1)).collect();
                let operator = if kind == 4 {
                    "intersectionOf"
                } else {
                    "unionOf"
                };
                format!("{typed}owl:{operator} ( {} )", operands.join(" "))
            }
        };
        self.node(po)
    }

    /// A data range of RL (Profiles §4.2.4) that holds every `xsd:int`: `xsd:int` or a
    /// datatype above it, or an intersection of such; typed `rdfs:Datatype` or not.
    fn rl_data(&mut self, depth: usize) -> String {
        if let Some(name) = self.again() {
            return name;
        }
        let typed = if self.random.chance(30) {
            "a rdfs:Datatype ; "
        } else {
            ""
        };
        if self.random.below(if depth == 0 { 1 } else { 2 }) == 0 {
            return ["xsd:int", "xsd:integer", "xsd:decimal"][self.random.below(3)].to_owned();
        }
        let operands: Vec<String> = (0..2).map(|_| self.rl_data(depth - 1)).collect();
        let operands = operands.join(" ");
        self.node(format!("{typed}owl:intersectionOf ( {operands} )"))
    }

    /// A class expression of depth at most `depth` that `:x`, in `:B`, `:C` and `:D`, is in:
    /// one of them, an intersection of such, or a union of such with or without a complement
    /// of a named class beside them; typed `owl:Class` or not.
    fn classes(&mut self, depth: usize) -> String {
        if let Some(name) = self.again() {
            return name;
        }
        let named = |random: &mut Rng| [":B", ":C", ":D"][random.below(3)];
        if depth == 0 || self.random.chance(30) {
            return named(&mut self.random).to_owned();
        }
        let typed = if self.random.chance(30) {
            "a owl:Class ; "
        } else {
            ""
        };
        let mut operands: Vec<String> = (0..2).map(|_| self.classes(depth - 1)).collect();
        let operator = if self.random.chance(50) {
            "intersectionOf"
        } else {
            if self.random.chance(50) {
                operands.push(format!("[ owl:complementOf {} ]", named(&mut self.random)));
            }
            "unionOf"
        };
        self.node(format!("{typed}owl:{operator} ( {} )", operands.join(" ")))
    }
}

/// [`owl2_rl_reads_an_undeclared_existential_by_its_filler`] over generated fillers, a wrong
/// case named by its half and index:
/// - 200 data ranges ([`Fillers::data`]: every constructor the reader classifies, nested to
///   depth three, with shared nodes named again). Each says the undeclared `:age` is a data
///   property: the places say what the table's data rows say, whatever the answer.
/// - Most of those are outside RL, where a class reading is `sound-only` too. So 200 of RL's
///   own ([`Fillers::rl_data`]), each holding the `xsd:int` 5 by its build: `:a a :Aged` is
///   entailed, so the answer is `true` or no place calls it complete.
/// - The mirror, 200 class expressions `:x` is in ([`Fillers::classes`]): object ones, the
///   rules derive `:a a :Social`, `complete` in every place with the rewriting on. With a
///   complement in them it is `sound-only`: RL has no complement in a subclass expression
///   (Profiles §4.2.3, `subClassExpression`).
#[test]
fn generated_fillers_are_read_by_their_kind_seed_5eedda7a() {
    let mut wrong = Vec::new();
    let mut check = |name: String, data: &str, read: &str, answer: &str, complete: bool| {
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let (text, line, statuses, _) = places(data, read, ql);
            if !text.contains(answer) || statuses != matrix(complete, ql) {
                wrong.push(format!("{name}: {data} / {line}"));
            }
        }
    };
    let (aged, social) = ("ASK { :a a :Aged }", "ASK { :a a :Social }");
    let mut fillers = Fillers::new();
    for i in 0..200 {
        let range = fillers.data(3);
        let data = format!("{} {}", fillers.take(), data_existential(&range));
        check(format!("data {i}"), &data, aged, "", false);
        let classes = fillers.classes(3);
        let control = format!("{} {}", fillers.take(), object_existential(&classes));
        let complete = !control.contains("owl:complementOf");
        check(format!("control {i}"), &control, social, "true", complete);
    }
    let mut fillers = Fillers::new();
    for i in 0..200 {
        let range = fillers.rl_data(3);
        let data = format!(
            "{} {} :a :age \"5\"^^xsd:int .",
            fillers.take(),
            data_existential(&range)
        );
        for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
            let (text, line, statuses, _) = places(&data, aged, ql);
            if !text.contains("true") && statuses.contains(&"complete") {
                wrong.push(format!("rl {i}: {data} / {line}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{} wrong: {wrong:#?}", wrong.len());
}

/// Thirty intersections, each of the one before twice (`_:c1` is `( _:c0 _:c0 )`, …): a data
/// range, read once per node. A walk along every path would take 2³⁰ steps; this one reads
/// it in the test's ordinary time, and no place calls the answer complete.
#[test]
fn a_deep_chain_of_shared_data_ranges_is_read_once_per_node() {
    let chain: Vec<String> = (1..=30)
        .map(|i| {
            let before = i - 1;
            format!("_:c{i} owl:intersectionOf ( _:c{before} _:c{before} ) .")
        })
        .collect();
    let data = format!(
        "_:c0 owl:intersectionOf ( xsd:integer xsd:decimal ) . {} {}",
        chain.join(" "),
        data_existential("_:c30")
    );
    let started = std::time::Instant::now();
    no_place_complete(&data, "ASK { :a a :Aged }");
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(60), "{took:?}");
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
    let wrong = complete_anywhere(data, read);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// [`no_place_complete`]'s check, its wrong answers as lines.
fn complete_anywhere(data: &str, read: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        let (text, line, statuses, reported) = places(data, read, ql);
        // With the rewriting on, its report is one of the places.
        let complete = statuses.contains(&"complete");
        if !text.contains("false") || complete || (ql == QlRewritingMode::On && !reported) {
            wrong.push(line);
        }
    }
    wrong
}

/// An `owl2-dl` store loaded with `data` in one commit.
fn owl2_dl(data: &str) -> StoreService {
    let dl = pipeline();
    insert(&dl, data).expect("data");
    dl.store().as_ref().clone()
}

/// Under the OWL 2 Direct Semantics a literal denotes its value, so a pattern's constant
/// in another lexical form of a stored value matches it (OWL 2 Direct Semantics §2.2, the
/// literal interpretation `⋅LT`; seed 86's `"+2"`, and seed 113's `"01"` against `"1"` and
/// `"+1"`). Spec: an `owl2-dl` answer that misses such a match isn't `complete`, for a
/// constant in the pattern, through `VALUES` and through `BIND`, on the RL route (`"+2"`), on
/// the bounds' path (a key over `:d0` reads its values: seed 113), and for a value an
/// individual holds only through an equality ([`THROUGH_EQUALITY`]).
#[test]
fn owl2_dl_calls_no_answer_complete_that_a_constants_other_spelling_misses() {
    let seed_113 = ":d0 a owl:DatatypeProperty . :C0 owl:hasKey ( :d0 ) . \
                    :a1 a :C0 ; :d0 \"1\"^^xsd:integer . :a2 a :C0 ; :d0 \"+1\"^^xsd:integer .";
    let wrong = misses_and_says_complete(
        owl2_dl,
        &[
            (THROUGH_EQUALITY, ONE_AS_PLUS_ONE, &["a0", "a1", "a2"]),
            (THROUGH_EQUALITY, ONE_AS_ZERO_ONE, &["a0", "a1", "a2"]),
            (
                PLUS_TWO,
                "SELECT ?v { ?v :d1 \"+2\"^^xsd:integer }",
                &["a0"],
            ),
            (
                PLUS_TWO,
                "SELECT ?v { VALUES ?n { \"+2\"^^xsd:integer } ?v :d1 ?n }",
                &["a0"],
            ),
            (
                PLUS_TWO,
                "SELECT ?v { BIND(\"+2\"^^xsd:integer AS ?n) ?v :d1 ?n }",
                &["a0"],
            ),
            (
                seed_113,
                "SELECT ?v { ?v :d0 \"01\"^^xsd:integer }",
                &["a1", "a2"],
            ),
        ],
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// [`owl2_dl_calls_no_answer_complete_that_a_constants_other_spelling_misses`] under
/// `owl2-ql`, where the rewriting's `complete` is the same promise ("no certain answer under
/// the OWL 2 Direct Semantics is missing", docs/spec/reasoning-semantics.md).
#[test]
fn owl2_ql_calls_no_answer_complete_that_a_constants_other_spelling_misses() {
    let store = |data: &str| closure(data, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let wrong = misses_and_says_complete(
        store,
        &[
            (
                PLUS_TWO,
                "SELECT ?v { ?v :d1 \"+2\"^^xsd:integer }",
                &["a0"],
            ),
            (THROUGH_EQUALITY, ONE_AS_PLUS_ONE, &["a0", "a1", "a2"]),
            (THROUGH_EQUALITY, ONE_AS_ZERO_ONE, &["a0", "a1", "a2"]),
        ],
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Seed 2577: `:a0` is in `:C2`, so it is its own `:p1` value, and `:C2` allows at most one;
/// `:a1` and `:a2` are its `:p1` values too, so all three are one individual (OWL 2 Direct
/// Semantics, `ObjectMaxCardinality`), and `:a1` and `:a2` hold `:a0`'s `"1"` only through
/// that equality.
const THROUGH_EQUALITY: &str = ":p1 a owl:ObjectProperty . :d1 a owl:DatatypeProperty . \
     :C2 owl:equivalentClass [ a owl:Restriction ; owl:onProperty :p1 ; owl:hasValue :a0 ] ; \
     rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p1 ; owl:maxCardinality 1 ] . \
     :a0 a :C2 ; :p1 :a1 , :a2 ; :d1 \"1\"^^xsd:integer .";

/// [`THROUGH_EQUALITY`]'s value asked in two other spellings.
const ONE_AS_PLUS_ONE: &str = "SELECT ?v { ?v :d1 \"+1\"^^xsd:integer }";
const ONE_AS_ZERO_ONE: &str = "SELECT ?v { ?v :d1 \"01\"^^xsd:integer }";

/// The control: a string has one lexical form per value (`xsd:string`), and a language-tagged
/// string is its own value, so matching by term is matching by value. Spec: such a constant,
/// in the pattern or through `VALUES`, leaves the answer `complete` under `owl2-dl`, run and
/// planned.
#[test]
fn a_string_constant_leaves_an_owl2_dl_answer_complete() {
    let data = ":name a owl:DatatypeProperty . :a0 :name \"Rex\" , \"Rex\"@en .";
    let mut wrong = Vec::new();
    for read in [
        "SELECT ?v { ?v :name \"Rex\" }",
        "SELECT ?v { ?v :name \"Rex\"@en }",
        "SELECT ?v { VALUES ?n { \"Rex\" } ?v :name ?n }",
    ] {
        let (rows, status) = answer(&owl2_dl(data), read);
        eprintln!("{read}: {rows} {status:?}");
        if !rows.contains("example.com/a0") || status != ["complete"; 2] {
            wrong.push(format!("{read}: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The control for a typed constant that no pattern matches: bound to a variable that is
/// only compared in a `FILTER` (which SPARQL does by value), or only projected
/// (`(1 AS ?k)`), it misses no row, whatever its spelling. Spec: under `owl2-dl` and
/// `owl2-ql` such an answer holds its row and is `complete`, run and planned.
#[test]
fn a_typed_constant_no_pattern_matches_leaves_an_answer_complete() {
    let data = ":age a owl:DatatypeProperty . :a0 :age 7 .";
    let ql = |data: &str| closure(data, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let mut wrong = Vec::new();
    for read in [
        "SELECT ?v { ?v :age ?a BIND(5 AS ?n) FILTER(?a > ?n) }",
        "SELECT ?v (1 AS ?k) { ?v :age ?a }",
    ] {
        for (regime, store) in [("owl2-dl", owl2_dl(data)), ("owl2-ql", ql(data))] {
            let (rows, status) = answer(&store, read);
            eprintln!("{regime} {read}: {rows} {status:?}");
            if !rows.contains("example.com/a0") || status != ["complete"; 2] {
                wrong.push(format!("{regime} {read}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A value the query computes and then matches against the data: `2 + 3` is the integer 5,
/// which `:a0`'s `"05"` is too, and a copy (`BIND(?c AS ?d)`) carries the same value. Under
/// the OWL 2 Direct Semantics `:a0` is a certain answer of both. Spec: under `owl2-dl` and
/// `owl2-ql` an answer that misses it isn't `complete`, run and planned.
#[test]
fn no_answer_is_complete_that_a_computed_values_other_spelling_misses() {
    let data = ":p a owl:DatatypeProperty . :a0 :p \"05\"^^xsd:integer .";
    let reads: [(&str, &str, &[&str]); 2] = [
        (data, "SELECT ?x { BIND(2 + 3 AS ?c) ?x :p ?c }", &["a0"]),
        (
            data,
            "SELECT ?x { BIND(5 AS ?c) BIND(?c AS ?d) ?x :p ?d }",
            &["a0"],
        ),
    ];
    let ql = |data: &str| closure(data, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let mut wrong = misses_and_says_complete(owl2_dl, &reads);
    wrong.extend(misses_and_says_complete(ql, &reads));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The known over-mark: a computed value is counted whatever its type, so a string or an
/// IRI the query computes and matches against the data lowers the answer, though a string
/// or an IRI has one spelling and the match misses nothing. Spec (docs/limits.md): under
/// `owl2-dl` and `owl2-ql` such an answer holds its row and is `sound-only`, run and planned.
#[test]
fn a_computed_string_or_iri_at_a_patterns_end_is_marked_sound_only() {
    let data = ":name a owl:DatatypeProperty . :a0 :name \"Rex\" .";
    let ql = |data: &str| closure(data, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let mut wrong = Vec::new();
    for read in [
        "SELECT ?v { BIND(CONCAT(\"R\", \"ex\") AS ?n) ?v :name ?n }",
        "SELECT ?v { BIND(IRI(\"http://example.com/a0\") AS ?v) ?v :name \"Rex\" }",
    ] {
        for (regime, store) in [("owl2-dl", owl2_dl(data)), ("owl2-ql", ql(data))] {
            let (rows, status) = answer(&store, read);
            eprintln!("{regime} {read}: {rows} {status:?}");
            if !rows.contains("example.com/a0") || status != ["sound-only"; 2] {
                wrong.push(format!("{regime} {read}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `read` over `data`'s owl2-rl closure with the rewriting `ql`: its answer, a printed line
/// with what each place (the run, its rewriting, the plan, the plan's rewriting) calls it,
/// those four names (`none`: no status), and whether the run has the rewriting's report.
fn places(
    data: &str,
    read: &str,
    ql: QlRewritingMode,
) -> (String, String, [&'static str; 4], bool) {
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
    (text, line, places.map(|(_, c)| name(c)), run.ql.is_some())
}

/// cls-maxc2 names its cardinality as the term `"1"^^xsd:nonNegativeInteger`, and the rule
/// set leaves out dt-eq: under owl2-rl only that spelling is read, and plain Turtle
/// `owl:maxCardinality 1` (an xsd:integer) or `"01"^^xsd:nonNegativeInteger` doesn't
/// equate `:x` and `:y`.
#[test]
fn owl2_rl_reads_only_the_mappings_spelling_of_a_max_cardinality() {
    let mut wrong = Vec::new();
    for (card, read) in [
        ("\"1\"^^xsd:nonNegativeInteger", true),
        ("1", false),
        ("\"01\"^^xsd:nonNegativeInteger", false),
    ] {
        let data = format!(
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
               owl:maxCardinality {card} ] . \
             :p a owl:ObjectProperty . :c a :C ; :p :x , :y ."
        );
        let store = closure(&data, Ruleset::Owl2Rl, QlRewritingMode::Auto);
        let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ :x owl:sameAs :y }}"));
        let run = store.execute_query(&request).expect("answered");
        let text = String::from_utf8_lossy(&run.payload).into_owned();
        eprintln!("owl2-rl maxCardinality {card}: ASK {{ :x owl:sameAs :y }} answers {text}");
        if text.contains("true") != read {
            wrong.push(format!("{card}: {text}"));
        }
    }
    assert!(wrong.is_empty(), "spellings read otherwise: {wrong:#?}");
}

/// An `owl2-dl` store loaded with `data` and an existential superclass, which is outside
/// OWL 2 RL: off the RL route.
fn owl2_dl_off_route(data: &str) -> StoreService {
    owl2_dl(&format!(
        "{data} :r a owl:ObjectProperty . :E rdfs:subClassOf \
         [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :E ] ."
    ))
}

/// The stores whose `complete` promises that no certain answer under the OWL 2 Direct
/// Semantics is missing: `owl2-dl` on and off the RL route, `owl2-ql`, and `owl2-rl` with
/// the QL rewriting on.
type Promising = [(&'static str, fn(&str) -> StoreService); 4];
const PROMISING: Promising = [
    ("owl2-dl", owl2_dl),
    ("owl2-dl off the RL route", owl2_dl_off_route),
    ("owl2-ql", |d| {
        closure(d, Ruleset::Owl2Ql, QlRewritingMode::Auto)
    }),
    ("owl2-rl rewriting on", |d| {
        closure(d, Ruleset::Owl2Rl, QlRewritingMode::On)
    }),
];

/// `:a`'s and `:b`'s value of `:age` is the integer 7, written `"07"` and `7`.
const SEVEN: &str = ":age a owl:DatatypeProperty . :a :age \"07\"^^xsd:integer . :b :age 7 .";

/// Whether rows (SPARQL JSON results) are the ones OWL 2 gives.
type Right = fn(&str) -> bool;

/// How many rows `rows` (SPARQL JSON results) binds `var` in.
fn bound(rows: &str, var: &str) -> usize {
    rows.matches(&format!("\"{var}\":{{")).count()
}

/// The shapes that compare data values by term: a join on one (over a declared and an
/// undeclared property, on a blank node, written or made by a path's sequence, and at the
/// start of a path with a reversed step, under `+` or in an alternative), and `DISTINCT`,
/// `GROUP BY` and `COUNT(DISTINCT)` over one (and `DISTINCT` and `GROUP BY` over an alias of
/// one). Each `(shape, data, read, right)`, `right` holding of the rows OWL 2 gives by value
/// over [`SEVEN`]: `:a` and `:b` share their value, which is one value.
const SHAPES: [(&str, &str, &str, Right); 11] = [
    (
        "a join",
        SEVEN,
        "SELECT ?x ?y { ?x :age ?n . ?y :age ?n FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "a join over an undeclared property",
        ":a :size \"07\"^^xsd:integer . :b :size 7 .",
        "SELECT ?x ?y { ?x :size ?n . ?y :size ?n FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "a join on a blank node",
        SEVEN,
        "SELECT ?x ?y { ?x :age/^:age ?y FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "a join on a written blank node",
        SEVEN,
        "SELECT ?x ?y { ?x :age _:v . ?y :age _:v FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "a join at a reversed path's start",
        SEVEN,
        "SELECT ?x ?y { ?n (^:age)+ ?x . ?n (^:age)+ ?y FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "a join at an alternative's reversed start",
        SEVEN,
        "SELECT ?x ?y { ?n (^:age|:p) ?x . ?n (^:age|:p) ?y FILTER(?x != ?y) }",
        |rows| rows.contains("example.com/a\""),
    ),
    (
        "DISTINCT",
        SEVEN,
        "SELECT DISTINCT ?n { ?x :age ?n }",
        |rows| bound(rows, "n") == 1,
    ),
    (
        "GROUP BY",
        SEVEN,
        "SELECT ?n (COUNT(?x) AS ?c) { ?x :age ?n } GROUP BY ?n",
        |rows| bound(rows, "n") == 1,
    ),
    (
        "COUNT(DISTINCT)",
        SEVEN,
        "SELECT (COUNT(DISTINCT ?n) AS ?c) { ?x :age ?n }",
        |rows| rows.contains("\"value\":\"1\""),
    ),
    (
        "DISTINCT over an alias",
        SEVEN,
        "SELECT DISTINCT (?n AS ?m) { ?x :age ?n }",
        |rows| bound(rows, "m") == 1,
    ),
    (
        "GROUP BY an alias",
        SEVEN,
        "SELECT ?g (COUNT(?x) AS ?c) { ?x :age ?n } GROUP BY (?n AS ?g)",
        |rows| bound(rows, "g") == 1,
    ),
];

/// Under the OWL 2 Direct Semantics a literal denotes its value, so `"07"` and `7` (one
/// `xsd:integer`) are one value; the store compares the terms as written. Spec: a join on a
/// data value, and `DISTINCT`, `GROUP BY` or `COUNT(DISTINCT)` over one, read through a
/// property that holds a value other than a plain or language-tagged string, isn't
/// `complete` unless its rows are the ones OWL 2 gives by value, under each regime that
/// promises `complete`, run and planned.
#[test]
fn no_answer_that_compares_data_values_by_term_is_complete_and_wrong() {
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for (shape, data, read, right) in SHAPES {
            let (rows, status) = answer(&store(data), read);
            eprintln!("{regime}, {shape}: {rows} {status:?}");
            if !right(&rows) && status.contains(&"complete") {
                wrong.push(format!("{regime}, {shape}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The controls: a property that holds only IRIs, or only strings (one spelling per value),
/// compares values by term as OWL 2 does, and so does a property beside another that holds a
/// number. Spec: such a join, `DISTINCT` or `GROUP BY` keeps its rows and `complete`, run
/// and planned, under each regime that promises it.
#[test]
fn comparing_iris_or_strings_by_term_leaves_an_answer_complete() {
    let knows = ":knows a owl:ObjectProperty . :a :knows :c . :b :knows :c .";
    let names = ":name a owl:DatatypeProperty . :a :name \"Rex\" . :b :name \"Rex\" , \"Max\"@en .";
    let beside = ":name a owl:DatatypeProperty . :a :name \"Rex\" ; :age 7 . :b :name \"Rex\" .";
    let pair = |rows: &str| rows.contains("example.com/a\"") && rows.contains("example.com/b\"");
    let cases: [(&str, &str, Right); 7] = [
        (
            knows,
            "SELECT ?x ?y { ?x :knows ?z . ?y :knows ?z FILTER(?x != ?y) }",
            pair,
        ),
        (knows, "SELECT DISTINCT ?z { ?x :knows ?z }", |r| {
            bound(r, "z") == 1
        }),
        (
            knows,
            "SELECT ?z (COUNT(?x) AS ?c) { ?x :knows ?z } GROUP BY ?z",
            |r| bound(r, "z") == 1,
        ),
        (
            names,
            "SELECT ?x ?y { ?x :name ?n . ?y :name ?n FILTER(?x != ?y) }",
            pair,
        ),
        (names, "SELECT DISTINCT ?n { ?x :name ?n }", |r| {
            bound(r, "n") == 2
        }),
        (
            names,
            "SELECT ?n (COUNT(?x) AS ?c) { ?x :name ?n } GROUP BY ?n",
            |r| bound(r, "n") == 2,
        ),
        (beside, "SELECT DISTINCT ?n { ?x :name ?n }", |r| {
            bound(r, "n") == 1
        }),
    ];
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for (data, read, right) in cases {
            let (rows, status) = answer(&store(data), read);
            eprintln!("{regime}, {read}: {rows} {status:?}");
            if !right(&rows) || status != ["complete"; 2] {
                wrong.push(format!("{regime}, {read}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The controls under `owl2-ql` for properties no ontology declares: one that holds only
/// IRIs and one that holds only strings. Spec: a join and a `DISTINCT` over either keep
/// their rows and `complete`, run and planned.
#[test]
fn owl2_ql_keeps_complete_over_undeclared_properties_of_iris_or_strings() {
    let store = |data: &str| closure(data, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let pair = |rows: &str| rows.contains("example.com/a\"") && rows.contains("example.com/b\"");
    let cases: [(&str, &str, Right); 4] = [
        (
            ":a :q :c . :b :q :c .",
            "SELECT ?x ?y { ?x :q ?z . ?y :q ?z FILTER(?x != ?y) }",
            pair,
        ),
        (
            ":a :q :c . :b :q :c .",
            "SELECT DISTINCT ?z { ?x :q ?z }",
            |r| bound(r, "z") == 1,
        ),
        (
            ":a :s \"Rex\" . :b :s \"Rex\" .",
            "SELECT ?x ?y { ?x :s ?n . ?y :s ?n FILTER(?x != ?y) }",
            pair,
        ),
        (
            ":a :s \"Rex\" . :b :s \"Rex\" .",
            "SELECT DISTINCT ?n { ?x :s ?n }",
            |r| bound(r, "n") == 1,
        ),
    ];
    let mut wrong = Vec::new();
    for (data, read, right) in cases {
        let (rows, status) = answer(&store(data), read);
        eprintln!("{read}: {rows} {status:?}");
        if !right(&rows) || status != ["complete"; 2] {
            wrong.push(format!("{read}: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A query prepared once and run after each commit: the property it reads holds only
/// strings, then also `"07"` and `7` (the first values other than strings), then `7` alone,
/// then strings again. Spec: its `DISTINCT` is `complete` exactly while the property holds
/// only strings, run and planned: the status follows the snapshot read, not the one the
/// query was prepared on.
#[test]
fn a_prepared_query_is_marked_by_what_its_property_holds_at_each_run() {
    let dl = pipeline();
    insert(
        &dl,
        ":v a owl:DatatypeProperty . :x :v \"a\" . :y :v \"b\" .",
    )
    .expect("data");
    let store = dl.store();
    let request = SparqlQueryRequest::all(format!("{PREFIXES}SELECT DISTINCT ?n {{ ?s :v ?n }}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let status = |label: &str| -> [&'static str; 2] {
        let mut run = None;
        store
            .run_query_reporting(&prepared, &CancellationToken::new(), Vec::new(), |s| {
                run = s
            })
            .expect("ran");
        let plan = store
            .plan_query(&prepared, &CancellationToken::new())
            .expect("planned")
            .completeness;
        let name = |c: Option<nrese_sparql::Completeness>| c.map_or("none", |c| c.as_str());
        let status = [name(run), name(plan)];
        eprintln!("{label}: {status:?}");
        status
    };
    let mut seen = vec![("strings", status("strings"))];
    crate::dl::update(&dl, "INSERT DATA { :z :v \"07\"^^xsd:integer . :w :v 7 }").expect("typed");
    seen.push(("\"07\" and 7", status("\"07\" and 7")));
    crate::dl::update(&dl, "DELETE DATA { :z :v \"07\"^^xsd:integer }").expect("one left");
    seen.push(("7", status("7")));
    crate::dl::update(&dl, "DELETE DATA { :w :v 7 }").expect("strings again");
    seen.push(("strings again", status("strings again")));
    let expected = ["complete", "sound-only", "sound-only", "complete"];
    let wrong: Vec<_> = seen
        .iter()
        .zip(expected)
        .filter(|((_, got), want)| got != &[*want; 2])
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// `"Rex"` stored as a type derived from `xsd:string` and as a plain string: the types share
/// xsd:string's value space, so OWL 2 takes `"Rex"^^xsd:token` and `"Rex"` as one value.
const DERIVED: [&str; 2] = [
    ":s a owl:DatatypeProperty . :a :s \"Rex\"^^xsd:token .",
    ":s a owl:DatatypeProperty . :a :s \"Rex\"^^xsd:normalizedString .",
];

/// The reads that match `"Rex"` against `:s`: as a pattern's object, and bound by `VALUES`
/// or `BIND`.
const NAMES_REX: [&str; 3] = [
    "SELECT ?x { ?x :s \"Rex\" }",
    "SELECT ?x { VALUES ?n { \"Rex\" } ?x :s ?n }",
    "SELECT ?x { BIND(\"Rex\" AS ?n) ?x :s ?n }",
];

/// The store matches the plain string as written and misses `:a`'s value spelled with a
/// derived string type. Spec: each of [`NAMES_REX`] over [`DERIVED`] finds `:a` or isn't
/// `complete`, run and planned, under each regime that promises `complete`.
#[test]
fn a_plain_string_misses_no_derived_string_type_and_says_complete() {
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let cases: Vec<_> = DERIVED
            .iter()
            .flat_map(|data| NAMES_REX.map(|read| (*data, read, &["a"][..])))
            .collect();
        for missed in misses_and_says_complete(store, &cases) {
            wrong.push(format!("{regime}: {missed}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The control: a plain string stored and asked is one term. Spec: each of [`NAMES_REX`]
/// finds `:a` and stays `complete`, run and planned, under each regime that promises it.
#[test]
fn a_plain_string_asked_where_one_is_stored_stays_complete() {
    let data = ":s a owl:DatatypeProperty . :a :s \"Rex\" .";
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for read in NAMES_REX {
            let (rows, status) = answer(&store(data), read);
            eprintln!("{regime}, {read}: {rows} {status:?}");
            if !rows.contains("example.com/a\"") || status != ["complete"; 2] {
                wrong.push(format!("{regime}, {read}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An aggregate over a data value compared by term gives a wrong number or wrong groups
/// (`COUNT(DISTINCT)` counts `"07"` and `7` as two values; OWL 2 as one), not only missing
/// rows. Spec: the `GROUP BY` (of a variable or an alias) and `COUNT(DISTINCT)` shapes of
/// [`SHAPES`] are `unsound`, run and planned, under each regime that promises `complete`.
#[test]
fn an_aggregate_over_data_values_compared_by_term_is_unsound() {
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for (shape, data, read, _) in SHAPES {
            if !matches!(shape, "GROUP BY" | "COUNT(DISTINCT)" | "GROUP BY an alias") {
                continue;
            }
            let (rows, status) = answer(&store(data), read);
            eprintln!("{regime}, {shape}: {rows} {status:?}");
            if status != ["unsound"; 2] {
                wrong.push(format!("{regime}, {shape}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Two marks the check makes where no row is wrong, kept as they are: a join of a property
/// that holds only strings with one that holds a number (no string equals a number), and a
/// string constant against a property that holds strings and numbers. The check asks per
/// property whether it holds a value other than a string, not which value meets which.
/// Spec: under `owl2-dl`, each keeps its rows and is `sound-only`, run and planned.
#[test]
fn a_string_beside_a_number_is_sound_only_though_no_row_is_missed() {
    let cases: [(&str, &str, Right); 2] = [
        (
            ":name a owl:DatatypeProperty . :age a owl:DatatypeProperty . \
             :a :name \"7\" . :b :age 7 .",
            "SELECT ?x ?y { ?x :name ?v . ?y :age ?v }",
            |rows| !rows.contains("example.com/"),
        ),
        (
            ":p a owl:DatatypeProperty . :a :p \"Rex\" . :b :p 7 .",
            "SELECT ?x { ?x :p \"Rex\" }",
            |rows| rows.contains("example.com/a\"") && !rows.contains("example.com/b\""),
        ),
    ];
    let mut wrong = Vec::new();
    for (data, read, right) in cases {
        let (rows, status) = answer(&owl2_dl(data), read);
        eprintln!("{read}: {rows} {status:?}");
        if !right(&rows) || status != ["sound-only"; 2] {
            wrong.push(format!("{read}: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The control for a path's start: a path with no reversed step starts at a subject (an IRI
/// or a blank node, never a data value), so a join there compares no value, though the
/// store holds a number. Spec: the join keeps its rows and `complete`, run and planned,
/// under each regime that promises it.
#[test]
fn a_join_at_the_start_of_a_path_without_a_reversed_step_stays_complete() {
    let data = ":knows a owl:ObjectProperty . :c :knows :a , :b . :a :age 7 .";
    let read = "SELECT ?x ?y { ?z :knows+ ?x . ?z :knows+ ?y FILTER(?x != ?y) }";
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let (rows, status) = answer(&store(data), read);
        eprintln!("{regime}: {rows} {status:?}");
        let pair = rows.contains("example.com/a\"") && rows.contains("example.com/b\"");
        if !pair || status != ["complete"; 2] {
            wrong.push(format!("{regime}: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The control for a negated set with a reversed member: a join at its start compares a data
/// value, and every route already says no more than `sound-only` there (a negated set reads
/// the vocabulary OWL 2 entails, and the parser turns `!(^:p)` into a path ending at `?n`).
/// Spec: the join over [`SEVEN`] isn't `complete`, run and planned, under each regime that
/// promises `complete`.
#[test]
fn a_join_at_a_negated_reversed_sets_start_is_not_complete() {
    let read = "SELECT ?x ?y { ?n !(^:p) ?x . ?n !(^:p) ?y FILTER(?x != ?y) }";
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let (rows, status) = answer(&store(SEVEN), read);
        eprintln!("{regime}: {status:?}");
        if status.contains(&"complete") {
            wrong.push(format!("{regime}: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A sequence inside `+`, `?` or an alternative stays a path, and the path meets two `:age`
/// edges inside it: `:a (:age/^:age)+ ?y` reaches `:b` through the value `:a` and `:b`
/// share, written `"07"` and `7` ([`SEVEN`]), or `7.0` and `7` (a decimal and an integer of
/// one value); the store follows terms. Spec: each read finds `:b` or isn't `complete`, and
/// its count is 1 or `unsound`, run and planned, under each regime that promises `complete`.
#[test]
fn a_join_inside_a_path_compares_data_values() {
    let data = [
        ("\"07\" and 7", SEVEN),
        (
            "7.0 and 7",
            ":age a owl:DatatypeProperty . :a :age 7.0 . :b :age 7 .",
        ),
    ];
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for (values, data) in data {
            let store = store(data);
            for path in ["(:age/^:age)+", "(:age/^:age)?", "(:age/^:age|:p)"] {
                let rows = format!("SELECT ?y {{ :a {path} ?y FILTER(?y != :a) }}");
                let (rows, status) = answer(&store, &rows);
                eprintln!("{regime}, {path} over {values}: {rows} {status:?}");
                if !rows.contains("example.com/b\"") && status.contains(&"complete") {
                    wrong.push(format!("{regime}, {path} over {values}: {rows} {status:?}"));
                }
                let count = format!("SELECT (COUNT(?y) AS ?c) {{ :a {path} ?y FILTER(?y != :a) }}");
                let (rows, status) = answer(&store, &count);
                eprintln!("{regime}, its count over {values}: {rows} {status:?}");
                if !rows.contains("\"value\":\"1\"") && status != ["unsound"; 2] {
                    wrong.push(format!(
                        "{regime}, {path}'s count over {values}: {rows} {status:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A `DISTINCT` in a subquery whose variable the outer query projects away: over [`SEVEN`]
/// it keeps `"07"` and `7`, one value to OWL 2, as two rows. Spec: an outer `COUNT(*)` over
/// it is 1 or `unsound`, and an outer join with it returns `:a` and `:b` once each or isn't
/// `complete`, run and planned, under each regime that promises `complete`.
#[test]
fn a_subquerys_distinct_over_data_values_is_judged_by_its_own_projection() {
    let count = "SELECT (COUNT(*) AS ?c) { { SELECT DISTINCT ?n { ?x :age ?n } } }";
    let join = "SELECT ?x { { SELECT DISTINCT ?n { ?y :age ?n } } ?x :age ?m }";
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let store = store(SEVEN);
        let (rows, status) = answer(&store, count);
        eprintln!("{regime}, the count: {rows} {status:?}");
        if !rows.contains("\"value\":\"1\"") && status != ["unsound"; 2] {
            wrong.push(format!("{regime}, the count: {rows} {status:?}"));
        }
        let (rows, status) = answer(&store, join);
        eprintln!("{regime}, the join: {rows} {status:?}");
        if bound(&rows, "x") != 2 && status.contains(&"complete") {
            wrong.push(format!("{regime}, the join: {rows} {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A path over a property that holds only IRIs ends at an IRI, though another property
/// holds a number: no data value is compared. Spec: a `DISTINCT` and a `COUNT(DISTINCT)`
/// over its end keep their rows and `complete`, run and planned, under each regime that
/// promises it.
#[test]
fn a_paths_end_over_iris_stays_complete_beside_a_number() {
    let data = ":knows a owl:ObjectProperty . :a :knows :b . :b :knows :c . :z :age 7 .";
    let cases: [(&str, Right); 2] = [
        ("SELECT DISTINCT ?y { ?x :knows+ ?y }", |r| {
            bound(r, "y") == 2
        }),
        ("SELECT (COUNT(DISTINCT ?y) AS ?c) { :a :knows+ ?y }", |r| {
            r.contains("\"value\":\"2\"")
        }),
    ];
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let store = store(data);
        for (read, right) in cases {
            let (rows, status) = answer(&store, read);
            eprintln!("{regime}, {read}: {rows} {status:?}");
            if !right(&rows) || status != ["complete"; 2] {
                wrong.push(format!("{regime}, {read}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A path that can match zero steps (`*`, `?`, or an alternative with one) pairs every term
/// of the graph with itself, data values included, whatever its predicates hold: over
/// [`SEVEN`], `?x :knows* ?y` binds `?x` and `?y` to `"07"` and to `7`, one value to OWL 2.
/// Spec: a `DISTINCT` over either end doesn't return both or isn't `complete`, run and
/// planned, under each regime that promises `complete`.
#[test]
fn a_zero_length_paths_ends_can_be_data_values() {
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        let store = store(SEVEN);
        for read in [
            "SELECT DISTINCT ?x { ?x :knows* ?y }",
            "SELECT DISTINCT ?x { ?x (:knows|:p?) ?y }",
            "SELECT DISTINCT ?y { ?x :knows* ?y }",
        ] {
            let (rows, status) = answer(&store, read);
            eprintln!("{regime}, {read}: {rows} {status:?}");
            let both = rows.contains("\"value\":\"07\"") && rows.contains("\"value\":\"7\"");
            if both && status.contains(&"complete") {
                wrong.push(format!("{regime}, {read}: {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A reader of the default graph and `:open`, with the inferences their graphs support, and
/// not of `:secret`, under `owl2-ql` and `owl2-rl` with the rewriting on: whether `:p` holds
/// a number in `:secret` is hidden from them. Spec: their rows and their entire assessment
/// (status and every reason, of the run, its header, its QL report and the plan, a prepared
/// query run twice included) are the same with and without `:z :p 7` in `:secret`.
#[test]
fn a_readers_status_doesnt_follow_what_they_cant_read() {
    use std::sync::Arc;

    use nrese_sparql::{Completeness, GraphAccess};
    use nrese_store::{ReadScope, SparqlUpdateRequest};

    use crate::support::Writes;

    let said = |c: Option<&Completeness>| {
        c.map_or("none".to_owned(), |c| {
            let reasons: Vec<_> = c.reasons.iter().map(ToString::to_string).collect();
            format!("{} {reasons:?}", c.header())
        })
    };
    let seen = |ruleset: Ruleset, ql: QlRewritingMode, secret: &str| {
        let store = StoreService::new(StoreConfig {
            ql_rewriting: ql,
            ..StoreConfig::in_memory()
        })
        .unwrap();
        let update = format!(
            "{PREFIXES}INSERT DATA {{ :p a owl:DatatypeProperty . GRAPH :open {{ :a :p \"Rex\" }} \
             GRAPH :secret {{ :y :p \"Max\" {secret} }} }}"
        );
        store
            .execute_update(&SparqlUpdateRequest::new(update))
            .unwrap();
        store.rematerialise(ruleset).unwrap();
        store.use_reasoning_rules(Some(ruleset.into()));
        let scope = ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec!["http://example.com/open".to_owned()],
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        }));
        let read = format!("{PREFIXES}SELECT DISTINCT ?n {{ GRAPH :open {{ ?s :p ?n }} }}");
        let request = SparqlQueryRequest::new(read, scope);
        let result = store.execute_query(&request).expect("answered");
        let mut seen = vec![
            String::from_utf8(result.payload).unwrap(),
            said(result.completeness.as_ref()),
            said(result.ql.as_ref().map(|r| &r.completeness)),
        ];
        let prepared = store.prepare_query(&request).expect("prepared");
        for _ in 0..2 {
            let (mut run, mut rows) = (None, Vec::new());
            store
                .run_query_reporting(&prepared, &CancellationToken::new(), &mut rows, |s| run = s)
                .expect("ran");
            let plan = store
                .plan_query(&prepared, &CancellationToken::new())
                .expect("planned");
            seen.push(String::from_utf8(rows).unwrap());
            seen.push(said(run.as_ref()));
            seen.push(said(plan.completeness.as_ref()));
        }
        seen
    };
    let mut wrong = Vec::new();
    for (regime, ruleset, ql) in [
        ("owl2-ql", Ruleset::Owl2Ql, QlRewritingMode::Auto),
        ("owl2-rl rewriting on", Ruleset::Owl2Rl, QlRewritingMode::On),
    ] {
        let (hidden, without) = (seen(ruleset, ql, ". :z :p 7"), seen(ruleset, ql, ""));
        eprintln!("{regime}, without: {without:#?}\n{regime}, with: {hidden:#?}");
        if hidden != without {
            wrong.push(format!("{regime}: without {without:#?}, with {hidden:#?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// OWL 2 defines neither triple terms nor base directions (RDF 1.2), so its regimes can't say
/// what a complete answer that uses one is: `<<( :s :age "07"^^xsd:integer )>>` is the stored
/// `<<( :s :age 7 )>>` if the value counts and isn't if the spelling does (RDF 1.2 Semantics
/// §5, §7.1), and a variable inside one joins by either. Spec: under each regime that promises
/// `complete`, run and planned, no answer is `complete` to a query with a triple term (a typed
/// or plain constant inside it, bound by `VALUES`, or a variable inside it joined outside) or
/// a base-direction literal, over a store with no triple term; or to a reader of everything
/// over a store that holds one, reached only through a variable.
#[test]
fn no_query_with_a_triple_term_is_complete_under_owl_2() {
    let plain = ":age a owl:DatatypeProperty . :y :age \"07\"^^xsd:integer . :a :name \"Rex\" .";
    let holds = ":a :says <<( :s :age 7 )>> . :b :says <<( :s :age \"07\"^^xsd:integer )>> .";
    let typed = "<<( :s :age \"07\"^^xsd:integer )>>";
    let cases = [
        (
            plain,
            "a typed constant",
            format!("SELECT ?x {{ ?x :says {typed} }}"),
        ),
        (
            plain,
            "a typed constant bound by VALUES",
            format!("SELECT ?x {{ VALUES ?t {{ {typed} }} ?x :says ?t }}"),
        ),
        (
            plain,
            "a variable joined outside",
            "SELECT ?x ?y { ?x :says <<( :s :age ?n )>> . ?y :age ?n }".to_owned(),
        ),
        (
            plain,
            "a plain string, which OWL 2 doesn't define inside a triple term either",
            "SELECT ?x { ?x :says <<( :s :name \"Rex\" )>> }".to_owned(),
        ),
        (
            plain,
            "a base-direction literal",
            "SELECT ?x { ?x :name \"Rex\"@ar--rtl }".to_owned(),
        ),
        (
            holds,
            "a triple term reached through a variable",
            "SELECT DISTINCT ?t { ?x :says ?t }".to_owned(),
        ),
    ];
    let mut wrong = Vec::new();
    for (regime, store) in PROMISING {
        for (data, case, read) in &cases {
            let (rows, status) = answer(&store(data), read);
            eprintln!("{regime}, {case}: {rows} {status:?}");
            if status.contains(&"complete") {
                wrong.push(format!("{regime}, {case}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A reader of `:open` (and of the default graph, or not), with the inferences their graphs
/// support, under `owl2-ql` and `owl2-rl` with the rewriting on. A triple term in a graph
/// the reader can't read takes no part in their answers, so it has no say in their status.
/// Spec: the status is `complete` (run and planned, rows kept) where no graph they read holds
/// a triple term: none in the store, one only in a hidden named graph, one only in the
/// default graph when they can't read it; and not `complete` where one they read does: in
/// `:open`, in the default graph they read, or inferred where they see it (`rdfs:subPropertyOf`
/// copies the object, so `:claims` holds the triple term).
#[test]
fn a_readers_status_follows_the_triple_terms_they_may_read() {
    use std::sync::Arc;

    use nrese_sparql::GraphAccess;
    use nrese_store::{ReadScope, SparqlUpdateRequest};

    use crate::support::Writes;

    let fact = "GRAPH :open { :a :p \"Rex\" }";
    let term = ":b :says <<( :s :age 7 )>>";
    let read = "SELECT ?x ?n { GRAPH :open { ?x :p ?n } }";
    let cases = [
        (true, "no triple term anywhere", fact.to_owned(), read, true),
        (
            true,
            "a triple term only in a graph they can't read",
            format!("{fact} GRAPH :secret {{ {term} }}"),
            read,
            true,
        ),
        (
            false,
            "a triple term only in the default graph, which they can't read",
            format!("{fact} {term}"),
            read,
            true,
        ),
        (
            true,
            "a triple term in a graph they read",
            format!("GRAPH :open {{ :a :p \"Rex\" . {term} }}"),
            read,
            false,
        ),
        (
            true,
            "a triple term in the default graph they read",
            format!("{fact} {term}"),
            read,
            false,
        ),
        (
            true,
            "an inferred triple term they see",
            format!("GRAPH :open {{ :says rdfs:subPropertyOf :claims . {term} }}"),
            "SELECT ?x ?t { ?x :claims ?t }",
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (regime, ruleset, ql) in [
        ("owl2-ql", Ruleset::Owl2Ql, QlRewritingMode::Auto),
        ("owl2-rl rewriting on", Ruleset::Owl2Rl, QlRewritingMode::On),
    ] {
        for (default_graph, case, data, read, complete) in &cases {
            let store = StoreService::new(StoreConfig {
                ql_rewriting: ql,
                ..StoreConfig::in_memory()
            })
            .unwrap();
            let update = format!("{PREFIXES}INSERT DATA {{ :p a owl:DatatypeProperty . {data} }}");
            store
                .execute_update(&SparqlUpdateRequest::new(update))
                .unwrap();
            store.rematerialise(ruleset).unwrap();
            store.use_reasoning_rules(Some(ruleset.into()));
            let scope = ReadScope::Graphs(Arc::new(GraphAccess {
                graphs: vec!["http://example.com/open".to_owned()],
                default_graph: *default_graph,
                inferred: true,
                inferred_by_support: true,
                ..GraphAccess::default()
            }));
            let request = SparqlQueryRequest::new(format!("{PREFIXES}{read}"), scope);
            let result = store.execute_query(&request).expect("answered");
            let prepared = store.prepare_query(&request).expect("prepared");
            let planned = store
                .plan_query(&prepared, &CancellationToken::new())
                .expect("planned");
            let status = [result.completeness, planned.completeness]
                .map(|c| c.map_or("none", |c| c.as_str()));
            let rows = String::from_utf8(result.payload).unwrap();
            eprintln!("{regime}, {case}: {rows} {status:?}");
            let kept = rows.contains("example.com/a\"") || rows.contains("example.com/b\"");
            let right = match complete {
                true => status == ["complete"; 2],
                false => !status.contains(&"complete"),
            };
            if !kept || !right {
                wrong.push(format!("{regime}, {case}: {rows} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
