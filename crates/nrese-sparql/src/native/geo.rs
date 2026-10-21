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
use nrese_exec::{Budget, BudgetExceeded, ChargedVec};
use nrese_rdf::{Literal, NamedNode, Term};
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

    /// The unit its measures (`distance`, `area`, `length`, `buffer`) are answered in.
    fn units(&self) -> Units {
        match self.geographic() {
            true => Units::Geographic,
            false => match in_metres(&self.crs) {
                true => Units::Metres,
                false => Units::Unknown,
            },
        }
    }
}

/// What a shape's measures can answer in.
#[derive(Clone, Copy, PartialEq)]
enum Units {
    /// CRS84 (EPSG:4326 read as it): metres geodesic, degrees and radians planar.
    Geographic,
    /// A projected system whose axes are in metres ([`in_metres`]): metres, planar.
    Metres,
    /// Any other system: its coordinates' unit isn't known here, so no requested unit can be
    /// shown to be its own, and its raw coordinate measures would answer in whatever unit was
    /// asked (SPR-19). Its measures are refused, an expression error.
    Unknown,
}

/// Whether `crs` is one of the projected systems whose axis unit is known to be the metre
/// (the EPSG registry): WGS 84 / UTM zones 1N-60N (32601-32660) and 1S-60S (32701-32760),
/// WGS 84 / Pseudo-Mercator (3857), ETRS89 / UTM zones 28N-38N (25828-25838), ETRS89-extended
/// / LAEA Europe (3035), RGF93 / Lambert-93 (2154), OSGB36 / British National Grid (27700)
/// and DHDN / 3-degree Gauss-Krueger zones 2-5 (31466-31469), under any of the identifier
/// forms [`epsg_code`] reads. A CRS database, with units and conversions for every system,
/// comes in v2.0.
fn in_metres(crs: &str) -> bool {
    let Some(code) = epsg_code(crs) else {
        return false;
    };
    matches!(
        code,
        32601..=32660
            | 32701..=32760
            | 3857
            | 25828..=25838
            | 3035
            | 2154
            | 27700
            | 31466..=31469
    )
}

/// The EPSG code `crs` names, in the identifier forms of one EPSG object: the OGC HTTP IRI
/// of any dataset version (`http://www.opengis.net/def/crs/EPSG/0/3857`, `.../EPSG/8.9.2/3857`),
/// the OGC URN with or without a version (`urn:ogc:def:crs:EPSG::3857`,
/// `urn:ogc:def:crs:EPSG:6.18:3:3857`), and GML's legacy URL
/// (`http://www.opengis.net/gml/srs/epsg.xml#3857`). A version is digits and dots, a code
/// digits only: any other suffix names no code.
fn epsg_code(crs: &str) -> Option<u32> {
    let version = |v: &str| {
        v.split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    };
    let (version_ok, code) = match (
        crs.strip_prefix("http://www.opengis.net/def/crs/EPSG/"),
        crs.strip_prefix("urn:ogc:def:crs:EPSG:"),
        crs.strip_prefix("http://www.opengis.net/gml/srs/epsg.xml#"),
    ) {
        (Some(rest), _, _) => {
            let (v, code) = rest.split_once('/')?;
            (version(v), code)
        }
        (_, Some(rest), _) => {
            let (v, code) = rest.rsplit_once(':')?;
            (v.is_empty() || v.split(':').all(version), code)
        }
        (_, _, Some(code)) => (true, code),
        (None, None, None) => return None,
    };
    let digits = !code.is_empty() && code.len() <= 6 && code.bytes().all(|b| b.is_ascii_digit());
    if !(version_ok && digits) {
        return None;
    }
    code.parse().ok()
}

/// Whether `unit` is the metre.
fn metres(unit: &str) -> bool {
    matches!(unit, "metre" | "meter")
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
    parse_literal(literal.value(), literal.datatype().as_str())
}

/// `g` with its polygons' lists of holes no larger than they hold: a parser may leave a
/// list's capacity behind (an in-place collect of its rings), which a shape's size
/// (`spatial::shape_bytes`, from the holes it has) wouldn't count.
fn tight(g: Geometry<f64>) -> Geometry<f64> {
    let polygon = |p: geo::Polygon<f64>| {
        let (exterior, mut holes) = p.into_inner();
        holes.shrink_to_fit();
        geo::Polygon::new(exterior, holes)
    };
    match g {
        Geometry::Polygon(p) => Geometry::Polygon(polygon(p)),
        Geometry::MultiPolygon(m) => {
            Geometry::MultiPolygon(MultiPolygon(m.0.into_iter().map(polygon).collect()))
        }
        Geometry::GeometryCollection(c) => {
            Geometry::GeometryCollection(c.0.into_iter().map(tight).collect())
        }
        g => g,
    }
}

