//! GeoSPARQL 1.1 filter functions over geometry literals: WKT (`geo:wktLiteral`), GeoJSON
//! (`geo:geoJSONLiteral`) and GML (`geo:gmlLiteral`, the simple features; see
//! [`super::geo_formats`]).
//!
//! | Functions | |
//! |---|---|
//! | Simple Features, Egenhofer, RCC8 relations | `geof:sfWithin`, `ehCovers`, `rcc8ntpp`, … and `geof:relate(a, b, "T*F**F***")` (DE-9IM) |
//! | Measures | `geof:distance(a, b, unit)`, `geof:area(a, unit)`, `geof:length(a, unit)` |
//! | Constructions | `buffer`, `convexHull`, `envelope`, `centroid`, `intersection`, `union`, `difference`, `symDifference` |
//! | Properties | `getSRID`, `isEmpty`, `dimension`, `asWKT`, `asGeoJSON` |
//!
//! Coordinates are in the literal's reference system: CRS84 (longitude, latitude) unless
//! the literal starts with another system's IRI; EPSG:4326 (latitude, longitude) is read
//! as CRS84. Two geometries in different systems aren't compared (an error). In CRS84,
//! metres are geodesic (distances between the nearest points, areas and lengths on the
//! WGS84 ellipsoid); degrees and radians are planar. In other systems a measure is in the
//! system's own unit. `buffer` in metres needs a projected system. The boolean operations
//! (`intersection`, `union`, …) take polygons. Constructions return WKT; `asGeoJSON`
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

/// The functions and how many arguments each takes.
const FUNCTIONS: &[(&str, usize)] = &[
    ("sfEquals", 2),
    ("sfDisjoint", 2),
    ("sfIntersects", 2),
    ("sfTouches", 2),
    ("sfCrosses", 2),
    ("sfWithin", 2),
    ("sfContains", 2),
    ("sfOverlaps", 2),
    ("ehEquals", 2),
    ("ehDisjoint", 2),
    ("ehMeet", 2),
    ("ehOverlap", 2),
    ("ehCovers", 2),
    ("ehCoveredBy", 2),
    ("ehInside", 2),
    ("ehContains", 2),
    ("rcc8eq", 2),
    ("rcc8dc", 2),
    ("rcc8ec", 2),
    ("rcc8po", 2),
    ("rcc8tppi", 2),
    ("rcc8tpp", 2),
    ("rcc8ntpp", 2),
    ("rcc8ntppi", 2),
    ("relate", 3),
    ("distance", 3),
    ("area", 2),
    ("length", 2),
    ("buffer", 3),
    ("convexHull", 1),
    ("envelope", 1),
    ("centroid", 1),
    ("intersection", 2),
    ("union", 2),
    ("difference", 2),
    ("symDifference", 2),
    ("getSRID", 1),
    ("isEmpty", 1),
    ("dimension", 1),
    ("asWKT", 1),
    ("asGeoJSON", 1),
];

/// Whether `name` is a GeoSPARQL function the executor evaluates with `arity` arguments.
pub(crate) fn supported(name: &str, arity: usize) -> bool {
    name.strip_prefix(GEOF)
        .is_some_and(|local| FUNCTIONS.contains(&(local, arity)))
}

/// A geometry and its coordinate reference system.
pub(super) struct Shape {
    pub(super) geometry: Geometry<f64>,
    pub(super) crs: String,
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
    let (crs, geometry) = match literal.datatype().as_str() {
        WKT_LITERAL => {
            let (crs, text) = match text.strip_prefix('<') {
                Some(rest) => {
                    let (iri, rest) = rest.split_once('>')?;
                    (iri.to_owned(), rest.trim())
                }
                None => (CRS84.to_owned(), text),
            };
            (crs, Geometry::<f64>::try_from_wkt_str(text).ok()?)
        }
        GEOJSON_LITERAL => (CRS84.to_owned(), geo_formats::from_geojson(text)?),
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
        }
    } else {
        Shape { geometry, crs }
    })
}

fn literal(shape: &Geometry<f64>, crs: &str) -> Term {
    let text = shape.wkt_string();
    let text = if crs == CRS84 {
        text
    } else {
        format!("<{crs}> {text}")
    };
    Literal::new_typed_literal(text, NamedNode::new_unchecked(WKT_LITERAL)).into()
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
            // In CRS84 the buffer is planar, in degrees.
            let radius = if a.geographic() {
                radius * planar_factor(unit)??
            } else {
                radius
            };
            let buffered = a.geometry.buffer(radius);
            Some(literal(&Geometry::MultiPolygon(buffered), &a.crs))
        }
        "convexHull" => {
            let a = shape(0)?;
            Some(literal(
                &Geometry::Polygon(a.geometry.convex_hull()),
                &a.crs,
            ))
        }
        "envelope" => {
            let a = shape(0)?;
            let rect = a.geometry.bounding_rect()?;
            Some(literal(&Geometry::Polygon(rect.to_polygon()), &a.crs))
        }
        "centroid" => {
            let a = shape(0)?;
            Some(literal(&Geometry::Point(a.geometry.centroid()?), &a.crs))
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
            Some(literal(&Geometry::MultiPolygon(result), &a.crs))
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
            Some(literal(&a.geometry, &a.crs))
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
