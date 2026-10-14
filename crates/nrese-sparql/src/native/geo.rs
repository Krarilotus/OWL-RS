//! GeoSPARQL 1.1 filter functions over geometry literals: WKT (`geo:wktLiteral`), GeoJSON
//! (`geo:geoJSONLiteral`) and GML (`geo:gmlLiteral`, the simple features; see
//! [`nrese_geo`], which reads them and owns the relations).
//!
//! | Functions | |
//! |---|---|
//! | Simple Features, Egenhofer, RCC8 relations | `geof:sfWithin`, `ehCovers`, `rcc8ntpp`, … and `geof:relate(a, b, "T*F**F***")` (DE-9IM) |
//! | Measures | `geof:distance(a, b, unit)`, `geof:area(a, unit)`, `geof:length(a, unit)` |
//! | Constructions | `buffer`, `convexHull`, `boundary`, `envelope`, `centroid`, `intersection`, `union`, `difference`, `symDifference` |
//! | Properties | `getSRID`, `isEmpty`, `dimension`, `asWKT`, `asGeoJSON` |
//!
//! Coordinates are in the literal's reference system: CRS84 (longitude, latitude) unless
//! the literal starts with another system's IRI; EPSG:4326 (latitude, longitude) is read
//! as CRS84. Two geometries in different systems aren't compared (an error). In CRS84,
//! metres are geodesic (distances between the nearest points, areas and lengths on the
//! WGS84 ellipsoid); degrees and radians are planar. In other systems a measure is in the
//! system's own unit. `buffer` in metres on CRS84 buffers in the azimuthal equidistant
//! projection about the geometry (geodesic from its centre; not near the poles). The boolean operations
//! (`intersection`, `union`, …) take polygons. Constructed coordinates are rounded to 9
//! decimal places. Constructions return WKT; `asGeoJSON`
//! takes CRS84 geometries.

use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Centroid, Closest, ClosestPoint, ConvexHull,
    CoordsIter, Distance, Euclidean, Geodesic, GeodesicArea, HasDimensions, Length, MapCoords,
    Relate,
};
use geo::{Geometry, MultiPolygon, Point};
use nrese_geo::formats;
use nrese_geo::{CRS84, GEOJSON_LITERAL, GML_LITERAL, Serialisation, Shape, WKT_LITERAL};
pub(super) use nrese_geo::{RELATIONS, holds_apart, relation};
use nrese_rdf::{Literal, NamedNode, Term};
use wkt::ToWkt;

use super::value::boolean_term;

/// The GeoSPARQL function namespace.
pub(crate) const GEOF: &str = "http://www.opengis.net/def/function/geosparql/";
const UOM: &str = "http://www.opengis.net/def/uom/OGC/1.0/";

/// The shape of a geometry literal (WKT, GeoJSON or GML); `None` for anything else.
pub(super) fn parse(term: &Term) -> Option<Shape> {
    let Term::Literal(literal) = term else {
        return None;
    };
    nrese_geo::parse(literal.datatype().as_str(), literal.value())
}

/// A constructed geometry as a literal written like `like`'s: WKT, GeoJSON (only in
/// CRS84, else WKT) or GML.
fn literal(geometry: &Geometry<f64>, like: &Shape) -> Term {
    match like.format {
        Serialisation::GeoJson if like.crs == CRS84 => Literal::new_typed_literal(
            formats::to_geojson(geometry),
            NamedNode::new_unchecked(GEOJSON_LITERAL),
        )
        .into(),
        Serialisation::Gml => Literal::new_typed_literal(
            formats::to_gml(geometry, &like.crs),
            NamedNode::new_unchecked(GML_LITERAL),
        )
        .into(),
        _ => wkt_literal(geometry, &like.crs),
    }
}

fn wkt_literal(shape: &Geometry<f64>, crs: &str) -> Term {
    let text = shape.wkt_string();
    let text = if crs == CRS84 {
        text
    } else {
        format!("<{crs}> {text}")
    };
    Literal::new_typed_literal(text, NamedNode::new_unchecked(WKT_LITERAL)).into()
}

