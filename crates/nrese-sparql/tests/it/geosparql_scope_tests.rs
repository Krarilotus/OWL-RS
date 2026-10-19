//! GeoSPARQL relations and indexed filters within the reader's scope (native/spatial.rs):
//! geometries, features, stated relations and filter candidates come from the graphs the
//! query reads, as the user's access restricts them. One table: each case answers, and
//! reads as many filter candidates, as the same query over an oracle store that holds only
//! what the case may see.

use std::sync::Arc;

use nrese_engine::{DurabilityConfig, Engine, EngineConfig, Snapshot, SyncPolicy};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const GEO: &str = "http://www.opengis.net/ont/geosparql#";
const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX ex: <http://example.com/> ";

/// A statement: subject, GeoSPARQL predicate, object (an `ex:` name, or WKT when it starts
/// upper-case), graph (`None`: the default graph; else a name under `ex:g/`).
type Statement = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// Public geometries in the default graph and two public graphs; a secret graph adds shapes
/// inside the public ones, a second shape of a public object, the geometry of a public
/// feature's default geometry, the default geometry of a feature that otherwise relates by
/// its public `hasGeometry`, and a stated relation.
const DATA: &[Statement] = &[
    (
        "pubA",
        "asWKT",
        "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))",
        None,
    ),
    ("pubPt", "asWKT", "POINT(5 5)", None),
    ("pubA", "ehCovers", "stated", None),
    ("pubB", "asWKT", "POINT(3 3)", Some("pub/1")),
    ("fPub", "hasDefaultGeometry", "pubB", Some("pub/1")),
    ("fPub2", "hasGeometry", "pubB", Some("pub/1")),
    (
        "pubC",
        "asWKT",
        "POLYGON((2 2, 8 2, 8 8, 2 8, 2 2))",
        Some("pub/2"),
    ),
    ("pubD", "asWKT", "POINT(7 7)", Some("pub/2")),
    ("split", "hasDefaultGeometry", "splitGeom", Some("pub/2")),
    ("splitGeom", "asWKT", "POINT(4 4)", Some("secret")),
    ("fPub2", "hasDefaultGeometry", "far", Some("secret")),
    ("far", "asWKT", "POINT(80 80)", Some("secret")),
    (
        "pubA",
        "asWKT",
        "POLYGON((100 100, 110 100, 110 110, 100 110, 100 100))",
        Some("secret"),
    ),
    (
        "secret",
        "asWKT",
        "POLYGON((1 1, 9 1, 9 9, 1 9, 1 1))",
        Some("secret"),
    ),
    ("secret", "sfContains", "pubB", Some("secret")),
    ("sec0", "asWKT", "POINT(3 4)", Some("secret")),
    ("sec1", "asWKT", "POINT(5 4)", Some("secret")),
    ("sec2", "asWKT", "POINT(4 6)", Some("secret")),
];

/// Committed after the replica's image: it reaches the replica through the log.
const LATER: &[Statement] = &[
    ("moved", "asWKT", "POINT(6 6)", Some("pub/1")),
    ("sec3", "asWKT", "POINT(6 5)", Some("secret")),
];

/// One query per path: a constant side, both sides free, a stated relation, a feature's
/// default geometry, a count, disjointness (every object), an index-seeded filter, and
/// relations under `GRAPH ?g` and `GRAPH <g>`.
const QUERIES: &[&str] = &[
    "SELECT ?x WHERE { ex:pubA geo:sfContains ?x }",
    "SELECT ?a ?b WHERE { ?a geo:sfIntersects ?b }",
    "SELECT ?b WHERE { ex:secret geo:sfContains ?b }",
    "SELECT ?x WHERE { ex:fPub2 geo:sfWithin ?x }",
    "SELECT (COUNT(*) AS ?n) WHERE { ?a geo:sfWithin ?b }",
    "SELECT ?x WHERE { ?x geo:sfDisjoint ex:pubPt }",
    "SELECT ?g WHERE { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \
     \"POLYGON((2.5 2.5, 6.5 2.5, 6.5 6.5, 2.5 6.5, 2.5 2.5))\"^^geo:wktLiteral)) }",
    "SELECT ?g ?x WHERE { GRAPH ?g { ?x geo:sfWithin ex:pubC } }",
    "SELECT ?x WHERE { GRAPH <http://example.com/g/pub/2> { ?x geo:sfWithin ex:pubC } }",
];

