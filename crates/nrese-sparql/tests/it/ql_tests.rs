//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): what the
//! tree-witness rewriting adds, its semantics for bags, and the guards of
//! docs/design/performance.md §0 (no change without tree witnesses; bounded rewritings).
//!
//! The data here is written closed, as the materialisation would leave it.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::ql::{Closure, Limits, QlRewriting};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, plan_query};
use nrese_sparql_syntax::{Query, SparqlParser};

const PREFIXES: &str = "@prefix : <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";

/// Employees work for an organisation; managers are employees; `worksFor` has the domain
/// `Person`. `ann` has a stated employer, `bob` and `cat` (a manager) don't.
const STAFF: &str = ":Employee rdfs:subClassOf [ a owl:Restriction ;
        owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
    :Manager rdfs:subClassOf :Employee .
    :worksFor rdfs:domain :Person .
    :ann a :Employee , :Person ; :worksFor :acme .
    :acme a :Organisation .
    :bob a :Employee .
    :cat a :Manager , :Employee .
    :dan a :Person ; :knows :ann .";

fn engine(turtle: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let text = format!("{PREFIXES}{turtle}");
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn parse(query: &str) -> Query {
    let text = format!("PREFIX : <http://example.org/> {query}");
    SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn on() -> QueryOptions {
    QueryOptions {
        ql: Some(Arc::new(QlRewriting::new(Closure { lists: true }))),
        ..QueryOptions::default()
    }
}

/// The solutions, as sorted lines of `?var=value` (with repeats: bags).
fn rows(engine: &Engine, query: &str, options: &QueryOptions) -> Vec<String> {
    let snapshot = engine.snapshot();
    match evaluate_query(&snapshot, &parse(query), options).unwrap() {
        QueryResults::Solutions(solutions) => {
            let mut out: Vec<String> = solutions
                .map(|s| {
                    let s = s.unwrap();
                    let mut row: Vec<String> = s
                        .iter()
                        .map(|(v, t)| {
                            format!(
                                "?{}={}",
                                v.as_str(),
                                t.to_string().replace("http://example.org/", "")
                            )
                        })
                        .collect();
                    row.sort();
                    row.join(" ")
                })
                .collect();
            out.sort();
            out
        }
        QueryResults::Boolean(b) => vec![b.to_string()],
        QueryResults::Graph(triples) => {
            let mut out: Vec<String> = triples
                .map(|t| t.unwrap().to_string().replace("http://example.org/", ""))
                .collect();
            out.sort();
            out
        }
    }
}

#[test]
fn an_unprojected_employer_is_found_through_the_existential() {
    let e = engine(STAFF);
    let query = "SELECT ?x WHERE { ?x :worksFor ?y }";
    assert_eq!(rows(&e, query, &QueryOptions::default()), ["?x=<ann>"]);
    assert_eq!(rows(&e, query, &on()), ["?x=<ann>", "?x=<bob>", "?x=<cat>"]);
    // A blank node is existential too; a projected employer isn't.
    assert_eq!(
        rows(&e, "SELECT ?x WHERE { ?x :worksFor [] }", &on()),
        ["?x=<ann>", "?x=<bob>", "?x=<cat>"]
    );
    assert_eq!(
        rows(&e, "SELECT ?x ?y WHERE { ?x :worksFor ?y }", &on()),
        ["?x=<ann> ?y=<acme>"]
    );
    // The filler's class holds of the anonymous employer.
    assert_eq!(
        rows(
            &e,
            "SELECT ?x WHERE { ?x :worksFor ?y . ?y a :Organisation }",
            &on()
        ),
        ["?x=<ann>", "?x=<bob>", "?x=<cat>"]
    );
    // Used elsewhere in the query, it isn't existential.
    assert_eq!(
        rows(
            &e,
            "SELECT ?x WHERE { ?x :worksFor ?y . FILTER(?y != :none) }",
            &on()
        ),
        ["?x=<ann>"]
    );
}

#[test]
fn memberships_only_an_existential_gives_are_answers() {
    let e = engine(STAFF);
    // Every employee works for someone, so is in `worksFor`'s domain.
    assert_eq!(
        rows(&e, "SELECT ?x WHERE { ?x a :Person }", &on()),
        ["?x=<ann>", "?x=<bob>", "?x=<cat>", "?x=<dan>"]
    );
    assert_eq!(rows(&e, "ASK { :bob a :Person }", &on()), ["true"]);
    assert_eq!(
        rows(&e, "ASK { :bob a :Person }", &QueryOptions::default()),
        ["false"]
    );
}

#[test]
fn bags_count_each_answer_through_the_rewriting_once() {
    let e = engine(&format!("{STAFF} :ann :worksFor :initech ."));
    // `ann` has two stated employers: two rows, as without the rewriting; `bob` and `cat`
    // one each.
    assert_eq!(
        rows(&e, "SELECT ?x WHERE { ?x :worksFor ?y }", &on()),
        ["?x=<ann>", "?x=<ann>", "?x=<bob>", "?x=<cat>"]
    );
    assert_eq!(
        rows(&e, "SELECT DISTINCT ?x WHERE { ?x :worksFor ?y }", &on()),
        ["?x=<ann>", "?x=<bob>", "?x=<cat>"]
    );
    let count = |query: &str| rows(&e, query, &on());
    let n = |n: u32| {
        [format!(
            "?n=\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        )]
    };
    assert_eq!(
        count("SELECT (COUNT(?x) AS ?n) WHERE { ?x :worksFor ?y }"),
        n(4)
    );
    assert_eq!(
        count("SELECT (COUNT(*) AS ?n) WHERE { ?x :worksFor [] }"),
        n(4)
    );
    // `COUNT(*)` reads `?y` too: the pairs.
    assert_eq!(
        count("SELECT (COUNT(*) AS ?n) WHERE { ?x :worksFor ?y }"),
        n(2)
    );
    // Nothing but blank nodes: the materialised rows; the rewriting adds a row only where
    // the data has none.
    assert_eq!(
        rows(&e, "SELECT * WHERE { [] :worksFor [] }", &on()),
        [""; 2]
    );
}

#[test]
fn optional_and_construct_see_the_rewriting() {
    let e = engine(STAFF);
    assert_eq!(
        rows(
            &e,
            "SELECT ?x ?w WHERE { ?x a :Person OPTIONAL { ?x :worksFor [] BIND(true AS ?w) } }",
            &on()
        ),
        [
            "?w=\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean> ?x=<ann>",
            "?w=\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean> ?x=<bob>",
            "?w=\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean> ?x=<cat>",
            "?x=<dan>",
        ]
    );
    assert_eq!(
        rows(
            &e,
            "CONSTRUCT { ?x a :Employed } WHERE { ?x :worksFor ?y }",
            &on()
        ),
        [
            "<ann> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <Employed>",
            "<bob> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <Employed>",
            "<cat> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <Employed>",
        ]
    );
}

#[test]
fn explain_shows_the_rewriting() {
    let e = engine(STAFF);
    let snapshot = e.snapshot();
    let plan = plan_query(
        &snapshot,
        &parse("SELECT ?x WHERE { ?x :worksFor ?y }"),
        &on(),
    )
    .unwrap();
    assert!(
        plan.rewrites.contains(&"ql-tree-witness"),
        "{:?}",
        plan.rewrites
    );
    assert!(
        plan.steps.iter().any(|s| s.operator == "union"),
        "{:?}",
        plan.steps
    );
    // Reading the asserted statements only, or a dataset of its own: not rewritten.
    let asserted = QueryOptions {
        read_model: nrese_engine::ReadModel::Asserted,
        ..on()
    };
    let plan = plan_query(
        &snapshot,
        &parse("SELECT ?x WHERE { ?x :worksFor ?y }"),
        &asserted,
    )
    .unwrap();
    assert!(!plan.rewrites.contains(&"ql-tree-witness"));
}

/// Guard (performance.md §0): a query without tree witnesses or affected class atoms plans
/// exactly as with the rewriting off; and a schema without generating axioms changes no
/// query.
#[test]
fn queries_without_tree_witnesses_plan_as_before() {
    let e = engine(STAFF);
    let snapshot = e.snapshot();
    let queries = [
        "SELECT ?x ?y WHERE { ?x :worksFor ?y }",
        "SELECT * WHERE { ?x :knows ?y . ?y :worksFor ?z }",
        "SELECT ?x WHERE { ?x a :Employee . ?x :worksFor :acme }",
        "SELECT ?x (COUNT(?y) AS ?n) WHERE { ?x :knows ?y } GROUP BY ?x",
        "SELECT ?x WHERE { ?x :knows ?y . ?y a :Manager } ORDER BY ?x LIMIT 3",
        "ASK { :ann :worksFor :acme }",
    ];
    for query in queries {
        let query = parse(query);
        let off = plan_query(&snapshot, &query, &QueryOptions::default()).unwrap();
        let with = plan_query(&snapshot, &query, &on()).unwrap();
        assert_eq!(
            (&off.rewrites, &off.steps),
            (&with.rewrites, &with.steps),
            "{query}"
        );
        // Nothing rewritten, and nothing missing (complete under the closure's regime).
        let mut nothing = nrese_sparql::ql::QlReport::default();
        nothing.completeness.regime = with.ql.as_ref().and_then(|r| r.completeness.regime);
        assert_eq!(with.ql.unwrap_or_default(), nothing);
    }
    let plain = engine(
        ":Manager rdfs:subClassOf :Employee . :worksFor rdfs:domain :Person .
         :ann :worksFor :acme .",
    );
    let snapshot = plain.snapshot();
    for query in [
        "SELECT ?x WHERE { ?x :worksFor ?y }",
        "SELECT ?x WHERE { ?x a :Person }",
    ] {
        let query = parse(query);
        let off = plan_query(&snapshot, &query, &QueryOptions::default()).unwrap();
        let with = plan_query(&snapshot, &query, &on()).unwrap();
        assert_eq!(
            (&off.rewrites, &off.steps),
            (&with.rewrites, &with.steps),
            "{query}"
        );
        // Nothing rewritten, and nothing missing (complete under the closure's regime).
        let mut nothing = nrese_sparql::ql::QlReport::default();
        nothing.completeness.regime = with.ql.as_ref().and_then(|r| r.completeness.regime);
        assert_eq!(with.ql.unwrap_or_default(), nothing);
    }
}

/// Guard (performance.md §0): rewritings are bounded (design §5); past a bound the
/// pattern runs as written and EXPLAIN says so.
#[test]
fn rewritings_stay_within_their_bounds() {
    // Each node has a successor that has one: chains of any length below every node.
    let e = engine(
        ":Node rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :next ;
            owl:someValuesFrom :Node ] .
         :a a :Node .",
    );
    let snapshot = e.snapshot();
    let chain = |n: usize| {
        let steps: Vec<String> = (0..n)
            .map(|i| format!("?v{i} :next ?v{} .", i + 1))
            .collect();
        format!("SELECT ?v0 WHERE {{ {} }}", steps.join(" "))
    };
    // Eight steps: a witness per suffix of the chain, nine branches.
    let plan = plan_query(&snapshot, &parse(&chain(8)), &on()).unwrap();
    assert!(plan.rewrites.contains(&"ql-tree-witness"));
    let unions = plan.steps.iter().filter(|s| s.operator == "union").count();
    assert!(unions <= 2 * 9, "{unions} unions: {:?}", plan.steps);
    assert_eq!(rows(&e, &chain(8), &on()), ["?v0=<a>"]);
    // Past the bound on existential variables: as written.
    let plan = plan_query(&snapshot, &parse(&chain(20)), &on()).unwrap();
    assert!(plan.rewrites.contains(&"ql-limit"));
    assert!(!plan.rewrites.contains(&"ql-tree-witness"));
    let off = plan_query(&snapshot, &parse(&chain(20)), &QueryOptions::default()).unwrap();
    assert_eq!(off.steps, plan.steps);
    // A tight bound on branches.
    let tight = QueryOptions {
        ql: Some(Arc::new(
            QlRewriting::new(Closure { lists: true }).with_limits(Limits {
                branches: 4,
                ..Limits::default()
            }),
        )),
        ..QueryOptions::default()
    };
    let plan = plan_query(&snapshot, &parse(&chain(8)), &tight).unwrap();
    assert!(plan.rewrites.contains(&"ql-limit"));
}

/// Incompleteness is never silent (design §7): what the rewriting doesn't follow is
/// reported with the plan, and the answers stay as they are.
#[test]
fn incompleteness_is_reported_never_silent() {
    let status = |e: &Engine, query: &str| {
        let plan = plan_query(&e.snapshot(), &parse(query), &on()).unwrap();
        let ql = plan.ql.expect("the rewriting applies");
        (
            ql.completeness.as_str(),
            ql.completeness
                .reasons
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | "),
        )
    };
    let pure = engine(STAFF);
    assert_eq!(
        status(&pure, "SELECT ?x WHERE { ?x :worksFor ?y }"),
        ("complete", String::new())
    );
    // A transitive worksFor meets the existential: its answers may be missing.
    let mixed = engine(&format!("{STAFF} :worksFor a owl:TransitiveProperty ."));
    let (state, reasons) = status(&mixed, "SELECT ?x WHERE { ?x :worksFor ?y }");
    assert_eq!(state, "sound-only");
    assert!(reasons.contains("worksFor> is transitive"), "{reasons}");
    // A term no hazard reaches stays complete.
    assert_eq!(
        status(&mixed, "SELECT ?x WHERE { ?x :knows ?y }").0,
        "complete"
    );
    // Patterns the rewriting doesn't enter, and variable predicates.
    let (state, reasons) = status(&pure, "SELECT ?x WHERE { ?x a :Person ; :worksFor+ ?y }");
    assert_eq!(state, "sound-only");
    assert!(reasons.contains("inside a property path"), "{reasons}");
    assert_eq!(
        status(&pure, "SELECT ?x ?p WHERE { ?x ?p [] }").0,
        "sound-only"
    );
    // The answers don't change for it.
    assert_eq!(
        rows(&mixed, "SELECT ?x WHERE { ?x :worksFor ?y }", &on()),
        ["?x=<ann>", "?x=<bob>", "?x=<cat>"]
    );
}

/// The schema read at the snapshot includes what the rewriting doesn't follow: a chain
/// through the role of an existential is reported (found by the mixed differential test).
#[test]
fn chains_in_the_store_are_hazards() {
    let e = engine(&format!(
        "{STAFF} :leads owl:propertyChainAxiom ( :worksFor :partOf ) ."
    ));
    let plan = plan_query(
        &e.snapshot(),
        &parse("SELECT ?x WHERE { ?x :worksFor ?y }"),
        &on(),
    )
    .unwrap();
    let ql = plan.ql.expect("the rewriting applies");
    assert_eq!(ql.completeness.as_str(), "sound-only");
    assert!(
        ql.completeness
            .reasons
            .iter()
            .any(|r| r.text.contains("property chain")),
        "{:?}",
        ql.completeness
    );
}

/// The research review's case, with its names: existentials feeding a transitive role.
/// `e1` is a certain answer through two anonymous individuals and the transitive shortcut;
/// the closure (here: `e1 a Engine`, nothing more) and the rewriting miss it, and the
/// answer says so, naming `partOf`.
#[test]
fn existentials_feeding_a_transitive_role_are_flagged() {
    let e = engine(
        ":Engine rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ;
            owl:someValuesFrom :Car ] .
         :Car rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ;
            owl:someValuesFrom :Fleet ] .
         :partOf a owl:TransitiveProperty .
         :e1 a :Engine .",
    );
    let query = "SELECT ?x { ?x :partOf ?f . ?f a :Fleet }";
    assert_eq!(rows(&e, query, &on()), Vec::<String>::new());
    let plan = plan_query(&e.snapshot(), &parse(query), &on()).unwrap();
    let completeness = plan.ql.expect("the rewriting applies").completeness;
    assert_eq!(completeness.as_str(), "sound-only");
    assert!(
        completeness
            .reasons
            .iter()
            .any(|r| r.source == "ql" && r.text.contains("partOf> is transitive")),
        "{completeness:?}"
    );
}

/// `EXISTS`, `NOT EXISTS`, `MINUS`'s right side and `GRAPH` naming the default graph read
/// the entailments as the query's other patterns do (design §2): a pattern there is a set,
/// its variables bound outside answer variables. Named graphs hold their asserted
/// statements only.
#[test]
fn negation_and_the_default_graph_alias_see_the_rewriting() {
    let e = engine(STAFF);
    let ask = |query: &str| {
        let plan = plan_query(&e.snapshot(), &parse(query), &on()).unwrap();
        let status = plan.ql.map(|ql| ql.completeness.as_str()).unwrap_or("none");
        (rows(&e, query, &on()), status)
    };
    let strings = |rows: &[&str]| rows.iter().map(|r| r.to_string()).collect::<Vec<_>>();
    let employed = strings(&["?x=<ann>", "?x=<bob>", "?x=<cat>"]);
    let (got, status) = ask("SELECT ?x WHERE { ?x a :Person FILTER EXISTS { ?x :worksFor [] } }");
    assert_eq!((got, status), (employed.clone(), "complete"));
    // Bob and Cat work for some organisation: no person but Dan is without an employer.
    for query in [
        "SELECT ?x WHERE { ?x a :Person FILTER NOT EXISTS { ?x :worksFor [] } }",
        "SELECT ?x WHERE { ?x a :Person FILTER (!EXISTS { ?x :worksFor ?y }) }",
        "SELECT ?x WHERE { ?x a :Person MINUS { ?x :worksFor ?y } }",
    ] {
        assert_eq!(ask(query), (strings(&["?x=<dan>"]), "complete"), "{query}");
    }
    assert_eq!(
        ask("SELECT ?x WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?x :worksFor [] } }"),
        (employed.clone(), "complete")
    );
    let (got, _) =
        ask("SELECT ?x ?b WHERE { ?x a :Employee BIND(EXISTS { ?x :worksFor [] } AS ?b) }");
    assert!(got.iter().all(|row| row.contains("\"true\"")), "{got:?}");

    // What a negated pattern may miss can make an answer wrong: unsound, with the reason.
    let mixed = engine(&format!("{STAFF} :worksFor a owl:TransitiveProperty ."));
    for query in [
        "SELECT ?x WHERE { ?x a :Person FILTER NOT EXISTS { ?x :worksFor [] } }",
        "SELECT ?x WHERE { ?x a :Person MINUS { ?x :worksFor ?y } }",
        "SELECT ?x ?b WHERE { ?x a :Person BIND(EXISTS { ?x :worksFor [] } AS ?b) }",
    ] {
        let plan = plan_query(&mixed.snapshot(), &parse(query), &on()).unwrap();
        let ql = plan.ql.expect("the rewriting applies");
        assert_eq!(ql.completeness.as_str(), "unsound", "{query}");
        assert!(
            ql.completeness
                .reasons
                .iter()
                .any(|r| r.text.contains("may be wrong as well as missing")),
            "{query}: {:?}",
            ql.completeness.reasons
        );
    }
    // Under no negation, sound-only as anywhere else.
    let plan = plan_query(
        &mixed.snapshot(),
        &parse("SELECT ?x WHERE { ?x a :Person FILTER EXISTS { ?x :worksFor [] } }"),
        &on(),
    )
    .unwrap();
    assert_eq!(plan.ql.unwrap().completeness.as_str(), "sound-only");

    // A variable an outer solution may leave unbound is free inside the EXISTS: said.
    let plan = plan_query(
        &e.snapshot(),
        &parse(
            "SELECT ?p WHERE { ?p a :Person OPTIONAL { ?p :knows ?x } FILTER EXISTS { ?x :worksFor [] } }",
        ),
        &on(),
    )
    .unwrap();
    let ql = plan.ql.unwrap();
    assert_eq!(ql.completeness.as_str(), "sound-only");
    assert!(
        ql.completeness
            .reasons
            .iter()
            .any(|r| r.text.contains("may be unbound")),
        "{:?}",
        ql.completeness.reasons
    );

    // A named graph holds what was asserted in it, with nothing to say about it.
    let quads = engine_trig(&format!(
        "{PREFIXES} :Employee rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
         :g {{ :eve a :Employee . :fay :worksFor :acme }}"
    ));
    let query = "SELECT ?x WHERE { GRAPH ?g { ?x :worksFor [] } }";
    assert_eq!(rows(&quads, query, &on()), ["?x=<fay>"]);
    let plan = plan_query(&quads.snapshot(), &parse(query), &on()).unwrap();
    assert_eq!(plan.ql.map(|ql| ql.completeness.as_str()), Some("complete"));
}

fn engine_trig(trig: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::TriG).for_reader(trig.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The guard of performance.md's "witnesses the data has" (design §3): a witness whose
/// tree the data has at every individual it folds to adds no answer, so the query runs as
/// written; one individual without it, and the rewriting is back.
#[test]
fn witnesses_the_data_has_are_not_folded() {
    let realised = ":Employee rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
        :ann a :Employee ; :worksFor :acme . :acme a :Organisation .";
    let query = "SELECT ?x WHERE { ?x :worksFor ?y }";
    let e = engine(realised);
    let plan = plan_query(&e.snapshot(), &parse(query), &on()).unwrap();
    let ql = plan.ql.unwrap();
    assert_eq!((ql.patterns, ql.realised, ql.checks), (0, 1, 1));
    assert!(
        !plan.rewrites.contains(&"ql-tree-witness"),
        "{:?}",
        plan.rewrites
    );
    assert_eq!(rows(&e, query, &on()), ["?x=<ann>"]);
    // Asked once per snapshot revision: the next query's answer is the cache's.
    let options = on();
    let snapshot = e.snapshot();
    plan_query(&snapshot, &parse(query), &options).unwrap();
    let again = plan_query(&snapshot, &parse(query), &options)
        .unwrap()
        .ql
        .unwrap();
    assert_eq!((again.realised, again.checks), (1, 0));

    let e = engine(&format!("{realised} :bob a :Employee ."));
    let plan = plan_query(&e.snapshot(), &parse(query), &on()).unwrap();
    let ql = plan.ql.unwrap();
    assert_eq!((ql.patterns, ql.realised), (1, 0));
    assert_eq!(rows(&e, query, &on()), ["?x=<ann>", "?x=<bob>"]);
    // A group no aggregate of which depends on how often a row comes reads a set.
    assert_eq!(
        rows(
            &e,
            "SELECT (COUNT(DISTINCT ?x) AS ?n) (SAMPLE(?x) AS ?s) WHERE { ?x :worksFor [] } HAVING (COUNT(DISTINCT ?x) > 0)",
            &on()
        )
        .len(),
        1
    );
    assert_eq!(
        rows(
            &e,
            "SELECT (COUNT(DISTINCT ?x) AS ?n) WHERE { ?x :worksFor [] }",
            &on()
        ),
        ["?n=\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"]
    );
}
