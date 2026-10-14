//! GeoSPARQL filter functions (native/geo.rs) against known answers.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const PREFIXES: &str = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> \
    PREFIX ex: <http://example.com/> ";

fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#wktLiteral");
    let as_wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#asWKT");
    for (name, text) in [
        ("square", "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"),
        ("inner", "POLYGON((2 2, 4 2, 4 4, 2 4, 2 2))"),
        ("neighbour", "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"),
        ("overlapping", "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"),
        ("far", "POLYGON((50 50, 60 50, 60 60, 50 60, 50 50))"),
        ("centre", "POINT(5 5)"),
        ("corner", "POINT(0 0)"),
        ("road", "LINESTRING(-5 5, 15 5)"),
    ] {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/{name}")),
                as_wkt.clone(),
                Literal::new_typed_literal(text, wkt.clone()),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine
}

fn select(engine: &Engine, query: &str) -> Vec<Vec<Option<Term>>> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    assert_eq!(
        explain_query(&snapshot, &query, &options).unwrap().executor,
        "native",
        "{text}"
    );
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|s| {
            let s = s.unwrap();
            variables.iter().map(|v| s.get(v).cloned()).collect()
        })
        .collect()
}

/// The features `relation(square-or-other, ?x)` holds for, by local name.
fn related(engine: &Engine, relation: &str, of: &str) -> Vec<String> {
    let rows = select(
        engine,
        &format!(
            "SELECT ?x WHERE {{ ex:{of} geo:asWKT ?a . ?x geo:asWKT ?b FILTER(?x != ex:{of} && geof:{relation}(?a, ?b)) }} ORDER BY ?x"
        ),
    );
    rows.into_iter()
        .map(|row| match &row[0] {
            Some(Term::NamedNode(n)) => n
                .as_str()
                .trim_start_matches("http://example.com/")
                .to_owned(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn number(engine: &Engine, expression: &str) -> Option<f64> {
    let rows = select(
        engine,
        &format!("SELECT ?v WHERE {{ BIND({expression} AS ?v) }}"),
    );
    rows[0][0].as_ref().map(|term| match term {
        Term::Literal(l) => l.value().parse().unwrap(),
        other => panic!("{other:?}"),
    })
}

fn wkt(text: &str) -> String {
    format!("\"{text}\"^^geo:wktLiteral")
}

#[test]
fn relations_between_features() {
    let engine = engine();
    assert_eq!(
        related(&engine, "sfContains", "square"),
        ["centre", "inner"]
    );
    assert_eq!(related(&engine, "sfWithin", "inner"), ["square"]);
    assert_eq!(
        related(&engine, "sfTouches", "square"),
        ["corner", "neighbour"]
    );
    assert_eq!(related(&engine, "sfOverlaps", "square"), ["overlapping"]);
    assert_eq!(related(&engine, "sfCrosses", "square"), ["road"]);
    assert_eq!(related(&engine, "sfDisjoint", "square"), ["far"]);
    assert_eq!(
        related(&engine, "sfIntersects", "square"),
        [
            "centre",
            "corner",
            "inner",
            "neighbour",
            "overlapping",
            "road"
        ]
    );
    // Egenhofer and RCC8 names of the same relations.
    assert_eq!(related(&engine, "ehInside", "inner"), ["square"]);
    assert_eq!(
        related(&engine, "ehMeet", "square"),
        ["corner", "neighbour"]
    );
    assert_eq!(related(&engine, "rcc8ntpp", "inner"), ["square"]);
    assert_eq!(related(&engine, "rcc8ec", "square"), ["neighbour"]);
    assert_eq!(related(&engine, "rcc8po", "square"), ["overlapping"]);
    // DE-9IM.
    let rows = select(
        &engine,
        "SELECT ?x WHERE { ex:inner geo:asWKT ?a . ?x geo:asWKT ?b FILTER(geof:relate(?a, ?b, \"T*F**F***\")) } ORDER BY ?x",
    );
    assert_eq!(rows.len(), 2, "within itself and the square");
}

#[test]
fn measures_in_metres_and_degrees() {
    let engine = engine();
    // Berlin to Paris on the WGS84 ellipsoid: 879.7 km (877.5 km on a sphere).
    let berlin_paris = number(
        &engine,
        &format!(
            "geof:distance({}, {}, uom:metre)",
            wkt("POINT(13.405 52.52)"),
            wkt("POINT(2.3522 48.8566)")
        ),
    )
    .unwrap();
    assert!((berlin_paris - 879_699.0).abs() < 100.0, "{berlin_paris}");
    // The same, with EPSG:4326's latitude-first order.
    let swapped = number(
        &engine,
        &format!(
            "geof:distance({}, {}, uom:metre)",
            wkt("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(52.52 13.405)"),
            wkt("POINT(2.3522 48.8566)")
        ),
    )
    .unwrap();
    assert!((swapped - berlin_paris).abs() < 1.0);
    // Planar degrees; from a point to a polygon's nearest edge.
    let degrees = number(
        &engine,
        &format!(
            "geof:distance({}, {}, uom:degree)",
            wkt("POINT(0 0)"),
            wkt("POINT(3 4)")
        ),
    )
    .unwrap();
    assert!((degrees - 5.0).abs() < 1e-9);
    let to_polygon = number(
        &engine,
        &format!(
            "geof:distance({}, {}, uom:metre)",
            wkt("POINT(0 1)"),
            wkt("POLYGON((1 0, 2 0, 2 2, 1 2, 1 0))")
        ),
    )
    .unwrap();
    assert!((to_polygon - 111_300.0).abs() < 1_000.0, "{to_polygon}");
    // A 1° square at the equator: about 12,308 km².
    let area = number(
        &engine,
        &format!(
            "geof:area({}, uom:metre)",
            wkt("POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))")
        ),
    )
    .unwrap();
    assert!((area / 1e6 - 12_308.0).abs() < 100.0, "{area}");
    // One degree of the equator: about 111.3 km.
    let length = number(
        &engine,
        &format!("geof:length({}, uom:metre)", wkt("LINESTRING(0 0, 1 0)")),
    )
    .unwrap();
    assert!((length - 111_319.5).abs() < 50.0, "{length}");
    // Different reference systems aren't compared.
    assert!(
        number(
            &engine,
            &format!(
                "geof:distance({}, {}, uom:metre)",
                wkt("<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(0 0)"),
                wkt("POINT(1 1)")
            ),
        )
        .is_none()
    );
    // In a projected system, measures are in its own unit.
    let projected = number(
        &engine,
        &format!(
            "geof:distance({}, {}, uom:metre)",
            wkt("<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(0 0)"),
            wkt("<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(300 400)")
        ),
    )
    .unwrap();
    assert!((projected - 500.0).abs() < 1e-9);
}

#[test]
fn constructions_and_properties() {
    let engine = engine();
    let one = |expression: &str| -> String {
        let rows = select(
            &engine,
            &format!("SELECT ?v WHERE {{ BIND({expression} AS ?v) }}"),
        );
        match &rows[0][0] {
            Some(Term::Literal(l)) => l.value().to_owned(),
            other => panic!("{expression}: {other:?}"),
        }
    };
    let area_of = |expression: &str| -> f64 {
        number(&engine, &format!("geof:area({expression}, uom:degree)")).unwrap_or_else(|| {
            // Planar areas are asked in the system's own unit: use a projected system.
            number(&engine, &format!("geof:area({expression}, uom:metre)")).unwrap()
        })
    };
    let projected = |text: &str| {
        wkt(&format!(
            "<http://www.opengis.net/def/crs/EPSG/0/3857> {text}"
        ))
    };
    let a = projected("POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))");
    let b = projected("POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))");
    assert!((area_of(&format!("geof:intersection({a}, {b})")) - 25.0).abs() < 1e-6);
    assert!((area_of(&format!("geof:union({a}, {b})")) - 175.0).abs() < 1e-6);
    assert!((area_of(&format!("geof:difference({a}, {b})")) - 75.0).abs() < 1e-6);
    assert!((area_of(&format!("geof:symDifference({a}, {b})")) - 150.0).abs() < 1e-6);
    let line = projected("LINESTRING(0 0, 10 5, 0 10)");
    assert!((area_of(&format!("geof:convexHull({line})")) - 50.0).abs() < 1e-6);
    assert!((area_of(&format!("geof:envelope({line})")) - 100.0).abs() < 1e-6);
    let circle = area_of(&format!(
        "geof:buffer({}, 10, uom:metre)",
        projected("POINT(0 0)")
    ));
    assert!(
        (circle - std::f64::consts::PI * 100.0).abs() < 5.0,
        "{circle}"
    );
    assert_eq!(
        one(&format!(
            "geof:centroid({})",
            wkt("POLYGON((0 0, 4 0, 4 4, 0 4, 0 0))")
        )),
        "POINT(2 2)"
    );
    assert_eq!(
        one(&format!("geof:getSRID({})", wkt("POINT(1 2)"))),
        "http://www.opengis.net/def/crs/OGC/1.3/CRS84"
    );
    assert_eq!(
        one(&format!("geof:dimension({})", wkt("LINESTRING(0 0, 1 1)"))),
        "1"
    );
    assert_eq!(
        one(&format!("geof:isEmpty({})", wkt("POINT(1 2)"))),
        "false"
    );
    // Not a WKT literal: an error, so the BIND leaves the variable unbound.
    assert!(number(&engine, "geof:area(\"POINT(1 2)\", uom:metre)").is_none());

    // Metres on CRS84: a 1 km buffer around Berlin covers about pi km² (geodesic area),
    // and reaches 1 km north, not more.
    let berlin = wkt("POINT(13.4 52.5)");
    let disc = format!("geof:buffer({berlin}, 1000, uom:metre)");
    let area = number(&engine, &format!("geof:area({disc}, uom:metre)")).unwrap();
    assert!(
        (area / (std::f64::consts::PI * 1e6) - 1.0).abs() < 0.02,
        "{area}"
    );
    let within = |lat: f64| {
        one(&format!(
            "geof:sfWithin({}, {disc})",
            wkt(&format!("POINT(13.4 {lat})"))
        ))
    };
    // 990 m and 1010 m north: 1 degree of latitude is about 111.2 km.
    assert!(matches!(
        within(52.5 + 0.99 / 111.25).as_str(),
        "true" | "1"
    ));
    assert!(matches!(
        within(52.5 + 1.01 / 111.25).as_str(),
        "false" | "0"
    ));
    // Constructed coordinates without the clipping arithmetic's noise.
    let square = |x: f64| {
        wkt(&format!(
            "POLYGON(({x} 34.1, {} 34.1, {} 34.5, {x} 34.5, {x} 34.1))",
            x + 0.4,
            x + 0.4
        ))
    };
    let intersection = one(&format!(
        "geof:intersection({}, {})",
        square(-83.6),
        square(-83.4)
    ));
    assert!(!intersection.contains("0000000"), "{intersection}");

    // The boundary: a polygon's ring, a line's end points, nothing for a point.
    assert_eq!(
        one(&format!(
            "geof:boundary({})",
            wkt("POLYGON((0 0, 4 0, 4 4, 0 4, 0 0))")
        )),
        "LINESTRING(0 0,4 0,4 4,0 4,0 0)"
    );
    assert_eq!(
        one(&format!(
            "geof:boundary({})",
            wkt("LINESTRING(0 0, 1 1, 2 0)")
        )),
        "MULTIPOINT((0 0),(2 0))"
    );
    assert!(matches!(
        one(&format!(
            "geof:isEmpty(geof:boundary({}))",
            wkt("POINT(1 2)")
        ))
        .as_str(),
        "true" | "1"
    ));
    // An empty literal is the empty geometry, equal to other empty ones.
    assert!(matches!(
        one(&format!(
            "geof:sfEquals({}, {})",
            wkt(""),
            wkt("LINESTRING EMPTY")
        ))
        .as_str(),
        "true" | "1"
    ));
}

/// A relation stated in the data holds for the triple pattern, also between nodes without
/// geometries; one both stated and computed is one solution.
#[test]
fn stated_relations_count_too() {
    let engine = engine();
    let mut tx = engine.transaction();
    let geo = |local: &str| {
        NamedNode::new_unchecked(format!("http://www.opengis.net/ont/geosparql#{local}"))
    };
    let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
    for (s, p, o) in [("x", "sfTouches", "y"), ("square", "sfContains", "inner")] {
        tx.insert(Quad::new(ex(s), geo(p), ex(o), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let names = |query: &str| -> Vec<String> {
        select(&engine, query)
            .into_iter()
            .map(|row| match &row[0] {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .trim_start_matches("http://example.com/")
                    .to_owned(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(names("SELECT ?b WHERE { ex:x geo:sfTouches ?b }"), ["y"]);
    assert_eq!(names("SELECT ?a WHERE { ?a geo:sfTouches ex:y }"), ["x"]);
    assert_eq!(
        names("SELECT ?a WHERE { ?a geo:sfTouches ?b FILTER(?b = ex:y) }"),
        ["x"]
    );
    let contained = names("SELECT ?b WHERE { ex:square geo:sfContains ?b }");
    assert_eq!(
        contained.iter().filter(|n| *n == "inner").count(),
        1,
        "{contained:?}"
    );
}

/// With the query-rewrite extension off (`geosparql_stated_only`), a relation in a triple
/// pattern matches the statements that assert it and nothing computed from geometries;
/// the filter functions still compute (GeoSPARQL's requirements 4 to 6 against 28 to 30).
#[test]
fn stated_only_reads_the_asserted_relations() {
    let engine = engine();
    let mut tx = engine.transaction();
    let geo = |local: &str| {
        NamedNode::new_unchecked(format!("http://www.opengis.net/ont/geosparql#{local}"))
    };
    let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
    tx.insert(Quad::new(ex("x"), geo("sfContains"), ex("y"), GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    let names = |query: &str, stated_only: bool| -> Vec<String> {
        let query = SparqlParser::new()
            .parse_query(&format!("{PREFIXES}{query}"))
            .unwrap();
        let options = QueryOptions {
            geosparql_stated_only: stated_only,
            ..QueryOptions::default()
        };
        let snapshot = engine.snapshot();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &query, &options).unwrap()
        else {
            panic!("solutions")
        };
        let mut names: Vec<String> = solutions
            .map(|s| match s.unwrap().iter().next().map(|(_, t)| t.clone()) {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .trim_start_matches("http://example.com/")
                    .to_owned(),
                other => panic!("{other:?}"),
            })
            .collect();
        names.sort();
        names
    };
    // The square contains `inner` and the point `centre` by their geometries.
    let computed = names("SELECT ?b WHERE { ex:square geo:sfContains ?b }", false);
    assert!(computed.contains(&"inner".to_owned()), "{computed:?}");
    assert!(names("SELECT ?b WHERE { ex:square geo:sfContains ?b }", true).is_empty());
    for stated_only in [false, true] {
        assert_eq!(
            names("SELECT ?b WHERE { ex:x geo:sfContains ?b }", stated_only),
            ["y"]
        );
    }
    let filtered = "SELECT ?b WHERE { ex:square geo:asWKT ?s . ?b geo:asWKT ?w \
                    FILTER(geof:sfContains(?s, ?w)) }";
    assert_eq!(names(filtered, true), names(filtered, false));
}

/// Relations as triple patterns: between random geometries, `?a geo:R ?b` equals the
/// filter `geof:R(wkt(?a), wkt(?b))` for every relation (the R-tree finds the candidates,
/// or every object for disjointness), and features stand in for their default geometry.
#[test]
fn relations_as_triple_patterns() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#wktLiteral");
    let geo = |local: &str| {
        NamedNode::new_unchecked(format!("http://www.opengis.net/ont/geosparql#{local}"))
    };
    let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
    let mut state: u64 = 11;
    let mut next = |n: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    for i in 0..40 {
        let (x, y) = (next(20), next(20));
        let text = match next(3) {
            0 => format!("POINT({x} {y})"),
            1 => format!("LINESTRING({x} {y}, {} {})", x + next(6), y + next(6)),
            _ => {
                let (w, h) = (1 + next(8), 1 + next(8));
                format!(
                    "POLYGON(({x} {y}, {} {y}, {} {}, {x} {}, {x} {y}))",
                    x + w,
                    x + w,
                    y + h,
                    y + h
                )
            }
        };
        let g = ex(&format!("g{i}"));
        tx.insert(
            Quad::new(
                g.clone(),
                geo("asWKT"),
                Literal::new_typed_literal(text, wkt.clone()),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        if i < 10 {
            tx.insert(
                Quad::new(
                    ex(&format!("f{i}")),
                    geo("hasDefaultGeometry"),
                    g,
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
    }
    tx.commit().unwrap();
    let pairs = |query: &str| -> Vec<String> {
        let mut rows: Vec<String> = select(&engine, query)
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|t| t.as_ref().map_or("-".to_owned(), ToString::to_string))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        rows.sort();
        rows
    };
    let mut nonempty = 0;
    for relation in [
        "sfEquals",
        "sfDisjoint",
        "sfIntersects",
        "sfTouches",
        "sfCrosses",
        "sfWithin",
        "sfContains",
        "sfOverlaps",
        "ehCovers",
        "ehCoveredBy",
        "ehInside",
        "rcc8ec",
        "rcc8po",
        "rcc8ntpp",
        "rcc8dc",
    ] {
        let property = pairs(&format!(
            "SELECT ?a ?b WHERE {{ ?a geo:{relation} ?b . ?a geo:asWKT ?x . ?b geo:asWKT ?y }}"
        ));
        let function = pairs(&format!(
            "SELECT ?a ?b WHERE {{ ?a geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:{relation}(?x, ?y)) }}"
        ));
        assert_eq!(property, function, "{relation}");
        nonempty += usize::from(!property.is_empty());
        // A feature relates as its default geometry does.
        let features = pairs(&format!(
            "SELECT ?b WHERE {{ ex:f3 geo:{relation} ?b . ?b geo:asWKT ?y }}"
        ));
        let geometry = pairs(&format!(
            "SELECT ?b WHERE {{ ex:g3 geo:{relation} ?b . ?b geo:asWKT ?y }}"
        ));
        assert_eq!(features, geometry, "{relation}");
        // From a constant side the candidates come from the R-tree.
        let filtered = pairs(&format!(
            "SELECT ?b WHERE {{ ex:g3 geo:asWKT ?x . ?b geo:asWKT ?y FILTER(geof:{relation}(?x, ?y)) }}"
        ));
        assert_eq!(geometry, filtered, "{relation} from a constant");
    }
    assert!(nonempty >= 10, "{nonempty}");
    // Both sides free; a constant side starting the joins; one pattern with LIMIT.
    let within = pairs("SELECT ?f WHERE { ?f geo:sfWithin ex:g0 . ?f geo:hasDefaultGeometry ?g }");
    let check = pairs(
        "SELECT ?f WHERE { ?f geo:hasDefaultGeometry ?g . ?g geo:asWKT ?x . ex:g0 geo:asWKT ?y FILTER(geof:sfWithin(?x, ?y)) }",
    );
    assert_eq!(within, check);
    assert!(pairs("SELECT ?a ?b WHERE { ?a geo:sfIntersects ?b } LIMIT 3").len() <= 3);
}

/// `FILTER(geof:R(?wkt, constant))` over a `geo:asWKT` pattern starts from the R-tree's
/// candidates: the answers equal the plain evaluation's for every relation, and a small
/// search box reads few rows.
#[test]
fn filter_functions_use_the_index() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#wktLiteral");
    let as_wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#asWKT");
    for i in 0..400u32 {
        let (x, y) = (i % 20, i / 20);
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/g{i}")),
                as_wkt.clone(),
                Literal::new_typed_literal(format!("POINT({x} {y})"), wkt.clone()),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let boxed = "\"POLYGON((2.5 2.5, 5.5 2.5, 5.5 5.5, 2.5 5.5, 2.5 2.5))\"^^geo:wktLiteral";
    for relation in [
        "sfWithin",
        "sfIntersects",
        "sfDisjoint",
        "ehInside",
        "rcc8ntpp",
    ] {
        let text = format!(
            "{PREFIXES}SELECT ?g WHERE {{ ?g geo:asWKT ?w FILTER(geof:{relation}(?w, {boxed})) }}"
        );
        let query = SparqlParser::new().parse_query(&text).unwrap();
        let answer = |options: &QueryOptions| -> Vec<String> {
            let QueryResults::Solutions(solutions) =
                evaluate_query(&snapshot, &query, options).unwrap()
            else {
                panic!()
            };
            let mut rows: Vec<String> = solutions.map(|s| s.unwrap()[0].to_string()).collect();
            rows.sort();
            rows
        };
        let plain = QueryOptions {
            as_written: true,
            ..QueryOptions::default()
        };
        assert_eq!(
            answer(&QueryOptions::default()),
            answer(&plain),
            "{relation}"
        );
    }
    let text = format!(
        "{PREFIXES}SELECT ?g WHERE {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {boxed})) }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    assert_eq!(plan.rows, 9);
    let largest = plan.steps.iter().map(|s| s.rows).max().unwrap();
    assert!(largest < 50, "{:#?}", plan.steps);
}

/// GeoJSON and GML literals take part like WKT ones: in relations as triple patterns, in
/// filters (index-seeded), with GML's EPSG:4326 read latitude first.
#[test]
fn geojson_and_gml_literals() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let geo = |local: &str| {
        NamedNode::new_unchecked(format!("http://www.opengis.net/ont/geosparql#{local}"))
    };
    for (name, property, datatype, text) in [
        (
            "area",
            "asGeoJSON",
            "geoJSONLiteral",
            r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]]]}"#,
        ),
        (
            "inside",
            "asGML",
            "gmlLiteral",
            r#"<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2" srsName="EPSG:4326"><gml:pos>6 5</gml:pos></gml:Point>"#,
        ),
        (
            "outside",
            "asGML",
            "gmlLiteral",
            r#"<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2" srsName="http://www.opengis.net/def/crs/EPSG/0/4326"><gml:pos>1 20</gml:pos></gml:Point>"#,
        ),
        ("same", "asWKT", "wktLiteral", "POINT(20 1)"),
    ] {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/{name}")),
                geo(property),
                Literal::new_typed_literal(text, geo(datatype)),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let names = |rows: Vec<Vec<Option<Term>>>| -> Vec<String> {
        rows.into_iter()
            .map(|row| match &row[0] {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .trim_start_matches("http://example.com/")
                    .to_owned(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(
        names(select(
            &engine,
            "SELECT ?x WHERE { ?x geo:sfWithin ex:area FILTER(?x != ex:area) }"
        )),
        ["inside"]
    );
    assert_eq!(
        names(select(
            &engine,
            "SELECT ?x WHERE { ?x geo:sfEquals ex:same FILTER(?x != ex:same) }"
        )),
        ["outside"]
    );
    let within = r#""{\"type\":\"Polygon\",\"coordinates\":[[[4,4],[8,4],[8,8],[4,8],[4,4]]]}"^^geo:geoJSONLiteral"#;
    assert_eq!(
        names(select(
            &engine,
            &format!("SELECT ?x WHERE {{ ?x geo:asGML ?l FILTER(geof:sfWithin(?l, {within})) }}")
        )),
        ["inside"]
    );
    let rows = select(
        &engine,
        &format!(
            "SELECT ?v WHERE {{ BIND(geof:asGeoJSON({}) AS ?v) }}",
            wkt("POINT(1 2)")
        ),
    );
    let Some(Term::Literal(l)) = &rows[0][0] else {
        panic!("{rows:?}")
    };
    assert_eq!(l.value(), r#"{"coordinates":[1.0,2.0],"type":"Point"}"#);
    assert_eq!(
        l.datatype().as_str(),
        "http://www.opengis.net/ont/geosparql#geoJSONLiteral"
    );
    let projected = wkt("<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(1 2)");
    let rows = select(
        &engine,
        &format!("SELECT ?v WHERE {{ BIND(geof:asGeoJSON({projected}) AS ?v) }}"),
    );
    assert_eq!(rows[0][0], None, "GeoJSON is CRS84 only");
}

/// The objects of a store whose geometry literals (and other terms) hang off a property of
/// its own in a named graph, as Geographica's datasets do (`geonames:asWKT`), and the
/// same again in a second graph: a grid of `side` × `side` points, then lines, polygons
/// and terms that aren't geometries. Each subject has a kind, `"k0"` or `"k1"` by the
/// parity of its position.
fn own_property_store(side: usize) -> (Engine, Vec<(String, Term)>) {
    let wkt = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#wktLiteral");
    let json = NamedNode::new_unchecked("http://www.opengis.net/ont/geosparql#geoJSONLiteral");
    let mut objects: Vec<Term> = Vec::new();
    for i in 0..side * side {
        let text = format!("POINT({} {})", i % side, i / side);
        objects.push(Literal::new_typed_literal(text, wkt.clone()).into());
    }
    for text in [
        "POLYGON((0 0, 3 0, 3 3, 0 3, 0 0))",
        "POLYGON((2 2, 5 2, 5 5, 2 5, 2 2))",
        "POLYGON((5 5, 8 5, 8 8, 5 8, 5 5))",
        "POLYGON((8 1, 11 1, 11 4, 8 4, 8 1))",
        "POLYGON((1 1, 10 1, 10 10, 1 10, 1 1),(3 3, 6 3, 6 6, 3 6, 3 3))",
        "POLYGON((3 3, 4 3, 4 4, 3 4, 3 3))",
        "LINESTRING(0 0, 11 11)",
        "LINESTRING(-1 5, 12 5)",
        "LINESTRING(4 4, 4 7)",
        "MULTIPOINT((4 4),(20 20))",
        "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(5 4)",
        "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(4 4)",
        "",
        "POINT(oops)",
    ] {
        objects.push(Literal::new_typed_literal(text, wkt.clone()).into());
    }
    objects
        .push(Literal::new_typed_literal(r#"{"type":"Point","coordinates":[4,5]}"#, json).into());
    objects.push(Literal::new_simple_literal("POINT(4 4)").into());
    objects.push(NamedNode::new_unchecked("http://example.com/notAGeometry").into());
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let shape = NamedNode::new_unchecked("http://example.com/shape");
    let mut expected = Vec::new();
    for graph in ["g1", "g2"] {
        let name = NamedNode::new_unchecked(format!("http://example.com/{graph}"));
        for (i, object) in objects.iter().enumerate() {
            let subject = format!("http://example.com/{graph}s{i}");
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked(subject.as_str()),
                    shape.clone(),
                    object.clone(),
                    GraphName::NamedNode(name.clone()),
                )
                .as_ref(),
            );
            tx.insert(
                Quad::new(
                    NamedNode::new_unchecked(subject.as_str()),
                    NamedNode::new_unchecked("http://example.com/kind"),
                    Literal::new_simple_literal(format!("k{}", i % 2)),
                    GraphName::NamedNode(name.clone()),
                )
                .as_ref(),
            );
            if graph == "g1" {
                expected.push((format!("<{subject}>"), object.clone()));
            }
        }
    }
    tx.commit().unwrap();
    (engine, expected)
}

/// `FILTER(geof:R(?o, C))` and `geof:R(C, ?o)` where `?o` is bound by a property of the
/// data's own (not `geo:asWKT`) in a named graph: for every relation and constants that
/// are polygons, points, lines and a `geof:buffer` expression, the rows are those whose
/// literal stands in the relation by its definition ([`nrese_geo::relate`], literals in
/// another reference system, empty, unreadable or not geometries excluded). The geometry
/// index gives the candidates by datatype, so the selective filters start from it.
#[test]
fn filters_on_a_property_of_the_data_equal_the_definition() {
    let (engine, objects) = own_property_store(12);
    let snapshot = engine.snapshot();
    let constants = [
        r#""POLYGON((2.5 2.5, 6.5 2.5, 6.5 6.5, 2.5 6.5, 2.5 2.5))"^^geo:wktLiteral"#,
        r#""POLYGON((2 2, 5 2, 5 5, 2 5, 2 2))"^^geo:wktLiteral"#,
        r#""POINT(4 4)"^^geo:wktLiteral"#,
        r#""LINESTRING(0 3, 11 3)"^^geo:wktLiteral"#,
        r#"geof:buffer("POINT(5 5)"^^geo:wktLiteral, 2, uom:degree)"#,
    ];
    let shape_of = |term: &Term| match term {
        Term::Literal(l) => nrese_geo::parse(l.datatype().as_str(), l.value()),
        _ => None,
    };
    for constant in constants {
        let Some(Some(value)) = select(
            &engine,
            &format!("SELECT ?c WHERE {{ BIND({constant} AS ?c) }}"),
        )
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next()) else {
            panic!("{constant}")
        };
        let fixed = shape_of(&value).expect("a geometry constant");
        for local in nrese_geo::RELATIONS {
            for first in [true, false] {
                let call = if first {
                    format!("geof:{local}(?o, {constant})")
                } else {
                    format!("geof:{local}({constant}, ?o)")
                };
                let text = format!(
                    "{PREFIXES}SELECT ?s WHERE {{ GRAPH ex:g1 {{ ?s ex:shape ?o }} FILTER({call}) }}"
                );
                let query = SparqlParser::new().parse_query(&text).unwrap();
                let QueryResults::Solutions(solutions) =
                    evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
                else {
                    panic!()
                };
                let mut rows: Vec<String> = solutions.map(|s| s.unwrap()[0].to_string()).collect();
                rows.sort();
                let mut expected: Vec<String> = objects
                    .iter()
                    .filter(|(_, object)| {
                        shape_of(object).is_some_and(|shape| {
                            let (a, b) = if first {
                                (&shape, &fixed)
                            } else {
                                (&fixed, &shape)
                            };
                            nrese_geo::relate(local, a, b) == Some(true)
                        })
                    })
                    .map(|(subject, _)| subject.clone())
                    .collect();
                expected.sort();
                assert_eq!(rows, expected, "{call}");
            }
        }
    }
    // A geometry the query computes (an id of its own) is left to the generic evaluator.
    let text = format!(
        "SELECT ?s WHERE {{ GRAPH ex:g1 {{ ?s ex:shape ?o }} \
         BIND(geof:centroid(?o) AS ?c) FILTER(geof:sfWithin(?c, {})) }}",
        constants[0]
    );
    let mut rows: Vec<String> = select(&engine, &text)
        .into_iter()
        .map(|row| row[0].as_ref().unwrap().to_string())
        .collect();
    rows.sort();
    let fixed = nrese_geo::parse(
        nrese_geo::WKT_LITERAL,
        "POLYGON((2.5 2.5, 6.5 2.5, 6.5 6.5, 2.5 6.5, 2.5 2.5))",
    )
    .unwrap();
    let mut expected: Vec<String> = objects
        .iter()
        .filter(|(_, object)| {
            shape_of(object).is_some_and(|shape| {
                use geo::Centroid;
                shape.geometry.centroid().is_some_and(|point| {
                    let centre = nrese_geo::Shape {
                        geometry: geo::Geometry::Point(point),
                        ..shape
                    };
                    nrese_geo::relate("sfWithin", &centre, &fixed) == Some(true)
                })
            })
        })
        .map(|(subject, _)| subject.clone())
        .collect();
    expected.sort();
    assert_eq!(rows, expected);
    // The box's 16 points and the shapes whose boxes lie in it start the joins: no step
    // reads the 161 statements of the property in the graph.
    let text = format!(
        "{PREFIXES}SELECT ?s WHERE {{ GRAPH ex:g1 {{ ?s ex:shape ?o }} FILTER(geof:sfWithin(?o, {})) }}",
        constants[0]
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let seeded = plan
        .steps
        .iter()
        .find(|s| s.operator == "geometry index")
        .unwrap_or_else(|| panic!("{:#?}", plan.steps));
    // 16 points, the small square, the EPSG:4326 point and GeoJSON's point, literals both
    // graphs share.
    assert_eq!(seeded.rows, 19, "{:#?}", plan.steps);
    let largest = plan.steps.iter().map(|s| s.rows).max().unwrap();
    assert!(largest <= 19, "{:#?}", plan.steps);
}

/// `FILTER(geof:R(?o1, ?o2))` between parts that share no variable (two `GRAPH` blocks, or
/// the two components of one basic graph pattern): for every relation, the rows are the
/// pairs of the cross product whose literals stand in it by its definition
/// ([`nrese_geo::relate`]); the disjointness relations, which the boxes don't narrow, as
/// before. EXPLAIN shows the spatial join, and an index nested-loop join from a side of
/// one row.
#[test]
fn spatial_joins_equal_the_definition() {
    let (engine, objects) = own_property_store(6);
    let snapshot = engine.snapshot();
    let shape_of = |term: &Term| match term {
        Term::Literal(l) => nrese_geo::parse(l.datatype().as_str(), l.value()),
        _ => None,
    };
    let shapes: Vec<Option<nrese_geo::Shape>> = objects.iter().map(|(_, o)| shape_of(o)).collect();
    let in_graph = |subject: &str, graph: &str| subject.replace("/g1s", &format!("/{graph}s"));
    let rows = |text: &str| -> Vec<String> {
        let query = SparqlParser::new()
            .parse_query(&format!("{PREFIXES}{text}"))
            .unwrap();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
        else {
            panic!()
        };
        let mut rows: Vec<String> = solutions
            .map(|s| {
                let s = s.unwrap();
                format!("{} {}", s[0], s[1])
            })
            .collect();
        rows.sort();
        rows
    };
    for local in nrese_geo::RELATIONS {
        // Two graphs, all subjects on both sides.
        let mut expected = Vec::new();
        // One basic graph pattern in g1, the first side's subjects of kind k0.
        let mut expected_k0 = Vec::new();
        for (i, (s1, _)) in objects.iter().enumerate() {
            for (j, (s2, _)) in objects.iter().enumerate() {
                let (Some(a), Some(b)) = (&shapes[i], &shapes[j]) else {
                    continue;
                };
                if nrese_geo::relate(local, a, b) == Some(true) {
                    expected.push(format!("{s1} {}", in_graph(s2, "g2")));
                    if i % 2 == 0 {
                        expected_k0.push(format!("{s1} {s2}"));
                    }
                }
            }
        }
        expected.sort();
        expected_k0.sort();
        let graphs = format!(
            "SELECT ?s1 ?s2 WHERE {{ GRAPH ex:g1 {{ ?s1 ex:shape ?o1 }} \
             GRAPH ex:g2 {{ ?s2 ex:shape ?o2 }} FILTER(geof:{local}(?o1, ?o2)) }}"
        );
        assert_eq!(rows(&graphs), expected, "{graphs}");
        let components = format!(
            "SELECT ?s1 ?s2 WHERE {{ GRAPH ex:g1 {{ ?s1 ex:shape ?o1 ; ex:kind \"k0\" . \
             ?s2 ex:shape ?o2 }} FILTER(geof:{local}(?o1, ?o2)) }}"
        );
        assert_eq!(rows(&components), expected_k0, "{components}");
    }
    let explain = |text: &str| {
        let query = SparqlParser::new()
            .parse_query(&format!("{PREFIXES}{text}"))
            .unwrap();
        explain_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    };
    let plan = explain(
        "SELECT ?s1 ?s2 WHERE { GRAPH ex:g1 { ?s1 ex:shape ?o1 } \
         GRAPH ex:g2 { ?s2 ex:shape ?o2 } FILTER(geof:sfIntersects(?o1, ?o2)) }",
    );
    let join = plan
        .steps
        .iter()
        .find(|s| s.operator == "spatial join")
        .unwrap_or_else(|| panic!("{:#?}", plan.steps));
    assert_eq!(join.rows, plan.rows, "{:#?}", plan.steps);
    // No step holds the cross product (53 × 53 statements).
    assert!(
        plan.steps.iter().all(|s| s.rows < 53 * 53),
        "{:#?}",
        plan.steps
    );
    // One small square against a grid of 400 points: the 5 literals whose boxes lie in
    // its box (its corners and itself) from the index, the other side evaluated from them.
    let (engine, _) = own_property_store(20);
    let snapshot = engine.snapshot();
    let text = "SELECT ?s2 WHERE { GRAPH ex:g1 { ex:g1s405 ex:shape ?o1 } \
                GRAPH ex:g2 { ?s2 ex:shape ?o2 } FILTER(geof:sfWithin(?o2, ?o1)) }";
    let query = SparqlParser::new()
        .parse_query(&format!("{PREFIXES}{text}"))
        .unwrap();
    let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let seeded = plan
        .steps
        .iter()
        .find(|s| s.operator == "geometry index")
        .unwrap_or_else(|| panic!("{:#?}", plan.steps));
    assert_eq!(seeded.rows, 5, "{:#?}", plan.steps);
    // The square is within itself (its copy in g2), nothing else.
    assert_eq!(plan.rows, 1, "{:#?}", plan.steps);
    assert!(plan.steps.iter().any(|s| s.operator == "spatial join"));
}

/// These query-owned geometry values have computed ids and systems absent from the index.
/// Independent side numbering both invents false matches and loses real ones. Compare
/// exact bags with generic execution, changing encounter order and the smaller side.
#[test]
fn computed_spatial_join_systems_share_identity() {
    let engine = engine();
    let snapshot = engine.snapshot();
    // Non-stored VALUES literals get query-computed ids and are certainly bound.
    // BIND expressions are nullable, so they cannot establish the join's route.
    // Distinct lexical points ensure each side discovers the absent CRSs in its own
    // encounter order while the geometries themselves remain equal.
    let a = "(1 \"<urn:crs:a> POINT(0 0)\"^^geo:wktLiteral) (2 \"<urn:crs:a> POINT(0 0)\"^^geo:wktLiteral) (3 \"<urn:crs:b> POINT(0 0)\"^^geo:wktLiteral) (4 \"<urn:crs:c> POINT(0 0)\"^^geo:wktLiteral)";
    let b = "(5 \"<urn:crs:b> POINT(0.0 0.0)\"^^geo:wktLiteral) (6 \"<urn:crs:b> POINT(0.0 0.0)\"^^geo:wktLiteral) (7 \"<urn:crs:a> POINT(0.0 0.0)\"^^geo:wktLiteral) (8 \"<urn:crs:a> POINT(0.0 0.0)\"^^geo:wktLiteral) (9 \"<urn:crs:d> POINT(0.0 0.0)\"^^geo:wktLiteral)";
    for (left, right) in [(a, b), (b, a)] {
        for relation in ["sfIntersects(?a, ?b)", "sfWithin(?b, ?a)"] {
            let text = format!(
                "{PREFIXES} SELECT ?i ?j WHERE {{ \
                 {{ VALUES (?i ?a) {{ {left} }} }} \
                 {{ VALUES (?j ?b) {{ {right} }} }} FILTER(geof:{relation}) }}"
            );
            let query = SparqlParser::new().parse_query(&text).unwrap();
            let rows = |options: QueryOptions| {
                let QueryResults::Solutions(solutions) =
                    evaluate_query(&snapshot, &query, &options).unwrap()
                else {
                    panic!()
                };
                let mut rows: Vec<_> = solutions
                    .map(|s| {
                        let s = s.unwrap();
                        format!("{} {}", s[0], s[1])
                    })
                    .collect();
                rows.sort();
                rows
            };
            let expected = rows(QueryOptions {
                as_written: true,
                ..QueryOptions::default()
            });
            assert_eq!(expected.len(), 6, "{text}");
            assert_eq!(rows(QueryOptions::default()), expected, "{text}");
            let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
            assert!(
                plan.steps
                    .iter()
                    .any(|step| step.operator == "spatial join"),
                "{:#?}",
                plan.steps
            );
        }
    }
}

/// Repeated values expand one geometric pair into the full bag. The row maps and the
/// result's overlap with the inputs must use the existing request/result owners.
#[test]
fn spatial_join_bags_and_seed_memory_keep_request_ownership() {
    use nrese_exec::{Envelope, SharedBudget};
    use nrese_sparql::{QueryEvaluationError, Resources};

    let engine = engine();
    let snapshot = engine.snapshot();
    let text = format!(
        "{PREFIXES} SELECT ?i ?j WHERE {{ \
        {{ VALUES ?i {{ 1 1 1 1 }} BIND(\"POINT(0 0)\"^^geo:wktLiteral AS ?a) }} \
        {{ VALUES ?j {{ 2 2 2 2 2 }} BIND(\"POINT(0 0)\"^^geo:wktLiteral AS ?b) }} \
        FILTER(geof:sfIntersects(?a, ?b)) }}"
    );
    let bag = SparqlParser::new().parse_query(&text).unwrap();
    let seed = SparqlParser::new()
        .parse_query(&format!(
            "{PREFIXES} SELECT ?s WHERE {{ \
        ?s geo:asWKT ?g FILTER(geof:sfIntersects(?g, \"POINT(5 5)\"^^geo:wktLiteral)) }}"
        ))
        .unwrap();
    // Build the persistent index before the request: it is owned by the dictionary.
    snapshot.geometries();
    for (query, count) in [(&bag, 20), (&seed, 4)] {
        let shared = SharedBudget::new(1024 * 1024);
        let memory = Envelope::new(None, Some(shared.clone()));
        let options = QueryOptions {
            resources: Resources {
                memory: memory.clone(),
                ..Resources::default()
            },
            ..QueryOptions::default()
        };
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, query, &options).unwrap()
        else {
            panic!()
        };
        assert!(
            memory.used() > 0,
            "live results retain their existing owner"
        );
        let rows: Vec<_> = solutions.collect::<Result<_, _>>().unwrap();
        assert_eq!(rows.len(), count);
        assert_eq!(memory.used(), 0);
        assert_eq!(shared.used(), 0);
        drop(rows);
        for shared_limit in [false, true] {
            let shared = SharedBudget::new(if shared_limit { 128 } else { 1024 * 1024 });
            let memory = Envelope::new(
                if shared_limit { None } else { Some(128) },
                Some(shared.clone()),
            );
            let options = QueryOptions {
                resources: Resources {
                    memory: memory.clone(),
                    ..Resources::default()
                },
                ..QueryOptions::default()
            };
            match evaluate_query(&snapshot, query, &options) {
                Err(QueryEvaluationError::MemoryLimit(error)) => {
                    assert_eq!(error.shared, shared_limit)
                }
                _ => panic!("request must refuse its finite budget"),
            }
            assert_eq!(memory.used(), 0);
            assert_eq!(shared.used(), 0);
        }
    }
}

/// A selective seed pays for its few actual hits, not all unrelated indexed literals.
/// The discarded full-index precharge rejects this correctly funded request.
#[test]
fn constant_spatial_seed_charges_hits_not_unrelated_indexed_data() {
    use nrese_exec::Envelope;
    use nrese_sparql::Resources;

    let (engine, _) = own_property_store(40);
    let snapshot = engine.snapshot();
    let index = snapshot.geometries();
    assert!(index.len() * 24 > 8 * 1024);
    let query = SparqlParser::new().parse_query(&format!("{PREFIXES} SELECT ?s WHERE {{ \
        GRAPH ex:g1 {{ ?s ex:shape ?g FILTER(geof:sfIntersects(?g, \"POINT(0 0)\"^^geo:wktLiteral)) }} }}")).unwrap();
    // Determine the answers before checking the same bag under a finite envelope.
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!()
    };
    let rows = |solutions: nrese_sparql::QuerySolutionIter<'_>| {
        let mut rows: Vec<_> = solutions.map(|s| s.unwrap()[0].to_string()).collect();
        rows.sort();
        rows
    };
    let expected = rows(solutions);
    assert_eq!(expected.len(), 3);
    let memory = Envelope::new(Some(8 * 1024), None);
    let options = QueryOptions {
        resources: Resources {
            memory: memory.clone(),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!()
    };
    assert_eq!(rows(solutions), expected);
    assert!(memory.peak() < index.len() * 24);
    assert_eq!(memory.used(), 0);

    // The variable-variable join's optional index seed has the same selective contract.
    let query = SparqlParser::new()
        .parse_query(&format!(
            "{PREFIXES} SELECT ?s WHERE {{ \
        GRAPH ex:g1 {{ ex:g1s1605 ex:shape ?a }} GRAPH ex:g2 {{ ?s ex:shape ?b }} \
        FILTER(geof:sfWithin(?b, ?a)) }}"
        ))
        .unwrap();
    let QueryResults::Solutions(solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!()
    };
    let expected = rows(solutions);
    assert_eq!(expected.len(), 1);
    let memory = Envelope::new(Some(16 * 1024), None);
    let options = QueryOptions {
        resources: Resources {
            memory: memory.clone(),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options).unwrap()
    else {
        panic!()
    };
    assert_eq!(rows(solutions), expected);
    assert!(memory.peak() < index.len() * 24);
    assert_eq!(memory.used(), 0);
}

/// Empty serialisations have no index box. A bound spatial triple must agree with
/// the fully bound exact path, in either direction and when both sides are enumerated.
#[test]
fn empty_spatial_triples_equal_fully_bound_evaluation() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (name, text) in [
        ("e1", ""),
        ("e2", "LINESTRING EMPTY"),
        ("other", "<urn:other:crs> LINESTRING EMPTY"),
        ("point", "POINT(0 0)"),
    ] {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/{name}")),
                NamedNode::new_unchecked(format!("{}asWKT", nrese_geo::GEO)),
                Literal::new_typed_literal(text, NamedNode::new_unchecked(nrese_geo::WKT_LITERAL)),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    for relation in ["sfEquals", "ehEquals", "rcc8eq"] {
        let mut expected = Vec::new();
        for name in ["e1", "e2", "other", "point"] {
            let query = SparqlParser::new()
                .parse_query(&format!(
                    "{PREFIXES} ASK {{ ex:e1 geo:{relation} ex:{name} }}"
                ))
                .unwrap();
            let QueryResults::Boolean(related) =
                evaluate_query(&engine.snapshot(), &query, &QueryOptions::default()).unwrap()
            else {
                panic!()
            };
            if related {
                expected.push(format!("<http://example.com/{name}>"));
            }
        }
        expected.sort();
        assert_eq!(
            expected,
            ["<http://example.com/e1>", "<http://example.com/e2>"]
        );
        for text in [
            format!("SELECT ?b WHERE {{ ex:e1 geo:{relation} ?b }}"),
            format!("SELECT ?b WHERE {{ ?b geo:{relation} ex:e1 }}"),
            format!("SELECT ?b WHERE {{ VALUES ?a {{ ex:e1 }} ?a geo:{relation} ?b }}"),
            format!("SELECT ?b WHERE {{ ?a geo:{relation} ?b FILTER(?a = ex:e1) }}"),
        ] {
            let mut found: Vec<_> = select(&engine, &text)
                .into_iter()
                .map(|row| row[0].as_ref().unwrap().to_string())
                .collect();
            found.sort();
            assert_eq!(found, expected, "{text}");
        }
    }
}

/// A buffer in metres is the geodesic disc: around Geographica Q14's point in Athens, the
/// points half a metre inside its 3,000 m circle are within it in every direction, those
/// half a metre outside aren't (the adjudicated answer counts a point at 2,998.8 m; the
/// equirectangular buffer of 32 segments missed it).
#[test]
fn a_metre_buffer_is_the_geodesic_disc() {
    use geo::{Destination, Geodesic};
    let engine = engine();
    let centre = geo::Point::new(23.71622, 37.97945);
    let disc = "geof:buffer(\"POINT(23.71622 37.97945)\"^^geo:wktLiteral, 3000, uom:metre)";
    for step in 0..24 {
        let bearing = f64::from(step) * 15.0 + 7.0;
        for (metres, inside) in [(2999.5, true), (3000.5, false)] {
            let point = Geodesic.destination(centre, bearing, metres);
            let rows = select(
                &engine,
                &format!(
                    "SELECT ?w WHERE {{ BIND(geof:sfWithin(\"POINT({} {})\"^^geo:wktLiteral, {disc}) AS ?w) }}",
                    point.x(),
                    point.y()
                ),
            );
            let Some(Term::Literal(within)) = &rows[0][0] else {
                panic!("{rows:?}")
            };
            assert_eq!(within.value() == "true", inside, "{metres} m at {bearing}°");
        }
    }
}

/// Projections over stored geometry literals read each geometry once, when the index takes
/// it in: `geof:envelope(?g)` comes from the index's boxes (equal to the function's own
/// answer for every literal, other terms an error), and `geof:area(?g)` without its unit
/// fails before it reads anything (Geographica Q02 and Q06 over CLC's 44,834 polygons).
#[test]
fn projections_read_no_geometry_the_index_holds() {
    let (engine, objects) = own_property_store(6);
    // The index takes the literals in (on other threads).
    engine.snapshot().geometries();
    let rows = |text: &str| select(&engine, text);
    let before = nrese_geo::parses_on_this_thread();
    let envelopes =
        rows("SELECT ?s ?e WHERE { GRAPH ex:g1 { ?s ex:shape ?o } BIND(geof:envelope(?o) AS ?e) }");
    let areas =
        rows("SELECT ?a WHERE { GRAPH ex:g1 { ?s ex:shape ?o } BIND(geof:area(?o) AS ?a) }");
    assert_eq!(
        nrese_geo::parses_on_this_thread(),
        before,
        "no geometry read"
    );
    assert_eq!(envelopes.len(), objects.len());
    assert!(
        areas.iter().all(|row| row[0].is_none()),
        "a unit is required"
    );
    // Each literal's envelope as the function computes it from the literal itself.
    for (subject, object) in &objects {
        let expected = rows(&format!(
            "SELECT ?e WHERE {{ BIND(geof:envelope({object}) AS ?e) }}"
        ))
        .remove(0)
        .remove(0);
        let found = envelopes
            .iter()
            .find(|row| row[0].as_ref().map(Term::to_string).as_deref() == Some(subject.as_str()))
            .unwrap_or_else(|| panic!("{subject}"));
        assert_eq!(found[1], expected, "{object}");
    }
}
