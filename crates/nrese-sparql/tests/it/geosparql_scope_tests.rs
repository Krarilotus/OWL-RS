//! GeoSPARQL relations and indexed filters within the reader's scope (native/spatial.rs):
//! geometries, features, stated relations and filter candidates come from the graphs the
//! query reads, as the user's access restricts them. One table: each case answers, and
//! reads as many filter candidates, as the same query over an oracle store that holds only
//! what the case may see.

use std::sync::Arc;

use nrese_engine::{
    DurabilityConfig, EncodedTriple, Engine, EngineConfig, Snapshot, SyncPolicy, TermId,
};
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
    // Linked through `pubBalias`, an `owl:sameAs` alias of pubB ([`CLASSES`]).
    ("fAlias", "hasGeometry", "pubBalias", Some("pub/2")),
];

/// `owl:sameAs` classes, inferred, alias and representative (the smaller id): a geometry's
/// alias, which a named graph's statement names, and a graph's name (its representative's
/// name interned before the data, so that canonical reads rename that graph).
const CLASSES: &[(&str, &str)] = &[("pubBalias", "pubB"), ("g/pub/2", "g/pub/2rep")];

/// `statement` as a default graph holds it, over representatives (a store's reasoner
/// keeps its default graph so; named graphs keep what they were given).
fn over_representatives(&(s, p, o, g): &Statement) -> Statement {
    let rep = |name: &'static str| {
        CLASSES
            .iter()
            .find(|(alias, _)| *alias == name)
            .map_or(name, |(_, rep)| rep)
    };
    (rep(s), p, rep(o), g)
}

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
    // fPub2's default geometry (secret) hides its other one (pub/1) in every scope that
    // shows both, also where the scope shows every graph the store holds.
    Case {
        name: "every graph selected: a default geometry in another graph hides",
        restricted: false,
        union_default_graph: true,
        dataset: "",
        oracle: Oracle::Dataset(
            &[None, Some("pub/1"), Some("pub/2"), Some("secret")],
            &["pub/1", "pub/2", "secret"],
        ),
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

/// The graph names of [`CLASSES`]' representatives, interned before the data.
fn graph_representatives(engine: &Engine) {
    let tx = engine.transaction();
    for (_, rep) in CLASSES.iter().filter(|(alias, _)| alias.starts_with("g/")) {
        tx.intern(
            NamedNode::new_unchecked(format!("{EX}{rep}"))
                .as_ref()
                .into(),
        );
    }
    tx.commit().unwrap();
}

/// The [`CLASSES`] put into `engine` as inferred `owl:sameAs` statements (after the data);
/// `owl:sameAs`'s id.
fn classes(engine: &Engine) -> TermId {
    let mut tx = engine.transaction();
    let id = |tx: &nrese_engine::Transaction<'_>, iri: String| {
        tx.intern(NamedNode::new_unchecked(iri).as_ref().into())
    };
    let same_as = id(&tx, "http://www.w3.org/2002/07/owl#sameAs".to_owned());
    for (alias, of) in CLASSES {
        let alias = id(&tx, format!("{EX}{alias}"));
        let of = id(&tx, format!("{EX}{of}"));
        tx.insert_inferred(EncodedTriple::new(alias, same_as, of));
    }
    tx.commit().unwrap();
    same_as
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

/// The oracle store of `case`: what it may see, with a merged default graph merged here
/// (its statements copied into the store's default graph), so that the oracle never reads
/// a merge itself.
fn oracle(case: &Case, canonical: bool) -> (Snapshot, TermId) {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    graph_representatives(&engine);
    // Statements copied into the default graph, over representatives where the classes
    // hold (canonical answers; strict ones read an alias as a term of its own).
    let copied: Vec<Statement> = all()
        .map(|st| match canonical {
            true => over_representatives(st),
            false => *st,
        })
        .collect();
    let copy = |st: &Statement| &copied[all().position(|x| x == st).expect("a statement")];
    let readable = |st: &&Statement| public().allows_graph(&graph_name(st.3));
    match case.oracle {
        Oracle::Readable if case.union_default_graph => commit(
            &engine,
            all()
                .filter(readable)
                .map(|st| (copy(st), GraphName::DefaultGraph))
                .chain(
                    all()
                        .filter(readable)
                        .filter(|st| st.3.is_some())
                        .map(|st| (st, graph_name(st.3))),
                ),
        ),
        Oracle::Readable => commit(
            &engine,
            all().filter(readable).map(|st| (st, graph_name(st.3))),
        ),
        Oracle::Dataset(default, named) => commit(
            &engine,
            all()
                .filter(|st| default.contains(&st.3))
                .map(|st| (copy(st), GraphName::DefaultGraph))
                .chain(
                    all()
                        .filter(|st| st.3.is_some_and(|g| named.contains(&g)))
                        .map(|st| (st, graph_name(st.3))),
                ),
        ),
    }
    let same_as = classes(&engine);
    (engine.snapshot(), same_as)
}

/// Whether canonical answers apply to `case` on both sides: its default graph is the
/// store's or the merge of every graph (no `FROM`, no restricted merge), as the oracle's is.
fn canonical_applies(case: &Case) -> bool {
    !case.dataset.contains(" FROM <") && !(case.union_default_graph && case.restricted)
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
/// primary and on a replica fed by the log; every case failed with an earlier index (the
/// last with the first graph-scoped one).
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
    graph_representatives(&primary);
    commit(&primary, DATA.iter().map(|st| (st, graph_name(st.3))));
    let same_as = classes(&primary);
    primary.write_image(replica_dir.path()).unwrap();
    let replica = Engine::open(replica_dir.path(), durable).unwrap();
    commit(&primary, LATER.iter().map(|st| (st, graph_name(st.3))));
    let batch = primary
        .log_since(replica.snapshot().revision(), 1 << 20)
        .unwrap();
    replica.apply_log(&batch.frames).unwrap();
    assert_eq!(replica.snapshot().revision(), primary.snapshot().revision());

    let mut failed = Vec::new();
    // Strict answers, then canonical ones (owl:sameAs classes named by representatives,
    // named graphs and their names too) where they apply to both sides.
    for canonical in [false, true] {
        for case in CASES
            .iter()
            .filter(|case| !canonical || canonical_applies(case))
        {
            let modes = |options: QueryOptions| QueryOptions {
                equality_canonical: canonical,
                equality_early_expansion: canonical,
                ..options
            };
            let options = modes(QueryOptions {
                access: case.restricted.then(|| Arc::new(public())),
                union_default_graph: case.union_default_graph,
                ..QueryOptions::default()
            });
            // The oracle reads its default graph as stored: never a merge.
            let plain = modes(QueryOptions::default());
            let equality = |snapshot: Snapshot, same_as| match canonical {
                true => snapshot.with_equality(Some(same_as)),
                false => snapshot,
            };
            let store = equality(
                match case.on_replica {
                    true => replica.snapshot(),
                    false => primary.snapshot(),
                },
                same_as,
            );
            let (expected_store, oracle_same_as) = oracle(case, canonical);
            let expected_store = equality(expected_store, oracle_same_as);
            for query in QUERIES {
                let got = run(&store, &with_dataset(query, case.dataset), &options);
                let expected = match case.oracle {
                    Oracle::Readable => {
                        run(&expected_store, &with_dataset(query, case.dataset), &plain)
                    }
                    Oracle::Dataset(..) => run(&expected_store, query, &plain),
                };
                if got != expected {
                    let mode = if canonical { "canonical " } else { "" };
                    failed.push(format!(
                        "{mode}{}: {query}
  got      {got:?}
  expected {expected:?}",
                        case.name
                    ));
                }
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

/// With `owl:sameAs` classes, strict and canonical, spatial relations name terms and
/// graphs as the query's own scans do: each relation, as a pattern (a constant side, a
/// bound value, under `GRAPH <g>`, `GRAPH ?g` and `FROM NAMED`) and as an index-seeded
/// filter, answers as `geof:` over the plain scans of its geometries. The default graph
/// holds a representative; a named graph, itself named by an alias, names the alias
/// (canonical reads name both by their representatives).
#[test]
fn an_alias_relates_as_its_class() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let tx = engine.transaction();
    let id = |tx: &nrese_engine::Transaction<'_>, local: &str| {
        let iri = match local.starts_with("http") {
            true => local.to_owned(),
            false => format!("{EX}{local}"),
        };
        tx.intern(NamedNode::new_unchecked(iri).as_ref().into())
    };
    // The representatives first (the smaller ids): the default graph's data names them; a
    // named graph, itself named by an alias, names the alias.
    for rep in ["rep", "g/grep"] {
        id(&tx, rep);
    }
    tx.commit().unwrap();
    let statements: [Statement; 4] = [
        ("rep", "asWKT", "POINT(1 1)", None),
        ("box", "asWKT", "POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))", None),
        ("alias", "asWKT", "POINT(1 1)", Some("galias")),
        (
            "box2",
            "asWKT",
            "POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))",
            Some("galias"),
        ),
    ];
    commit(&engine, statements.iter().map(|st| (st, graph_name(st.3))));
    let mut tx = engine.transaction();
    let same_as = id(&tx, "http://www.w3.org/2002/07/owl#sameAs");
    for (alias, of) in [("alias", "rep"), ("g/galias", "g/grep")] {
        let (alias, of) = (id(&tx, alias), id(&tx, of));
        tx.insert_inferred(EncodedTriple::new(alias, same_as, of));
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot().with_equality(Some(same_as));
    let in_graph = "<http://example.com/g/galias>";
    let box_ = "\"POLYGON((0.5 0.5, 1.5 0.5, 1.5 1.5, 0.5 1.5, 0.5 0.5))\"^^geo:wktLiteral";
    // A relation as a pattern, the same over plain scans, and whether it must find some.
    let pairs = [
        (
            "SELECT ?b WHERE { ex:alias geo:sfWithin ?b }".to_owned(),
            "SELECT ?b WHERE { ex:alias geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) }".to_owned(),
            false,
        ),
        (
            "SELECT ?b WHERE { VALUES ?a { ex:alias } ?a geo:sfWithin ?b }".to_owned(),
            "SELECT ?b WHERE { VALUES ?a { ex:alias } ?a geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) }".to_owned(),
            false,
        ),
        (
            format!("SELECT ?b WHERE {{ GRAPH {in_graph} {{ ex:alias geo:sfWithin ?b }} }}"),
            format!("SELECT ?b WHERE {{ GRAPH {in_graph} {{ ex:alias geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) }} }}"),
            false,
        ),
        (
            "SELECT ?g ?a ?b WHERE { GRAPH ?g { ?a geo:sfWithin ?b } }".to_owned(),
            "SELECT ?g ?a ?b WHERE { GRAPH ?g { ?a geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) } }".to_owned(),
            true,
        ),
        (
            format!("SELECT ?g ?a ?b FROM NAMED {in_graph} WHERE {{ GRAPH ?g {{ ?a geo:sfWithin ?b }} }}"),
            format!("SELECT ?g ?a ?b FROM NAMED {in_graph} WHERE {{ GRAPH ?g {{ ?a geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) }} }}"),
            true,
        ),
        (
            format!("SELECT ?g ?x WHERE {{ GRAPH ?g {{ ?x geo:asWKT ?w FILTER(geof:sfWithin(?w, {box_})) }} }}"),
            format!("SELECT ?g ?x WHERE {{ GRAPH ?g {{ ?x geo:asWKT ?w FILTER(geof:sfWithin(?w, {box_})) }} }}"),
            true,
        ),
    ];
    for canonical in [false, true] {
        let options = QueryOptions {
            equality_canonical: canonical,
            equality_early_expansion: true,
            ..QueryOptions::default()
        };
        // The scans alone: no relation pattern, no seed.
        let scans = QueryOptions {
            as_written: true,
            ..options.clone()
        };
        for (pattern, scanned, some) in &pairs {
            let (got, _) = run(&snapshot, pattern, &options);
            let (expected, _) = run(&snapshot, scanned, &scans);
            assert_eq!(got, expected, "canonical {canonical}: {pattern}");
            assert!(
                !*some || !got.is_empty(),
                "canonical {canonical}: {pattern}"
            );
        }
        // Canonical answers name the alias by its class, whose representative holds the
        // shape in the default graph.
        if canonical {
            let (rows, _) = run(&snapshot, &pairs[0].0, &options);
            assert!(rows.contains(&format!("<{EX}box>")), "{rows:?}");
        }
    }
}
