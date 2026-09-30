//! GeoSPARQL 1.1 filter functions over geometry literals: WKT (`geo:wktLiteral`), GeoJSON
//! (`geo:geoJSONLiteral`) and GML (`geo:gmlLiteral`, the simple features; see
//! [`super::geo_formats`]).
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
//! system's own unit. `buffer` in metres on CRS84 buffers in a plane about the geometry
//! (true within 1% up to tens of kilometres; not near the poles). The boolean operations
//! (`intersection`, `union`, …) take polygons. Constructed coordinates are rounded to 9
//! decimal places. Constructions return WKT; `asGeoJSON`
//! takes CRS84 geometries.

use geo::{
    Area, BooleanOps, BoundingRect, Buffer, Centroid, Closest, ClosestPoint, ConvexHull,
    CoordsIter, Distance, Euclidean, Geodesic, GeodesicArea, HasDimensions, Length, MapCoords,
    Relate,
};
use geo::{Geometry, MultiPolygon, Point};
use oxrdf::{Literal, NamedNode, Term};
use wkt::{ToWkt, TryFromWkt};

use super::geo_formats;
use super::value::boolean_term;

/// The GeoSPARQL function namespace.
pub(crate) const GEOF: &str = "http://www.opengis.net/def/function/geosparql/";
const WKT_LITERAL: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";
const GEOJSON_LITERAL: &str = "http://www.opengis.net/ont/geosparql#geoJSONLiteral";
const GML_LITERAL: &str = "http://www.opengis.net/ont/geosparql#gmlLiteral";
const CRS84: &str = "http://www.opengis.net/def/crs/OGC/1.3/CRS84";
const EPSG_4326: &str = "http://www.opengis.net/def/crs/EPSG/0/4326";
const UOM: &str = "http://www.opengis.net/def/uom/OGC/1.0/";

/// How a geometry literal is written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Serialisation {
    Wkt,
    GeoJson,
    Gml,
}

/// A geometry, its coordinate reference system, and how its literal was written.
pub(super) struct Shape {
    pub(super) geometry: Geometry<f64>,
    pub(super) crs: String,
    pub(super) format: Serialisation,
}

