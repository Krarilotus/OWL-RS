//! GeoJSON and GML geometry literals (`geo:geoJSONLiteral`, `geo:gmlLiteral`), next to WKT.
//!
//! GeoJSON (RFC 7946) is always in CRS84: a geometry object, or a feature (its geometry)
//! or feature collection (a collection of their geometries). GML is read for the simple
//! features: `Point`, `LineString`, `LinearRing`, `Polygon`, `Envelope` and their `Multi…`
//! forms (`MultiCurve`, `MultiSurface` and GML 2's `MultiLineString`, `MultiPolygon`,
//! `outerBoundaryIs`), coordinates in `pos`, `posList` or GML 2's `coordinates`, in the
//! system `srsName` names (CRS84 without one). Curves with arcs and 3D solids aren't read.

use geo::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon, Rect,
};
use quick_xml::events::{BytesStart, Event};
use serde_json::{Value, json};

/// The geometry of a GeoJSON text.
pub(super) fn from_geojson(text: &str) -> Option<Geometry<f64>> {
    geojson_geometry(&serde_json::from_str(text).ok()?)
}

fn position(value: &Value) -> Option<Coord<f64>> {
    let numbers = value.as_array()?;
    Some(Coord {
        x: numbers.first()?.as_f64()?,
        y: numbers.get(1)?.as_f64()?,
    })
}

fn positions(value: &Value) -> Option<Vec<Coord<f64>>> {
    value.as_array()?.iter().map(position).collect()
}

fn rings(value: &Value) -> Option<Polygon<f64>> {
    let mut rings = value
        .as_array()?
        .iter()
        .map(|ring| positions(ring).map(LineString::new));
    let exterior = match rings.next() {
        Some(ring) => ring?,
        None => LineString::new(Vec::new()),
    };
    Some(Polygon::new(exterior, rings.collect::<Option<_>>()?))
}

fn many<T>(value: &Value, one: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    value.as_array()?.iter().map(one).collect()
}

fn geojson_geometry(value: &Value) -> Option<Geometry<f64>> {
    let coordinates = value.get("coordinates");
    Some(match value.get("type")?.as_str()? {
        "Point" => match coordinates?.as_array()?.is_empty() {
            // An empty point has no geo type of its own: an empty collection.
            true => Geometry::GeometryCollection(GeometryCollection::default()),
            false => Geometry::Point(Point(position(coordinates?)?)),
        },
        "LineString" => Geometry::LineString(LineString::new(positions(coordinates?)?)),
        "Polygon" => Geometry::Polygon(rings(coordinates?)?),
        "MultiPoint" => Geometry::MultiPoint(MultiPoint::new(
            positions(coordinates?)?.into_iter().map(Point).collect(),
        )),
        "MultiLineString" => {
            Geometry::MultiLineString(MultiLineString::new(many(coordinates?, |l| {
                positions(l).map(LineString::new)
            })?))
        }
        "MultiPolygon" => Geometry::MultiPolygon(MultiPolygon::new(many(coordinates?, rings)?)),
        "GeometryCollection" => Geometry::GeometryCollection(GeometryCollection::new_from(many(
            value.get("geometries")?,
            geojson_geometry,
        )?)),
        "Feature" => geojson_geometry(value.get("geometry")?)?,
        "FeatureCollection" => Geometry::GeometryCollection(GeometryCollection::new_from(many(
            value.get("features")?,
            geojson_geometry,
        )?)),
        _ => return None,
    })
}

/// The GeoJSON text of a geometry (in CRS84).
pub(super) fn to_geojson(geometry: &Geometry<f64>) -> String {
    geojson_value(geometry).to_string()
}

fn coord(c: &Coord<f64>) -> Value {
    json!([c.x, c.y])
}

fn line(l: &LineString<f64>) -> Value {
    Value::Array(l.0.iter().map(coord).collect())
}

fn polygon(p: &Polygon<f64>) -> Value {
    Value::Array(
        std::iter::once(p.exterior())
            .chain(p.interiors())
            .map(line)
            .collect(),
    )
}

