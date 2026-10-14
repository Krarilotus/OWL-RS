//! Geometry literals read into shapes: WKT (`geo:wktLiteral`), GeoJSON
//! (`geo:geoJSONLiteral`) and GML (`geo:gmlLiteral`, the simple features; see
//! [`crate::formats`]).
//!
//! Coordinates are in the literal's reference system: CRS84 (longitude, latitude) unless
//! the literal starts with another system's IRI; EPSG:4326 (latitude, longitude) is read
//! as CRS84.

use geo::{BoundingRect, Geometry, MapCoords, Rect};
use wkt::TryFromWkt;

use crate::formats;

/// GeoSPARQL's ontology namespace.
pub const GEO: &str = "http://www.opengis.net/ont/geosparql#";
pub const WKT_LITERAL: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";
pub const GEOJSON_LITERAL: &str = "http://www.opengis.net/ont/geosparql#geoJSONLiteral";
pub const GML_LITERAL: &str = "http://www.opengis.net/ont/geosparql#gmlLiteral";
/// The datatypes of the geometry literals this crate reads: a literal of one of them is a
/// geometry (GeoSPARQL 1.1, §8.5), whatever property states it.
pub const DATATYPES: [&str; 3] = [WKT_LITERAL, GEOJSON_LITERAL, GML_LITERAL];
pub const CRS84: &str = "http://www.opengis.net/def/crs/OGC/1.3/CRS84";
const EPSG_4326: &str = "http://www.opengis.net/def/crs/EPSG/0/4326";

/// How a geometry literal is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Serialisation {
    Wkt,
    GeoJson,
    Gml,
}

/// A geometry, its coordinate reference system, and how its literal was written.
#[derive(Debug, Clone)]
pub struct Shape {
    pub geometry: Geometry<f64>,
    pub crs: String,
    pub format: Serialisation,
}

impl Shape {
    /// Whether the shape is in CRS84 (longitude, latitude in degrees).
    pub fn geographic(&self) -> bool {
        self.crs == CRS84
    }

    /// The bounding box; `None` for an empty geometry.
    pub fn rect(&self) -> Option<Rect<f64>> {
        self.geometry.bounding_rect()
    }
}

/// Heap capacity retained by a geometry, excluding the inline value and parser scratch.
/// Coordinates and public collection vectors use their actual capacities. Polygon's
/// private ring-vector capacity is estimated from WKT's geometric growth and in-place
/// conversion: even no interior rings can retain the original ring-list allocation.
pub fn geometry_heap_bytes(geometry: &Geometry<f64>) -> usize {
    use geo::{Coord, LineString};
    let line = |line: &LineString<f64>| line.0.capacity().saturating_mul(size_of::<Coord<f64>>());
    let polygon = |polygon: &geo::Polygon<f64>| {
        let rings = polygon.interiors();
        let headers = rings
            .len()
            .saturating_add(1)
            .max(4)
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX)
            .saturating_mul(
                size_of::<wkt::types::LineString<f64>>().max(size_of::<LineString<f64>>()),
            );
        rings.iter().fold(
            line(polygon.exterior()).saturating_add(headers),
            |bytes, ring| bytes.saturating_add(line(ring)),
        )
    };
    match geometry {
        Geometry::LineString(g) => line(g),
        Geometry::Polygon(g) => polygon(g),
        Geometry::MultiPoint(g) => g.0.capacity().saturating_mul(size_of::<geo::Point<f64>>()),
        Geometry::MultiLineString(g) => g.0.iter().fold(
            g.0.capacity().saturating_mul(size_of::<LineString<f64>>()),
            |bytes, ring| bytes.saturating_add(line(ring)),
        ),
        Geometry::MultiPolygon(g) => g.0.iter().fold(
            g.0.capacity()
                .saturating_mul(size_of::<geo::Polygon<f64>>()),
            |bytes, part| bytes.saturating_add(polygon(part)),
        ),
        Geometry::GeometryCollection(g) => g.0.iter().fold(
            g.0.capacity().saturating_mul(size_of::<Geometry<f64>>()),
            |bytes, part| bytes.saturating_add(geometry_heap_bytes(part)),
        ),
        Geometry::Point(_) | Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => 0,
    }
}