/// The boundary of a geometry (Simple Features): a polygon's rings, a line's end points
/// (none for a closed line), none for points; for several lines, the end points that end
/// an odd number of them.
fn boundary(geometry: &Geometry<f64>) -> Geometry<f64> {
    use geo::{GeometryCollection, LineString, MultiLineString, MultiPoint};
    let empty = || Geometry::GeometryCollection(GeometryCollection::default());
    let rings = |polygons: &[geo::Polygon<f64>]| -> Geometry<f64> {
        let mut lines: Vec<LineString<f64>> = Vec::new();
        for p in polygons {
            lines.push(p.exterior().clone());
            lines.extend(p.interiors().iter().cloned());
        }
        match lines.len() {
            0 => empty(),
            1 => Geometry::LineString(lines.remove(0)),
            _ => Geometry::MultiLineString(MultiLineString::new(lines)),
        }
    };
    let ends = |lines: &[LineString<f64>]| -> Geometry<f64> {
        let mut counted: Vec<(geo::Coord<f64>, usize)> = Vec::new();
        for line in lines.iter().filter(|l| l.0.len() > 1 && !l.is_closed()) {
            for end in [line.0[0], line.0[line.0.len() - 1]] {
                match counted.iter_mut().find(|(c, _)| *c == end) {
                    Some((_, n)) => *n += 1,
                    None => counted.push((end, 1)),
                }
            }
        }
        let points: Vec<Point> = counted
            .into_iter()
            .filter(|(_, n)| n % 2 == 1)
            .map(|(c, _)| Point(c))
            .collect();
        if points.is_empty() {
            empty()
        } else {
            Geometry::MultiPoint(MultiPoint::new(points))
        }
    };
    match geometry {
        Geometry::Point(_) | Geometry::MultiPoint(_) => empty(),
        Geometry::Line(l) => ends(&[LineString::new(vec![l.start, l.end])]),
        Geometry::LineString(l) => ends(std::slice::from_ref(l)),
        Geometry::MultiLineString(m) => ends(&m.0),
        Geometry::Polygon(p) => rings(std::slice::from_ref(p)),
        Geometry::MultiPolygon(m) => rings(&m.0),
        Geometry::Rect(r) => rings(&[r.to_polygon()]),
        Geometry::Triangle(t) => rings(&[t.to_polygon()]),
        Geometry::GeometryCollection(c) => {
            Geometry::GeometryCollection(GeometryCollection::new_from(
                c.0.iter().map(boundary).filter(|g| !g.is_empty()).collect(),
            ))
        }
    }
}

/// The sag of a metre buffer's arcs (the gap between an arc and its chord) as a share of
/// the radius, and in metres at least: 222 segments a circle above 100 m, fewer below
/// (44 at 4 m), and no point more than a ten-thousandth of the radius inside the true
/// buffer is left out.
const BUFFER_SAG: f64 = 1e-4;
const BUFFER_SAG_METRES: f64 = 0.01;

/// A buffer of `metres` around a CRS84 geometry: buffered in the azimuthal equidistant
/// projection about its centroid on the WGS84 ellipsoid, whose distances and directions
/// from the centre are the geodesic ones, so a point's buffer has its vertices on the
/// geodesic circle (and other geometries' are true near the centre: within 0.1% to tens
/// of kilometres). The arcs' segments sag at most [`BUFFER_SAG`] of the radius. `None`
/// within about a degree of a pole, where the result wraps around it.
fn metre_buffer(geometry: &Geometry<f64>, metres: f64) -> Option<MultiPolygon<f64>> {
    use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
    use geo::{Bearing, Destination};
    let centre = geometry.centroid()?;
    if centre.y().abs() > 88.85 {
        return None;
    }
    let plane = geometry.map_coords(|c| {
        let point = Point::from(c);
        let distance = Geodesic.distance(centre, point);
        let bearing = Geodesic.bearing(centre, point).to_radians();
        geo::coord! { x: distance * bearing.sin(), y: distance * bearing.cos() }
    });
    let sag = (metres * BUFFER_SAG).max(BUFFER_SAG_METRES) / metres;
    // The angle an arc's segment spans: its sag is radius × (1 − cos(angle / 2)).
    let angle = 2.0 * (1.0 - sag.min(1.0)).acos();
    let style = BufferStyle::new(metres)
        .line_join(LineJoin::Round(angle))
        .line_cap(LineCap::Round(angle));
    Some(plane.buffer_with_style(style).map_coords(|c| {
        let distance = c.x.hypot(c.y);
        let bearing = c.x.atan2(c.y).to_degrees();
        Geodesic.destination(centre, bearing, distance).0
    }))
}