fn geojson_value(geometry: &Geometry<f64>) -> Value {
    let typed =
        |kind: &str, coordinates: Value| json!({ "type": kind, "coordinates": coordinates });
    match geometry {
        Geometry::Point(p) => typed("Point", coord(&p.0)),
        Geometry::Line(l) => typed("LineString", json!([coord(&l.start), coord(&l.end)])),
        Geometry::LineString(l) => typed("LineString", line(l)),
        Geometry::Polygon(p) => typed("Polygon", polygon(p)),
        Geometry::MultiPoint(m) => typed(
            "MultiPoint",
            Value::Array(m.0.iter().map(|p| coord(&p.0)).collect()),
        ),
        Geometry::MultiLineString(m) => typed(
            "MultiLineString",
            Value::Array(m.0.iter().map(line).collect()),
        ),
        Geometry::MultiPolygon(m) => typed(
            "MultiPolygon",
            Value::Array(m.0.iter().map(polygon).collect()),
        ),
        Geometry::GeometryCollection(c) => json!({
            "type": "GeometryCollection",
            "geometries": c.0.iter().map(geojson_value).collect::<Vec<_>>(),
        }),
        Geometry::Rect(r) => typed("Polygon", polygon(&r.to_polygon())),
        Geometry::Triangle(t) => typed("Polygon", polygon(&t.to_polygon())),
    }
}

/// The GML 3.2 text of a geometry in reference system `crs`.
pub(super) fn to_gml(geometry: &Geometry<f64>, crs: &str) -> String {
    const NS: &str = "http://www.opengis.net/ont/gml";
    fn positions(coords: impl Iterator<Item = Coord<f64>>) -> String {
        coords
            .map(|c| format!("{} {}", c.x, c.y))
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn ring(l: &LineString<f64>) -> String {
        format!(
            "<gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing>",
            positions(l.0.iter().copied())
        )
    }
    fn polygon(p: &Polygon<f64>) -> String {
        let mut out = format!(
            "<gml:Polygon><gml:exterior>{}</gml:exterior>",
            ring(p.exterior())
        );
        for interior in p.interiors() {
            out.push_str(&format!("<gml:interior>{}</gml:interior>", ring(interior)));
        }
        out + "</gml:Polygon>"
    }
    fn body(geometry: &Geometry<f64>) -> String {
        match geometry {
            Geometry::Point(p) => format!(
                "<gml:Point><gml:pos>{} {}</gml:pos></gml:Point>",
                p.x(),
                p.y()
            ),
            Geometry::Line(l) => format!(
                "<gml:LineString><gml:posList>{}</gml:posList></gml:LineString>",
                positions([l.start, l.end].into_iter())
            ),
            Geometry::LineString(l) => format!(
                "<gml:LineString><gml:posList>{}</gml:posList></gml:LineString>",
                positions(l.0.iter().copied())
            ),
            Geometry::Polygon(p) => polygon(p),
            Geometry::MultiPoint(m) => format!(
                "<gml:MultiPoint>{}</gml:MultiPoint>",
                m.0.iter()
                    .map(|p| format!(
                        "<gml:pointMember>{}</gml:pointMember>",
                        body(&Geometry::Point(*p))
                    ))
                    .collect::<String>()
            ),
            Geometry::MultiLineString(m) => format!(
                "<gml:MultiCurve>{}</gml:MultiCurve>",
                m.0.iter()
                    .map(|l| format!(
                        "<gml:curveMember>{}</gml:curveMember>",
                        body(&Geometry::LineString(l.clone()))
                    ))
                    .collect::<String>()
            ),
            Geometry::MultiPolygon(m) => format!(
                "<gml:MultiSurface>{}</gml:MultiSurface>",
                m.0.iter()
                    .map(|p| format!("<gml:surfaceMember>{}</gml:surfaceMember>", polygon(p)))
                    .collect::<String>()
            ),
            Geometry::GeometryCollection(c) => format!(
                "<gml:MultiGeometry>{}</gml:MultiGeometry>",
                c.0.iter()
                    .map(|g| format!("<gml:geometryMember>{}</gml:geometryMember>", body(g)))
                    .collect::<String>()
            ),
            Geometry::Rect(r) => polygon(&r.to_polygon()),
            Geometry::Triangle(t) => polygon(&t.to_polygon()),
        }
    }
    // The namespace and reference system on the outermost element.
    let text = body(geometry);
    let end = text.find('>').unwrap_or(text.len());
    let (open, rest) = text.split_at(end);
    let srs = crs.replace('&', "&amp;").replace('"', "&quot;");
    format!("{open} xmlns:gml=\"{NS}\" srsName=\"{srs}\"{rest}")
}

/// An element of a GML text: its local name, `srsName` and `srsDimension`, text and
/// children.
#[derive(Default)]
struct Element {
    name: String,
    srs: Option<String>,
    dimension: Option<usize>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn open(start: &BytesStart<'_>) -> Option<Self> {
        let mut element = Self {
            name: String::from_utf8(start.local_name().as_ref().to_vec()).ok()?,
            ..Self::default()
        };
        for attribute in start.attributes() {
            let attribute = attribute.ok()?;
            let value = attribute.unescape_value().ok()?;
            match attribute.key.local_name().as_ref() {
                b"srsName" => element.srs = Some(value.into_owned()),
                b"srsDimension" => element.dimension = value.trim().parse().ok(),
                _ => {}
            }
        }
        Some(element)
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
}

fn parse_xml(text: &str) -> Option<Element> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<Element> = Vec::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(start) => stack.push(Element::open(&start)?),
            Event::Empty(start) => {
                let element = Element::open(&start)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => return Some(element),
                }
            }
            Event::Text(text) => {
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&text.unescape().ok()?);
                }
            }
            Event::End(_) => {
                let element = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => return Some(element),
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// The geometry of a GML text and its `srsName`.
pub(super) fn from_gml(text: &str) -> Option<(Option<String>, Geometry<f64>)> {
    let root = parse_xml(text)?;
    let geometry = gml_geometry(&root, root.dimension.unwrap_or(2))?;
    Some((root.srs.clone(), geometry))
}

/// The coordinates of an element with `pos`, `posList` or `coordinates` children.
fn gml_coords(element: &Element, dimension: usize) -> Option<Vec<Coord<f64>>> {
    let mut out = Vec::new();
    for child in &element.children {
        let dimension = child.dimension.unwrap_or(dimension).max(2);
        match child.name.as_str() {
            "pos" | "posList" | "lowerCorner" | "upperCorner" => {
                let numbers: Vec<f64> = child
                    .text
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .ok()?;
                if !numbers.len().is_multiple_of(dimension) {
                    return None;
                }
                out.extend(
                    numbers
                        .chunks(dimension)
                        .map(|c| Coord { x: c[0], y: c[1] }),
                );
            }
            "coordinates" => {
                for tuple in child.text.split_whitespace() {
                    let mut numbers = tuple.split(',').map(str::parse::<f64>);
                    let x = numbers.next()?.ok()?;
                    let y = numbers.next()?.ok()?;
                    out.push(Coord { x, y });
                }
            }
            "Point" => out.extend(gml_coords(child, dimension)?),
            _ => {}
        }
    }
    Some(out)
}

/// The members of a multi-geometry: children of its member properties, which may hold
/// one (`pointMember`) or several (`pointMembers`) geometries.
fn members<'a>(element: &'a Element, properties: &[&str]) -> Vec<&'a Element> {
    element
        .children
        .iter()
        .filter(|c| properties.contains(&c.name.as_str()))
        .flat_map(|c| c.children.iter())
        .collect()
}

