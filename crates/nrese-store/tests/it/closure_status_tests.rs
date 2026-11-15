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

/// The rewriting's own report (`ql.completeness`) says no more than the answer's status:
/// a denied read is `complete` in neither, run, planned or explained.
#[test]
fn the_rewritings_report_calls_no_denied_read_complete() {
    let store = closure(DATA, Ruleset::Owl2Ql, QlRewritingMode::Auto);
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ :fresh a owl:Thing }}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let cancel = CancellationToken::new();
    let run = store.execute_query(&request).expect("answered");
    let planned = store.plan_query(&prepared, &cancel).expect("planned");
    let explained = store.explain_query(&prepared, &cancel).expect("explained");
    for (how, status, report) in [
        ("run", run.completeness, run.ql.map(|q| q.completeness)),
        (
            "planned",
            planned.completeness,
            planned.ql.map(|q| q.completeness),
        ),
        (
            "explained",
            explained.completeness,
            explained.ql.map(|q| q.completeness),
        ),
    ] {
        let report = report.unwrap_or_else(|| panic!("{how}: no report of the rewriting"));
        assert_eq!(report.as_str(), "sound-only", "{how}: ql.completeness");
        assert_eq!(status.map(|s| s.as_str()), Some("sound-only"), "{how}");
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
/// DL path as on the others: LIMIT or OFFSET without ORDER BY, GRAPH, SERVICE and DESCRIBE
/// are monotone in the data. Adding the statements an incomplete answer lacks can only add
/// rows or triples; it can't make a returned row wrong. So an answer incomplete for its
/// own reasons (here candidates the bounds leave unresolved) is `sound-only` under them.
/// An ordered slice differs: a missing row can push a returned one out, so it is
/// `unsound`. Changing this list means arguing with these cases.
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
        // Sound under the monotone ones: only y, which the RL closure has, is returned.
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

/// `owl:sameAs` between two different constant IRIs is the one reserved read that may be
/// `complete`, on the DL path only: the bounds decide equality between the data's
/// individuals as they decide their other facts. :x has one :p, so :a = :b is entailed;
/// nothing makes :a = :x. Off the RL route (an existential outside OWL 2 RL) and on it.
/// The same read over the rules' closure stays denied.
#[test]
fn owl2_dl_decides_same_as_between_two_different_constants() {
    let data = ":p a owl:ObjectProperty , owl:FunctionalProperty . :x :p :a , :b . \
                :a a :Dog . :x a :Dog .";
    let existential = ":Dog rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :q ; \
                       owl:someValuesFrom :Dog ] .";
    let mut wrong = Vec::new();
    for off_the_rl_route in [false, true] {
        let dl = pipeline();
        let extra = if off_the_rl_route { existential } else { "" };
        insert(&dl, &format!("{data} {extra}")).expect("data");
        for (read, holds) in [
            ("ASK { :a owl:sameAs :b }", "true"),
            ("ASK { :a owl:sameAs :x }", "false"),
        ] {
            let (text, status) = answer(dl.store(), read);
            if !text.contains(holds) || status != ["complete"; 2] {
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

/// What the `owl:sameAs` entry leaves denied on the DL path: one term at both ends (eq-ref,
/// in neither bound: `:x owl:sameAs :x` and `:fresh owl:sameAs :fresh` are entailed, and
/// the store answers `false`), and a variable end (`:a`'s own pair is missing).
#[test]
fn owl2_dl_denies_same_as_of_one_term_or_a_variable() {
    let dl = pipeline();
    insert(&dl, ":p a owl:FunctionalProperty . :x :p :a , :b .").expect("data");
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

/// A constant outside the data isn't one of the individuals U1 bounds, and where the
/// ontology allows one individual only (`owl:Thing ⊑ {:a}`) it is entailed equal to every
/// other, which neither bound says. So the entry holds only for terms of the data: there
/// every term is entailed equal to :a, and the store answers `false` for those outside the
/// data. In data with no such axiom the same reads are `false` and right, but not
/// `complete` either.
#[test]
fn owl2_dl_same_as_of_a_term_outside_the_data_is_never_complete() {
    let mut wrong = Vec::new();
    for (data, entailed) in [
        (":x a :Dog .", false),
        (
            "owl:Thing rdfs:subClassOf [ owl:oneOf ( :a ) ] . :a a :Dog . :b a :Dog .",
            true,
        ),
    ] {
        let dl = pipeline();
        insert(&dl, data).expect("data");
        for read in [
            "ASK { :fresh owl:sameAs :new }",
            "ASK { :x owl:sameAs :fresh }",
        ] {
            let (text, status) = answer(dl.store(), read);
            if !text.contains("false") || status.contains(&"complete") {
                wrong.push(format!("{data}: {read}: {text} {status:?}"));
            }
        }
        // Between two terms of the data the bounds decide, the one domain element included.
        if entailed {
            let (text, status) = answer(dl.store(), "ASK { :a owl:sameAs :b }");
            if !(text.contains("true") || !status.contains(&"complete")) {
                wrong.push(format!("{data}: :a = :b: {text} {status:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