impl Shape {
    fn geographic(&self) -> bool {
        self.crs == CRS84
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

/// The shape of a geometry literal (WKT, GeoJSON or GML); `None` for anything else.
pub(super) fn parse(term: &Term) -> Option<Shape> {
    let Term::Literal(literal) = term else {
        return None;
    };
    let text = literal.value().trim();
    let format = match literal.datatype().as_str() {
        GEOJSON_LITERAL => Serialisation::GeoJson,
        GML_LITERAL => Serialisation::Gml,
        _ => Serialisation::Wkt,
    };
    let (crs, geometry) = match literal.datatype().as_str() {
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
                Geometry::GeometryCollection(geo::GeometryCollection::default())
            } else {
                Geometry::<f64>::try_from_wkt_str(text).ok()?
            };
            (crs, geometry)
        }
        GEOJSON_LITERAL => (CRS84.to_owned(), geo_formats::from_geojson(text)?),
        GML_LITERAL if text.is_empty() => (
            CRS84.to_owned(),
            Geometry::GeometryCollection(geo::GeometryCollection::default()),
        ),
        GML_LITERAL => {
            let (srs, geometry) = geo_formats::from_gml(text)?;
            (
                srs.map_or_else(|| CRS84.to_owned(), |s| srs_iri(&s)),
                geometry,
            )
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

/// A constructed geometry as a literal written like `like`'s: WKT, GeoJSON (only in
/// CRS84, else WKT) or GML.
fn literal(geometry: &Geometry<f64>, like: &Shape) -> Term {
    match like.format {
        Serialisation::GeoJson if like.crs == CRS84 => Literal::new_typed_literal(
            geo_formats::to_geojson(geometry),
            NamedNode::new_unchecked(GEOJSON_LITERAL),
        )
        .into(),
        Serialisation::Gml => Literal::new_typed_literal(
            geo_formats::to_gml(geometry, &like.crs),
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

/// A buffer of `metres` around a CRS84 geometry: buffered in an equirectangular plane
/// about its centroid (x scaled by the cosine of the latitude), so distances in metres are
/// true near the geometry: within 1% for buffers up to tens of kilometres. `None` within
/// about a degree of a pole, where the plane fails.
fn metre_buffer(geometry: &Geometry<f64>, metres: f64) -> Option<MultiPolygon<f64>> {
    // The mean radius of the WGS84 ellipsoid.
    const R: f64 = 6_371_008.8;
    let centre = geometry.centroid()?;
    let (lon0, lat0) = (centre.x(), centre.y());
    let k = lat0.to_radians().cos();
    if k < 0.02 {
        return None;
    }
    let plane = geometry.map_coords(|c| {
        geo::coord! {
            x: (c.x - lon0).to_radians() * R * k,
            y: (c.y - lat0).to_radians() * R,
        }
    });
    Some(plane.buffer(metres).map_coords(|c| {
        geo::coord! {
            x: lon0 + (c.x / (R * k)).to_degrees(),
            y: lat0 + (c.y / R).to_degrees(),
        }
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

/// The topological relations, by their local names (functions in `geof:`, properties in
/// `geo:`).
pub(super) const RELATIONS: &[&str] = &[
    "sfEquals",
    "sfDisjoint",
    "sfIntersects",
    "sfTouches",
    "sfCrosses",
    "sfWithin",
    "sfContains",
    "sfOverlaps",
    "ehEquals",
    "ehDisjoint",
    "ehMeet",
    "ehOverlap",
    "ehCovers",
    "ehCoveredBy",
    "ehInside",
    "ehContains",
    "rcc8eq",
    "rcc8dc",
    "rcc8ec",
    "rcc8po",
    "rcc8tppi",
    "rcc8tpp",
    "rcc8ntpp",
    "rcc8ntppi",
];

/// Whether a relation can hold between shapes whose bounding boxes don't meet (the
/// disjointness relations): these can't be looked up in a spatial index.
pub(super) fn holds_apart(local: &str) -> bool {
    matches!(local, "sfDisjoint" | "ehDisjoint" | "rcc8dc")
}

/// Whether relation `local` holds between `a` and `b`; `None` for an unknown name.
pub(super) fn relation(local: &str, a: &Geometry<f64>, b: &Geometry<f64>) -> Option<bool> {
    // Two empty geometries are equal (their DE-9IM matrix is all F, which no equality
    // pattern matches).
    if a.is_empty() && b.is_empty() && matches!(local, "sfEquals" | "ehEquals" | "rcc8eq") {
        return Some(true);
    }
    let pattern = match local {
        "sfDisjoint" | "ehDisjoint" => "FF*FF****",
        "ehOverlap" => "T*T***T**",
        "ehCovers" => "T*TFT*FF*",
        "ehCoveredBy" => "TFF*TFT**",
        "ehInside" => "TFF*FFT**",
        "ehContains" => "T*TFF*FF*",
        "rcc8eq" => "TFFFTFFFT",
        "rcc8dc" => "FFTFFTTTT",
        "rcc8ec" => "FFTFTTTTT",
        "rcc8po" => "TTTTTTTTT",
        "rcc8tppi" => "TTTFTTFFT",
        "rcc8tpp" => "TFFTTFTTT",
        "rcc8ntpp" => "TFFTFFTTT",
        "rcc8ntppi" => "TTTFFTFFT",
        other => {
            let m = a.relate(b);
            return Some(match other {
                "sfEquals" | "ehEquals" => m.is_equal_topo(),
                "sfIntersects" => m.is_intersects(),
                "sfTouches" | "ehMeet" => m.is_touches(),
                "sfCrosses" => m.is_crosses(),
                "sfWithin" => m.is_within(),
                "sfContains" => m.is_contains(),
                "sfOverlaps" => m.is_overlaps(),
                _ => return None,
            });
        }
    };
    a.relate(b).matches(pattern).ok()
}

/// `geof:name(args)`; `None` is an error (a wrong argument, systems that differ).
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
            let (a, b) = pair()?;
            let unit = unit(args.get(2)?)?;
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
            let a = shape(0)?;
            let unit = unit(args.get(1)?)?;
            match (a.geographic(), unit) {
                (true, "metre" | "meter" | "squareMetre" | "squareMeter") => {
                    double(a.geometry.geodesic_area_unsigned())
                }
                (true, _) => None,
                (false, _) => double(a.geometry.unsigned_area()),
            }
        }
        "length" => {
            let a = shape(0)?;
            let unit = unit(args.get(1)?)?;
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
            let a = shape(0)?;
            let radius = match args.get(1)? {
                Term::Literal(l) => l.value().parse::<f64>().ok()?,
                _ => return None,
            };
            let unit = unit(args.get(2)?)?;
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
            let rect = a.geometry.bounding_rect()?;
            Some(literal(&Geometry::Polygon(rect.to_polygon()), &a))
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
                    geo_formats::to_geojson(&a.geometry),
                    NamedNode::new_unchecked(GEOJSON_LITERAL),
                )
                .into()
            })
        }
        _ => None,
    }
}