fn gml_polygon(element: &Element, dimension: usize) -> Option<Polygon<f64>> {
    let ring = |boundary: &Element| -> Option<LineString<f64>> {
        let ring = boundary.child("LinearRing")?;
        Some(LineString::new(gml_coords(ring, dimension)?))
    };
    let exterior = match element
        .children
        .iter()
        .find(|c| c.name == "exterior" || c.name == "outerBoundaryIs")
    {
        Some(boundary) => ring(boundary)?,
        None => LineString::new(Vec::new()),
    };
    let interiors = element
        .children
        .iter()
        .filter(|c| c.name == "interior" || c.name == "innerBoundaryIs")
        .map(ring)
        .collect::<Option<_>>()?;
    Some(Polygon::new(exterior, interiors))
}

fn gml_geometry(element: &Element, dimension: usize) -> Option<Geometry<f64>> {
    let dimension = element.dimension.unwrap_or(dimension);
    let each = |properties: &[&str]| -> Option<Vec<Geometry<f64>>> {
        members(element, properties)
            .into_iter()
            .map(|m| gml_geometry(m, dimension))
            .collect()
    };
    Some(match element.name.as_str() {
        "Point" => match gml_coords(element, dimension)?.as_slice() {
            [c] => Geometry::Point(Point(*c)),
            // The empty point (geo has no empty Point).
            [] => Geometry::GeometryCollection(GeometryCollection::default()),
            _ => return None,
        },
        "LineString" | "LinearRing" => {
            Geometry::LineString(LineString::new(gml_coords(element, dimension)?))
        }
        "Polygon" => Geometry::Polygon(gml_polygon(element, dimension)?),
        "Envelope" | "Box" => match gml_coords(element, dimension)?.as_slice() {
            [low, high] => Geometry::Rect(Rect::new(*low, *high)),
            _ => return None,
        },
        "MultiPoint" => Geometry::MultiPoint(MultiPoint::new(
            each(&["pointMember", "pointMembers"])?
                .into_iter()
                .map(|g| match g {
                    Geometry::Point(p) => Some(p),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        )),
        "MultiCurve" | "MultiLineString" => Geometry::MultiLineString(MultiLineString::new(
            each(&["curveMember", "curveMembers", "lineStringMember"])?
                .into_iter()
                .map(|g| match g {
                    Geometry::LineString(l) => Some(l),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        )),
        "MultiSurface" | "MultiPolygon" => Geometry::MultiPolygon(MultiPolygon::new(
            each(&["surfaceMember", "surfaceMembers", "polygonMember"])?
                .into_iter()
                .map(|g| match g {
                    Geometry::Polygon(p) => Some(p),
                    Geometry::Rect(r) => Some(r.to_polygon()),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        )),
        "MultiGeometry" => Geometry::GeometryCollection(GeometryCollection::new_from(each(&[
            "geometryMember",
            "geometryMembers",
        ])?)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geojson_round_trip() {
        let text = r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[1,1],[2,1],[2,2],[1,1]]]}"#;
        let geometry = from_geojson(text).unwrap();
        let Geometry::Polygon(p) = &geometry else {
            panic!()
        };
        assert_eq!(p.interiors().len(), 1);
        assert_eq!(from_geojson(&to_geojson(&geometry)).unwrap(), geometry);
        let feature = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[13.4,52.5,34]}}"#;
        assert_eq!(
            from_geojson(feature).unwrap(),
            Geometry::Point(Point::new(13.4, 52.5))
        );
        assert!(from_geojson(r#"{"type":"Point","coordinates":["a"]}"#).is_none());
        assert!(from_geojson("POINT(1 2)").is_none());
    }

    #[test]
    fn gml_simple_features() {
        let point = r#"<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2" srsName="http://www.opengis.net/def/crs/EPSG/0/4326"><gml:pos>52.5 13.4</gml:pos></gml:Point>"#;
        let (srs, geometry) = from_gml(point).unwrap();
        assert_eq!(
            srs.as_deref(),
            Some("http://www.opengis.net/def/crs/EPSG/0/4326")
        );
        assert_eq!(geometry, Geometry::Point(Point::new(52.5, 13.4)));
        let polygon = r#"<gml:Polygon xmlns:gml="http://www.opengis.net/gml/3.2" srsDimension="3">
            <gml:exterior><gml:LinearRing><gml:posList>0 0 1 4 0 1 4 4 1 0 0 1</gml:posList></gml:LinearRing></gml:exterior>
            <gml:interior><gml:LinearRing><gml:pos>1 1 0</gml:pos><gml:pos>2 1 0</gml:pos><gml:pos>2 2 0</gml:pos><gml:pos>1 1 0</gml:pos></gml:LinearRing></gml:interior>
        </gml:Polygon>"#;
        let (srs, geometry) = from_gml(polygon).unwrap();
        assert_eq!(srs, None);
        let Geometry::Polygon(p) = geometry else {
            panic!()
        };
        assert_eq!(p.exterior().0.len(), 4);
        assert_eq!(p.exterior().0[1], Coord { x: 4.0, y: 0.0 });
        assert_eq!(p.interiors()[0].0.len(), 4);
        let gml2 = r#"<gml:MultiPolygon xmlns:gml="http://www.opengis.net/gml"><gml:polygonMember><gml:Polygon><gml:outerBoundaryIs><gml:LinearRing><gml:coordinates>0,0 1,0 1,1 0,0</gml:coordinates></gml:LinearRing></gml:outerBoundaryIs></gml:Polygon></gml:polygonMember></gml:MultiPolygon>"#;
        let (_, geometry) = from_gml(gml2).unwrap();
        assert!(matches!(geometry, Geometry::MultiPolygon(m) if m.0.len() == 1));
        let multi = r#"<MultiPoint><pointMembers><Point><pos>1 2</pos></Point><Point><pos>3 4</pos></Point></pointMembers></MultiPoint>"#;
        assert!(matches!(from_gml(multi).unwrap().1, Geometry::MultiPoint(m) if m.0.len() == 2));
        let envelope =
            r#"<Envelope><lowerCorner>0 0</lowerCorner><upperCorner>2 3</upperCorner></Envelope>"#;
        assert!(matches!(from_gml(envelope).unwrap().1, Geometry::Rect(_)));
        assert!(from_gml("<Point><pos>1 2 3</pos></Point>").is_none());
        // Written and read back: the same geometry and system.
        let (_, geometry) = from_gml(polygon).unwrap();
        let written = to_gml(&geometry, "http://www.opengis.net/def/crs/EPSG/0/3857");
        let (srs, again) = from_gml(&written).unwrap();
        assert_eq!(
            srs.as_deref(),
            Some("http://www.opengis.net/def/crs/EPSG/0/3857")
        );
        assert_eq!(again, geometry);
        assert!(from_gml("<Curve/>").is_none());
        assert!(from_gml("<Point>").is_none());
    }
}