/// The store an oracle holds.
enum Oracle {
    /// The graphs the public reader may read, as they are.
    Readable,
    /// A query dataset: these graphs merged into the default graph, these named.
    Dataset(&'static [Option<&'static str>], &'static [&'static str]),
}

struct Case {
    name: &'static str,
    /// Only the public graphs and the default graph readable.
    restricted: bool,
    union_default_graph: bool,
    /// `FROM` and `FROM NAMED` clauses.
    dataset: &'static str,
    oracle: Oracle,
    on_replica: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "unreadable graph, default graph",
        restricted: true,
        union_default_graph: false,
        dataset: "",
        oracle: Oracle::Readable,
        on_replica: false,
    },
    Case {
        name: "unreadable graph, merged default graph",
        restricted: true,
        union_default_graph: true,
        dataset: "",
        oracle: Oracle::Readable,
        on_replica: false,
    },
    Case {
        name: "unreadable graph, FROM",
        restricted: true,
        union_default_graph: false,
        dataset: " FROM <http://example.com/g/pub/2> FROM <http://example.com/g/secret>",
        oracle: Oracle::Readable,
        on_replica: false,
    },
    Case {
        name: "unreadable graph, FROM NAMED",
        restricted: true,
        union_default_graph: false,
        dataset: " FROM NAMED <http://example.com/g/pub/2> FROM NAMED <http://example.com/g/secret>",
        oracle: Oracle::Readable,
        on_replica: false,
    },
    Case {
        name: "dataset, FROM",
        restricted: false,
        union_default_graph: false,
        dataset: " FROM <http://example.com/g/pub/1> FROM <http://example.com/g/pub/2>",
        oracle: Oracle::Dataset(&[Some("pub/1"), Some("pub/2")], &[]),
        on_replica: false,
    },
    Case {
        name: "dataset, FROM NAMED",
        restricted: false,
        union_default_graph: false,
        dataset: " FROM <http://example.com/g/pub/1> FROM NAMED <http://example.com/g/pub/2>",
        oracle: Oracle::Dataset(&[Some("pub/1")], &["pub/2"]),
        on_replica: false,
    },
    Case {
        name: "replica, unreadable graph",
        restricted: true,
        union_default_graph: false,
        dataset: "",
        oracle: Oracle::Readable,
        on_replica: true,
    },
];

fn graph_name(graph: Option<&str>) -> GraphName {
    match graph {
        None => GraphName::DefaultGraph,
        Some(g) => NamedNode::new_unchecked(format!("{EX}g/{g}")).into(),
    }
}

fn quad(&(s, p, o, _): &Statement, graph: GraphName) -> Quad {
    let object: Term = if o.starts_with(|c: char| c.is_ascii_uppercase()) {
        Literal::new_typed_literal(o, NamedNode::new_unchecked(format!("{GEO}wktLiteral"))).into()
    } else {
        NamedNode::new_unchecked(format!("{EX}{o}")).into()
    };
    Quad::new(
        NamedNode::new_unchecked(format!("{EX}{s}")),
        NamedNode::new_unchecked(format!("{GEO}{p}")),
        object,
        graph,
    )
}

fn commit<'a>(engine: &Engine, statements: impl IntoIterator<Item = (&'a Statement, GraphName)>) {
    let mut tx = engine.transaction();
    for (statement, graph) in statements {
        tx.insert(quad(statement, graph).as_ref());
    }
    tx.commit().unwrap();
}