/// The shape of a literal of `datatype` with the lexical form `value` ([`parse`]).
pub(super) fn parse_literal(value: &str, datatype: &str) -> Option<Shape> {
    let text = value.trim();
    let format = match datatype {
        GEOJSON_LITERAL => Serialisation::GeoJson,
        GML_LITERAL => Serialisation::Gml,
        _ => Serialisation::Wkt,
    };
    let (crs, geometry) = match datatype {
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
    let geometry = tight(geometry);
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

/// What geo's `relate` allocates per coordinate of the two geometries: the graph's copy of
/// it in its edge, its segment's entry in the edge-set R-tree, and its share of the nodes,
/// labels and edge ends at the edges' ends. Measured at most 159 bytes (a point in a ring
/// of 1,000, polygons with holes, crossing zigzags); this is with room for growth.
const RELATE_PER_COORDINATE: usize = 256;
/// What `relate` allocates per intersection it records: an edge intersection in a sorted
/// set on each of the two edges, its node in the node map with its star, and the edge ends
/// the edges are split into there with their labelled bundles. Measured at most 717 bytes
/// (self-crossing lines, crossing rings of one polygon, equal lines and rings).
const RELATE_PER_INTERSECTION: usize = 1024;
/// What `relate` allocates whatever the geometries: its maps, intersector and matrix.
/// Measured at most 8.6 KiB.
const RELATE_FIXED: usize = 16 << 10;
/// Below this many bytes, the intersections are bounded by counting segment pairs, not
/// by testing them.
const RELATE_COUNTED_FROM: usize = 64 << 10;

/// The most bytes geo's `relate` allocates for `a` and `b`, known before it runs:
/// [`RELATE_PER_COORDINATE`] for each coordinate, [`RELATE_PER_INTERSECTION`] for each
/// intersection it records, and [`RELATE_FIXED`]. It records what its graphs' segments
/// meet in: within one geometry every intersection between different edges, and within an
/// edge of a geometry that isn't all rings every one but at the point adjacent segments
/// share; between the two geometries every one but a proper crossing (a point inside both
/// segments, which it only flags). A collinear overlap records its two ends; each
/// intersection is recorded on both edges. Mostly there are none worth testing for: the
/// pairs of segments bound them (a point has no segment, a ring has no pair within one
/// geometry). Where the pairs would bound them above [`RELATE_COUNTED_FROM`] bytes, the
/// segments are swept in order of their boxes and each pair whose boxes meet is tested as
/// geo tests it, with the sweep's array charged to `scratch` first: a geometry that
/// crosses itself a million times takes about 2.7 GB in geo, and this bound says so
/// before geo allocates it.
pub(super) fn relate_bound(
    (a, size_a): (&Geometry<f64>, &Size),
    (b, size_b): (&Geometry<f64>, &Size),
    scratch: &Budget,
) -> Result<usize, BudgetExceeded> {
    if let Some(bound) = sized_bound(size_a, size_b) {
        return Ok(bound);
    }
    let intersections = recorded(a, b, size_a, size_b, scratch)?;
    Ok(bytes(
        size_a.coordinates + size_b.coordinates,
        intersections,
    ))
}

/// [`relate_bound`] where the geometries' sizes decide it: where their pairs of segments
/// bound the intersections to [`RELATE_COUNTED_FROM`] bytes. The bound grows with each
/// part of a size, so a size no smaller in any part ([`Size::largest`]) bounds as much.
pub(super) fn sized_bound(a: &Size, b: &Size) -> Option<usize> {
    // A pair of segments records at most two points on each of its two edges; a pair
    // within one geometry comes in both orders (counted so by `within`).
    let pairs = a
        .within
        .saturating_add(b.within)
        .saturating_add(a.segments.saturating_mul(b.segments));
    let intersections = pairs.saturating_mul(4);
    (intersections.saturating_mul(RELATE_PER_INTERSECTION) <= RELATE_COUNTED_FROM)
        .then(|| bytes(a.coordinates + b.coordinates, intersections))
}

fn bytes(coordinates: usize, intersections: usize) -> usize {
    RELATE_FIXED
        .saturating_add(coordinates.saturating_mul(RELATE_PER_COORDINATE))
        .saturating_add(intersections.saturating_mul(RELATE_PER_INTERSECTION))
}

/// What [`relate_bound`] reads of a geometry: its coordinates, its graph's segments, the
/// pairs of them geo intersects within the geometry, and whether its edges are all rings
/// (whose segments geo doesn't intersect within one edge). Known once for a geometry
/// related to many.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Size {
    coordinates: usize,
    segments: usize,
    within: usize,
    rings: bool,
}

impl Size {
    pub(super) fn of(g: &Geometry<f64>) -> Self {
        let coordinates = g.coords_count();
        if let Geometry::Point(_) = g {
            return Self {
                coordinates,
                ..Self::default()
            };
        }
        let (mut segments, mut squares) = (0usize, 0usize);
        edges(g, &mut |edge| {
            let n = edge.segments();
            segments += n;
            squares = squares.saturating_add(n * n);
        });
        let all = segments.saturating_mul(segments);
        let rings = rings(g);
        let within = match rings {
            true => all - squares.min(all),
            false => all,
        };
        Self {
            coordinates,
            segments,
            within,
            rings,
        }
    }

    /// The largest of both in every part, which bounds what either bounds.
    pub(super) fn largest(self, other: Self) -> Self {
        Self {
            coordinates: self.coordinates.max(other.coordinates),
            segments: self.segments.max(other.segments),
            within: self.within.max(other.within),
            rings: self.rings && other.rings,
        }
    }
}

/// An edge of a geometry's graph: a line string's coordinates, or a line's, box's or
/// triangle's (its segments' count of them, and whether it is a ring).
#[derive(Clone, Copy)]
enum Edge<'a> {
    Coords(&'a [geo::Coord<f64>]),
    Fixed([geo::Coord<f64>; 5], usize, bool),
}

impl Edge<'_> {
    fn segments(&self) -> usize {
        match self {
            Edge::Coords(c) => c.len().saturating_sub(1),
            Edge::Fixed(_, n, _) => *n,
        }
    }

    fn segment(&self, i: usize) -> geo::Line<f64> {
        match self {
            Edge::Coords(c) => geo::Line::new(c[i], c[i + 1]),
            Edge::Fixed(c, _, _) => geo::Line::new(c[i], c[i + 1]),
        }
    }

    fn closed(&self) -> bool {
        match self {
            Edge::Coords(c) => c.len() > 1 && c.first() == c.last(),
            Edge::Fixed(_, _, ring) => *ring,
        }
    }
}