/// A constructed geometry with its coordinates rounded to 9 decimal places: the clipping
/// and buffering arithmetic leaves noise such as -83.60000000018627.
fn rounded(geometry: Geometry<f64>) -> Geometry<f64> {
    geometry.map_coords(|c| {
        geo::coord! {
            x: (c.x * 1e9).round() / 1e9,
            y: (c.y * 1e9).round() / 1e9,
        }
    })
}

fn double(value: f64) -> Option<Term> {
    value.is_finite().then(|| Literal::from(value).into())
}

/// The unit a measure is asked in: the local name in the OGC unit namespace.
fn unit(term: &Term) -> Option<&str> {
    let iri = match term {
        Term::NamedNode(n) => n.as_str(),
        Term::Literal(l) if l.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#anyURI" => {
            l.value()
        }
        _ => return None,
    };
    iri.strip_prefix(UOM)
}

/// Degrees per unit of a geographic system's planar measures, or `None` for metres, which
/// are geodesic.
fn planar_factor(unit: &str) -> Option<Option<f64>> {
    match unit {
        "metre" | "meter" => Some(None),
        "degree" => Some(Some(1.0)),
        "radian" => Some(Some(1.0_f64.to_degrees())),
        _ => None,
    }
}

/// The geodesic distance in metres between the nearest points of two geographic shapes.
fn geodesic_distance(a: &Geometry<f64>, b: &Geometry<f64>) -> Option<f64> {
    if a.relate(b).is_intersects() {
        return Some(0.0);
    }
    let mut best = f64::INFINITY;
    let mut nearest = |from: &Geometry<f64>, to: &Geometry<f64>| {
        for coord in from.coords_iter() {
            let point = Point::from(coord);
            let closest = match to.closest_point(&point) {
                Closest::Intersection(p) | Closest::SinglePoint(p) => p,
                Closest::Indeterminate => continue,
            };
            best = best.min(Geodesic.distance(point, closest));
        }
    };
    nearest(a, b);
    nearest(b, a);
    best.is_finite().then_some(best)
}

fn polygons(geometry: &Geometry<f64>) -> Option<MultiPolygon<f64>> {
    match geometry {
        Geometry::Polygon(p) => Some(MultiPolygon(vec![p.clone()])),
        Geometry::MultiPolygon(m) => Some(m.clone()),
        Geometry::Rect(r) => Some(MultiPolygon(vec![r.to_polygon()])),
        Geometry::Triangle(t) => Some(MultiPolygon(vec![t.to_polygon()])),
        _ => None,
    }
}

/// `geof:envelope` of a geometry whose bounding box is `rect`, in reference system `crs`,
/// written as `format` (the geometry's own literal): the box as a polygon.
pub(super) fn envelope(rect: &geo::Rect<f64>, crs: &str, format: Serialisation) -> Term {
    let like = Shape {
        geometry: Geometry::Polygon(rect.to_polygon()),
        crs: crs.to_owned(),
        format,
    };
    literal(&like.geometry, &like)
}