/// The reference system a GML `srsName` names, in GeoSPARQL's IRI form.
fn srs_iri(name: &str) -> String {
    let code = name
        .strip_prefix("EPSG:")
        .or_else(|| name.strip_prefix("urn:ogc:def:crs:EPSG::"))
        .or_else(|| name.strip_prefix("http://www.opengis.net/def/crs/EPSG/0/"));
    match code {
        Some(code) => format!("http://www.opengis.net/def/crs/EPSG/0/{code}"),
        None if name == "urn:ogc:def:crs:OGC:1.3:CRS84" || name == "urn:ogc:def:crs:OGC::CRS84" => {
            CRS84.to_owned()
        }
        None => name.to_owned(),
    }
}

fn empty() -> Geometry<f64> {
    Geometry::GeometryCollection(geo::GeometryCollection::default())
}

thread_local! {
    static PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The literals [`parse`] has read on this thread: what a guard counts to show that a path
/// reads no geometry (the index's boxes, envelopes and points serve it instead).
pub fn parses_on_this_thread() -> u64 {
    PARSES.with(std::cell::Cell::get)
}

/// The shape of a literal with lexical form `text` and datatype `datatype` (WKT, GeoJSON
/// or GML); `None` for another datatype or a form that doesn't read.
pub fn parse(datatype: &str, text: &str) -> Option<Shape> {
    PARSES.with(|n| n.set(n.get() + 1));
    let text = text.trim();
    let (format, crs, geometry) = match datatype {
        WKT_LITERAL => {
            let (crs, text) = match text.strip_prefix('<') {
                Some(rest) => {
                    let (iri, rest) = rest.split_once('>')?;
                    (iri.to_owned(), rest.trim())
                }
                None => (CRS84.to_owned(), text),
            };
            // An empty literal is the empty geometry (GeoSPARQL 1.1).
            let geometry = if text.is_empty() {
                empty()
            } else {
                Geometry::<f64>::try_from_wkt_str(text).ok()?
            };
            (Serialisation::Wkt, crs, geometry)
        }
        GEOJSON_LITERAL => (
            Serialisation::GeoJson,
            CRS84.to_owned(),
            formats::from_geojson(text)?,
        ),
        GML_LITERAL if text.is_empty() => (Serialisation::Gml, CRS84.to_owned(), empty()),
        GML_LITERAL => {
            let (srs, geometry) = formats::from_gml(text)?;
            let crs = srs.map_or_else(|| CRS84.to_owned(), |s| srs_iri(&s));
            (Serialisation::Gml, crs, geometry)
        }
        _ => return None,
    };
    Some(if crs == EPSG_4326 {
        Shape {
            geometry: geometry.map_coords(|c| geo::coord! { x: c.y, y: c.x }),
            crs: CRS84.to_owned(),
            format,
        }
    } else {
        Shape {
            geometry,
            crs,
            format,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_read_by_datatype_and_system() {
        let point = parse(WKT_LITERAL, " POINT(1 2) ").unwrap();
        assert_eq!(point.crs, CRS84);
        assert_eq!(point.geometry, Geometry::Point(geo::Point::new(1.0, 2.0)));
        // EPSG:4326 writes latitude first: read as CRS84.
        let swapped = parse(WKT_LITERAL, &format!("<{EPSG_4326}> POINT(2 1)")).unwrap();
        assert_eq!(swapped.crs, CRS84);
        assert_eq!(swapped.geometry, point.geometry);
        let other = parse(
            WKT_LITERAL,
            "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(1 2)",
        )
        .unwrap();
        assert_eq!(other.crs, "http://www.opengis.net/def/crs/EPSG/0/3857");
        assert!(parse(WKT_LITERAL, "").unwrap().rect().is_none());
        assert!(parse(WKT_LITERAL, "POINT(1)").is_none());
        assert!(parse("http://www.w3.org/2001/XMLSchema#string", "POINT(1 2)").is_none());
        let json = parse(GEOJSON_LITERAL, r#"{"type":"Point","coordinates":[1,2]}"#).unwrap();
        assert_eq!(json.geometry, point.geometry);
        assert_eq!(json.format, Serialisation::GeoJson);
    }
}