/// Whether geo treats `g`'s edges as rings, not intersecting them within themselves.
fn rings(g: &Geometry<f64>) -> bool {
    match g {
        Geometry::LineString(l) => l.is_closed(),
        Geometry::MultiLineString(m) => m.is_closed(),
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => true,
        _ => false,
    }
}

/// `f` for each edge of `g`'s graph.
fn edges<'a>(g: &'a Geometry<f64>, f: &mut impl FnMut(Edge<'a>)) {
    let polygon = |p: &'a geo::Polygon<f64>, f: &mut dyn FnMut(Edge<'a>)| {
        f(Edge::Coords(&p.exterior().0));
        for ring in p.interiors() {
            f(Edge::Coords(&ring.0));
        }
    };
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => {}
        Geometry::Line(l) => {
            f(Edge::Fixed([l.start, l.end, l.end, l.end, l.end], 1, false));
        }
        Geometry::LineString(l) => f(Edge::Coords(&l.0)),
        Geometry::MultiLineString(m) => m.0.iter().for_each(|l| f(Edge::Coords(&l.0))),
        Geometry::Polygon(p) => polygon(p, f),
        Geometry::MultiPolygon(m) => m.0.iter().for_each(|p| polygon(p, f)),
        Geometry::Rect(r) => {
            let (lo, hi) = (r.min(), r.max());
            let corner = |x, y| geo::Coord { x, y };
            let ring = [lo, corner(hi.x, lo.y), hi, corner(lo.x, hi.y), lo];
            f(Edge::Fixed(ring, 4, true));
        }
        Geometry::Triangle(t) => f(Edge::Fixed(
            [t.v1(), t.v2(), t.v3(), t.v1(), t.v1()],
            3,
            true,
        )),
        Geometry::GeometryCollection(c) => c.0.iter().for_each(|g| edges(g, f)),
    }
}

/// A segment in the sweep: its box, which geometry and edge it is of, and its place.
#[derive(Clone, Copy)]
struct Swept {
    bbox: [f64; 4],
    line: geo::Line<f64>,
    of_b: bool,
    edge: u32,
    index: u32,
    /// The edge's last segment if the edge is closed (its first and last are adjacent).
    last: Option<u32>,
}