fn all() -> impl Iterator<Item = &'static Statement> {
    DATA.iter().chain(LATER)
}

fn public() -> GraphAccess {
    GraphAccess {
        prefixes: vec![format!("{EX}g/pub/")],
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }
}

fn oracle(oracle: &Oracle) -> Snapshot {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    match oracle {
        Oracle::Readable => commit(
            &engine,
            all()
                .filter(|st| public().allows_graph(&graph_name(st.3)))
                .map(|st| (st, graph_name(st.3))),
        ),
        Oracle::Dataset(default, named) => commit(
            &engine,
            all()
                .filter(|st| default.contains(&st.3))
                .map(|st| (st, GraphName::DefaultGraph))
                .chain(
                    all()
                        .filter(|st| st.3.is_some_and(|g| named.contains(&g)))
                        .map(|st| (st, graph_name(st.3))),
                ),
        ),
    }
    engine.snapshot()
}

/// The answer as sorted lines, and the most rows any step read (a seeded filter's
/// candidates).
fn run(snapshot: &Snapshot, query: &str, options: &QueryOptions) -> (Vec<String>, u64) {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let QueryResults::Solutions(solutions) = evaluate_query(snapshot, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    let mut rows: Vec<String> = solutions
        .map(|s| {
            let s = s.unwrap();
            variables
                .iter()
                .map(|v| s.get(v).map_or("-".to_owned(), ToString::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    let plan = explain_query(snapshot, &query, options).unwrap();
    (rows, plan.steps.iter().map(|s| s.rows).max().unwrap_or(0))
}

fn with_dataset(query: &str, dataset: &str) -> String {
    let at = query.find(" WHERE").expect("a WHERE clause");
    format!("{}{dataset}{}", &query[..at], &query[at..])
}

/// A reader without access to a graph, and a query's dataset, see only their graphs, on a
/// primary and on a replica fed by the log; every case fails where the spatial index reads
/// every graph.
#[test]
fn spatial_answers_stay_in_the_readers_scope() {
    let durable = EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            sync: SyncPolicy::EveryCommit,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    };
    let (primary_dir, replica_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let primary = Engine::open(primary_dir.path(), durable).unwrap();
    commit(&primary, DATA.iter().map(|st| (st, graph_name(st.3))));
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), durable).unwrap();
    commit(&primary, LATER.iter().map(|st| (st, graph_name(st.3))));
    let batch = primary
        .log_since(replica.snapshot().revision(), 1 << 20)
        .unwrap();
    replica.apply_log(&batch.frames).unwrap();
    assert_eq!(replica.snapshot().revision(), primary.snapshot().revision());

    let mut failed = Vec::new();
    for case in CASES {
        let options = QueryOptions {
            access: case.restricted.then(|| Arc::new(public())),
            union_default_graph: case.union_default_graph,
            ..QueryOptions::default()
        };
        let plain = QueryOptions {
            union_default_graph: case.union_default_graph,
            ..QueryOptions::default()
        };
        let store = match case.on_replica {
            true => replica.snapshot(),
            false => primary.snapshot(),
        };
        let expected_store = oracle(&case.oracle);
        for query in QUERIES {
            let got = run(&store, &with_dataset(query, case.dataset), &options);
            let expected = match case.oracle {
                Oracle::Readable => {
                    run(&expected_store, &with_dataset(query, case.dataset), &plain)
                }
                Oracle::Dataset(..) => run(&expected_store, query, &plain),
            };
            if got != expected {
                failed.push(format!(
                    "{}: {query}\n  got      {got:?}\n  expected {expected:?}",
                    case.name
                ));
            }
        }
    }
    let cases: std::collections::BTreeSet<&str> = failed
        .iter()
        .map(|f| f.split(':').next().unwrap())
        .collect();
    assert!(
        failed.is_empty(),
        "cases failing: {cases:?}\n{}",
        failed.join("\n")
    );
}