/// `geof:name(args)`; `None` is an error (a wrong argument, systems that differ). The
/// arguments that aren't geometries are checked first: a call that fails on them reads no
/// geometry (Geographica's `geof:area(?g)` without a unit, over 44,834 polygons).
pub(crate) fn call(name: &str, args: &[Term]) -> Option<Term> {
    let local = name.strip_prefix(GEOF)?;
    let shape = |i: usize| args.get(i).and_then(parse);
    let pair = || -> Option<(Shape, Shape)> {
        let (a, b) = (shape(0)?, shape(1)?);
        (a.crs == b.crs).then_some((a, b))
    };
    if RELATIONS.contains(&local) {
        let (a, b) = pair()?;
        return relation(local, &a.geometry, &b.geometry).map(boolean_term);
    }
    match local {
        "relate" => {
            let Some(Term::Literal(spec)) = args.get(2) else {
                return None;
            };
            let spec = spec.value().to_owned();
            let (a, b) = pair()?;
            a.geometry
                .relate(&b.geometry)
                .matches(&spec)
                .ok()
                .map(boolean_term)
        }
        "distance" => {
            let unit = unit(args.get(2)?)?;
            let (a, b) = pair()?;
            if a.geographic() {
                match planar_factor(unit)? {
                    None => double(geodesic_distance(&a.geometry, &b.geometry)?),
                    Some(per_degree) => {
                        double(Euclidean.distance(&a.geometry, &b.geometry) / per_degree)
                    }
                }
            } else {
                double(Euclidean.distance(&a.geometry, &b.geometry))
            }
        }
        "area" => {
            let unit = unit(args.get(1)?)?;
            let a = shape(0)?;
            match (a.geographic(), unit) {
                (true, "metre" | "meter" | "squareMetre" | "squareMeter") => {
                    double(a.geometry.geodesic_area_unsigned())
                }
                (true, _) => None,
                (false, _) => double(a.geometry.unsigned_area()),
            }
        }
        "length" => {
            let unit = unit(args.get(1)?)?;
            let a = shape(0)?;
            let lines = |metric: &dyn Fn(&geo::LineString<f64>) -> f64| -> f64 {
                match &a.geometry {
                    Geometry::LineString(l) => metric(l),
                    Geometry::MultiLineString(m) => m.0.iter().map(metric).sum(),
                    Geometry::Line(l) => metric(&geo::LineString::from(vec![l.start, l.end])),
                    Geometry::Polygon(p) => metric(p.exterior()),
                    _ => 0.0,
                }
            };
            match (a.geographic(), planar_factor(unit)) {
                (true, Some(None)) => double(lines(&|l| Geodesic.length(l))),
                (true, Some(Some(per_degree))) => {
                    double(lines(&|l| Euclidean.length(l)) / per_degree)
                }
                (true, None) => None,
                (false, _) => double(lines(&|l| Euclidean.length(l))),
            }
        }
        "buffer" => {
            let radius = match args.get(1)? {
                Term::Literal(l) => l.value().parse::<f64>().ok()?,
                _ => return None,
            };
            let unit = unit(args.get(2)?)?;
            let a = shape(0)?;
            let buffered = match (a.geographic(), planar_factor(unit)?) {
                // In CRS84, metres: in a plane about the geometry.
                (true, None) => metre_buffer(&a.geometry, radius)?,
                // In CRS84, degrees and radians: planar.
                (true, Some(factor)) => a.geometry.buffer(radius * factor),
                (false, _) => a.geometry.buffer(radius),
            };
            Some(literal(&rounded(Geometry::MultiPolygon(buffered)), &a))
        }
        "convexHull" => {
            let a = shape(0)?;
            Some(literal(&Geometry::Polygon(a.geometry.convex_hull()), &a))
        }
        "envelope" => {
            let a = shape(0)?;
            Some(envelope(&a.geometry.bounding_rect()?, &a.crs, a.format))
        }
        "boundary" => {
            let a = shape(0)?;
            Some(literal(&boundary(&a.geometry), &a))
        }
        "centroid" => {
            let a = shape(0)?;
            Some(literal(&Geometry::Point(a.geometry.centroid()?), &a))
        }
        "intersection" | "union" | "difference" | "symDifference" => {
            let (a, b) = pair()?;
            let (x, y) = (polygons(&a.geometry)?, polygons(&b.geometry)?);
            let result = match local {
                "intersection" => x.intersection(&y),
                "union" => x.union(&y),
                "difference" => x.difference(&y),
                _ => x.xor(&y),
            };
            Some(literal(&rounded(Geometry::MultiPolygon(result)), &a))
        }
        "getSRID" => {
            let a = shape(0)?;
            Some(
                Literal::new_typed_literal(
                    a.crs,
                    NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#anyURI"),
                )
                .into(),
            )
        }
        "isEmpty" => Some(boolean_term(shape(0)?.geometry.is_empty())),
        "dimension" => {
            let dimension: i64 = match shape(0)?.geometry.dimensions() {
                geo::dimensions::Dimensions::Empty => return None,
                geo::dimensions::Dimensions::ZeroDimensional => 0,
                geo::dimensions::Dimensions::OneDimensional => 1,
                geo::dimensions::Dimensions::TwoDimensional => 2,
            };
            Some(Literal::from(dimension).into())
        }
        "asWKT" => {
            let a = shape(0)?;
            Some(wkt_literal(&a.geometry, &a.crs))
        }
        "asGeoJSON" => {
            let a = shape(0)?;
            a.geographic().then(|| {
                Literal::new_typed_literal(
                    formats::to_geojson(&a.geometry),
                    NamedNode::new_unchecked(GEOJSON_LITERAL),
                )
                .into()
            })
        }
        _ => None,
    }
}