/// The intersections geo's `relate` records for `a` and `b` (on both edges, a pair within
/// one geometry in both orders): their segments swept by the least `x` of their boxes,
/// each pair whose boxes meet tested as geo tests it.
fn recorded(
    a: &Geometry<f64>,
    b: &Geometry<f64>,
    sa: &Size,
    sb: &Size,
    scratch: &Budget,
) -> Result<usize, BudgetExceeded> {
    use geo::line_intersection::{LineIntersection, line_intersection};
    let mut swept: ChargedVec<Swept, &Budget> =
        ChargedVec::with_capacity(scratch, sa.segments + sb.segments)?;
    let mut edge = 0;
    for (g, of_b) in [(a, false), (b, true)] {
        edges(g, &mut |e| {
            let n = e.segments();
            let last = e.closed().then(|| n.saturating_sub(1) as u32);
            for i in 0..n {
                let line = e.segment(i);
                let (p, q) = (line.start, line.end);
                let bbox = [p.x.min(q.x), p.y.min(q.y), p.x.max(q.x), p.y.max(q.y)];
                let segment = Swept {
                    bbox,
                    line,
                    of_b,
                    edge,
                    index: i as u32,
                    last,
                };
                // Room for every segment was charged: this doesn't grow.
                swept.push(segment).expect("reserved");
            }
            edge += 1;
        });
    }
    swept.sort_unstable_by(|x, y| x.bbox[0].total_cmp(&y.bbox[0]));
    let mut recorded = 0usize;
    for (i, x) in swept.iter().enumerate() {
        for y in swept[i + 1..].iter().take_while(|y| y.bbox[0] <= x.bbox[2]) {
            if y.bbox[1] > x.bbox[3] || x.bbox[1] > y.bbox[3] {
                continue;
            }
            let same = x.edge == y.edge;
            if x.of_b == y.of_b && same && [sa, sb][usize::from(x.of_b)].rings {
                continue;
            }
            let Some(meeting) = line_intersection(x.line, y.line) else {
                continue;
            };
            let points = match meeting {
                LineIntersection::SinglePoint { is_proper, .. } => {
                    if x.of_b != y.of_b && is_proper {
                        continue;
                    }
                    // Adjacent segments of one edge share a point, which isn't recorded.
                    let ends = |p: u32, q: u32| {
                        p.abs_diff(q) == 1
                            || x.last.is_some_and(|l| (p, q) == (0, l) || (q, p) == (0, l))
                    };
                    if same && x.of_b == y.of_b && ends(x.index, y.index) {
                        continue;
                    }
                    1
                }
                LineIntersection::Collinear { .. } => 2,
            };
            let orders = if x.of_b == y.of_b { 2 } else { 1 };
            recorded = recorded.saturating_add(2 * orders * points);
        }
    }
    Ok(recorded)
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
            match a.units() {
                Units::Geographic => match planar_factor(unit)? {
                    None => double(geodesic_distance(&a.geometry, &b.geometry)?),
                    Some(per_degree) => {
                        double(Euclidean.distance(&a.geometry, &b.geometry) / per_degree)
                    }
                },
                Units::Metres if metres(unit) => {
                    double(Euclidean.distance(&a.geometry, &b.geometry))
                }
                Units::Metres | Units::Unknown => None,
            }
        }
        "area" => {
            let a = shape(0)?;
            let unit = unit(args.get(1)?)?;
            let square_metres = matches!(unit, "metre" | "meter" | "squareMetre" | "squareMeter");
            match a.units() {
                Units::Geographic if square_metres => double(a.geometry.geodesic_area_unsigned()),
                Units::Metres if square_metres => double(a.geometry.unsigned_area()),
                _ => None,
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
            match (a.units(), planar_factor(unit)) {
                (Units::Geographic, Some(None)) => double(lines(&|l| Geodesic.length(l))),
                (Units::Geographic, Some(Some(per_degree))) => {
                    double(lines(&|l| Euclidean.length(l)) / per_degree)
                }
                (Units::Metres, _) if metres(unit) => double(lines(&|l| Euclidean.length(l))),
                _ => None,
            }
        }
        "buffer" => {
            let a = shape(0)?;
            let radius = match args.get(1)? {
                Term::Literal(l) => l.value().parse::<f64>().ok()?,
                _ => return None,
            };
            let unit = unit(args.get(2)?)?;
            let buffered = match (a.units(), planar_factor(unit)?) {
                // In CRS84, metres: in a plane about the geometry.
                (Units::Geographic, None) => metre_buffer(&a.geometry, radius)?,
                // In CRS84, degrees and radians: planar.
                (Units::Geographic, Some(factor)) => a.geometry.buffer(radius * factor),
                (Units::Metres, None) => a.geometry.buffer(radius),
                (Units::Metres, Some(_)) | (Units::Unknown, _) => return None,
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

#[cfg(test)]
mod tests {
    use geo::{Coord, LineString, MultiPolygon, Polygon};
    use nrese_exec::measure::{alone, measure};

    use super::*;

    fn line(points: impl Iterator<Item = (f64, f64)>) -> LineString<f64> {
        LineString(points.map(|(x, y)| Coord { x, y }).collect())
    }

    fn ring(n: usize, cx: f64, r: f64) -> LineString<f64> {
        let mut l = line((0..n).map(|i| {
            let t = i as f64 / n as f64 * std::f64::consts::TAU;
            (cx + r * t.cos(), r * t.sin())
        }));
        l.0.push(l.0[0]);
        l
    }

    fn zigzag(n: usize, up: bool) -> LineString<f64> {
        line((0..n).map(|i| match up {
            true => (i as f64, (i % 2) as f64 * n as f64),
            false => ((i % 2) as f64 * n as f64, i as f64 + 0.5),
        }))
    }

    /// geo's `relate` allocates no more than its bound, over the cases that drive each of
    /// its terms: equal and overlapping lines and rings (intersections at every vertex),
    /// crossing zigzags (crossings it doesn't record), a line crossing itself and the
    /// crossing rings of one polygon (crossings it records, a million at 1,000 vertices),
    /// holes, points and boxes.
    #[test]
    fn relate_allocates_no_more_than_its_bound() {
        if !alone() {
            return;
        }
        let poly = |l: LineString<f64>| Geometry::Polygon(Polygon::new(l, vec![]));
        let mut failures = Vec::new();
        for n in [4usize, 10, 100, 1_000] {
            let mut crossing = zigzag(n, true);
            crossing.0.extend(zigzag(n, false).0);
            let closed = |mut l: LineString<f64>| {
                l.0.push(l.0[0]);
                Polygon::new(l, vec![])
            };
            let rings = MultiPolygon(vec![closed(zigzag(n, true)), closed(zigzag(n, false))]);
            let mut cases: Vec<(&str, Geometry<f64>, Geometry<f64>)> = vec![
                (
                    "equal lines",
                    line((0..n).map(|i| (i as f64, 0.0))).into(),
                    line((0..n).map(|i| (i as f64, 0.0))).into(),
                ),
                (
                    "back and forth",
                    line((0..n).map(|i| ((i % 2) as f64, 0.0))).into(),
                    line((0..n).map(|i| ((i % 2) as f64, 1.0 - (i % 2) as f64))).into(),
                ),
                (
                    "crossing zigzags",
                    zigzag(n, true).into(),
                    zigzag(n, false).into(),
                ),
                (
                    "offset rings",
                    poly(ring(n, 0.0, 10.0)),
                    poly(ring(n, 5.0, 10.0)),
                ),
                (
                    "equal rings",
                    poly(ring(n, 0.0, 10.0)),
                    poly(ring(n, 0.0, 10.0)),
                ),
                (
                    "holes",
                    Polygon::new(
                        ring(n, 0.0, 10.0),
                        (0..4).map(|k| ring(n, k as f64 - 1.5, 0.4)).collect(),
                    )
                    .into(),
                    poly(ring(n, 0.5, 10.0)),
                ),
                (
                    "point in ring",
                    Point::new(1.0, 1.0).into(),
                    poly(ring(n, 0.0, 10.0)),
                ),
                (
                    "points in ring",
                    Geometry::MultiPoint(
                        (0..n)
                            .map(|i| Point::new(i as f64 / n as f64, 0.5))
                            .collect(),
                    ),
                    poly(ring(n, 0.0, 10.0)),
                ),
                (
                    "box over ring",
                    geo::Rect::new((-5.0, -5.0), (5.0, 5.0)).into(),
                    poly(ring(n, 0.0, 5.0)),
                ),
            ];
            if n <= 100 {
                cases.push((
                    "a line crossing itself",
                    crossing.into(),
                    poly(ring(8, 0.0, 3.0)),
                ));
                cases.push(("crossing rings", rings.into(), poly(ring(8, 0.0, 3.0))));
            }
            for (case, a, b) in cases {
                let bound = relate_bound(
                    (&a, &Size::of(&a)),
                    (&b, &Size::of(&b)),
                    &Budget::unlimited(),
                )
                .unwrap();
                let (_, held) = measure(|| a.relate(&b).is_intersects());
                eprintln!("{case} at {n}: held {held} of {bound}");
                if held > bound {
                    failures.push(format!("{case} at {n}: {held} > {bound}"));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
