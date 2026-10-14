//! The dictionary's geometry literals (`Snapshot::geometries`): by datatype, whatever
//! property states them; nothing built before a query asks; extended with the literals
//! interned since.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, QuadRef};

fn store(engine: &Engine, literals: &[(&str, &str)]) {
    let mut tx = engine.transaction();
    for (i, (datatype, text)) in literals.iter().enumerate() {
        let quad = Quad::new(
            NamedNode::new_unchecked(format!("http://example.com/s{i}{text}")),
            NamedNode::new_unchecked("http://example.com/anyProperty"),
            Literal::new_typed_literal(*text, NamedNode::new_unchecked(*datatype)),
            GraphName::DefaultGraph,
        );
        tx.insert(QuadRef::from(&quad));
    }
    tx.commit().expect("commit");
}

#[test]
fn geometry_literals_are_indexed_by_datatype_when_first_asked_for() {
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let xsd_string = "http://www.w3.org/2001/XMLSchema#string";
    store(
        &engine,
        &[
            (nrese_geo::WKT_LITERAL, "POINT(1 2)"),
            (nrese_geo::WKT_LITERAL, "LINESTRING(0 0, 4 4)"),
            (nrese_geo::WKT_LITERAL, "POINT(nonsense)"),
            (
                nrese_geo::GEOJSON_LITERAL,
                r#"{"type":"Point","coordinates":[9,9]}"#,
            ),
            (xsd_string, "POINT(3 3)"),
        ],
    );
    // A store without spatial queries pays nothing.
    assert_eq!(engine.stats().dictionary.geometry_bytes, 0);
    let snapshot = engine.snapshot();
    let index = snapshot.geometries();
    assert_eq!(
        index.len(),
        3,
        "two WKT literals and one GeoJSON literal read"
    );
    assert!(engine.stats().dictionary.geometry_bytes > 0);
    let near = geo::Rect::new(
        geo::coord! { x: 0.5, y: 1.5 },
        geo::coord! { x: 1.5, y: 2.5 },
    );
    let found = index.meeting(&near);
    assert_eq!(found.len(), 2, "the point and the line's box");
    // Literals interned later are taken in at the next call, the earlier ones kept.
    store(&engine, &[(nrese_geo::WKT_LITERAL, "POINT(1 2.1)")]);
    let index = engine.snapshot().geometries();
    assert_eq!(index.len(), 4);
    assert_eq!(index.meeting(&near).len(), 3);
    let point = engine
        .snapshot()
        .lookup(
            Literal::new_typed_literal(
                "POINT(1 2.1)",
                NamedNode::new_unchecked(nrese_geo::WKT_LITERAL),
            )
            .as_ref()
            .into(),
        )
        .expect("interned");
    let entry = index.get(point.raw()).expect("indexed");
    assert_eq!(entry.as_point(), Some(geo::Point::new(1.0, 2.1)));
}
