//! The topological relations of GeoSPARQL (Simple Features, Egenhofer, RCC8), by their
//! local names (functions in `geof:`, properties in `geo:`), and what bounding boxes
//! decide about them.
//!
//! [`relation`] is the one definition: the filter functions, the relations in triple
//! patterns and the join ([`crate::join`]) all evaluate it, so they agree. It decides
//! through `geo`'s DE-9IM matrix, which starts from the same box test as
//! [`decided_by_boxes`]: two geometries whose boxes don't meet are disjoint.

use geo::{Geometry, HasDimensions, Rect, Relate};

use crate::shape::Shape;

/// The topological relations, by their local names.
pub const RELATIONS: &[&str] = &[
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
pub fn holds_apart(local: &str) -> bool {
    matches!(local, "sfDisjoint" | "ehDisjoint" | "rcc8dc")
}

/// What a relation between `a` and `b` requires of their bounding boxes, beyond meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boxes {
    /// They meet.
    Meet,
    /// `a`'s lies in `b`'s (`a` is part of `b`).
    Inside,
    /// `b`'s lies in `a`'s.
    Contains,
    /// They are the same box (each lies in the other).
    Equal,
}

/// What `local` requires of the boxes of two nonempty geometries it holds between;
/// `None` for the disjointness relations, which hold between boxes that don't meet too.
///
/// A relation that makes `a` part of `b` (within, inside, covered by, a proper part)
/// puts every vertex of `a` in `b`, whose box therefore holds `a`'s; equality is part in
/// both directions.
pub fn boxes(local: &str) -> Option<Boxes> {
    if holds_apart(local) {
        return None;
    }
    Some(match local {
        "sfEquals" | "ehEquals" | "rcc8eq" => Boxes::Equal,
        "sfWithin" | "ehInside" | "ehCoveredBy" | "rcc8tpp" | "rcc8ntpp" => Boxes::Inside,
        "sfContains" | "ehContains" | "ehCovers" | "rcc8tppi" | "rcc8ntppi" => Boxes::Contains,
        _ => Boxes::Meet,
    })
}

fn holds(outer: &Rect<f64>, inner: &Rect<f64>) -> bool {
    outer.min().x <= inner.min().x
        && outer.min().y <= inner.min().y
        && inner.max().x <= outer.max().x
        && inner.max().y <= outer.max().y
}

fn meet(a: &Rect<f64>, b: &Rect<f64>) -> bool {
    a.min().x <= b.max().x
        && b.min().x <= a.max().x
        && a.min().y <= b.max().y
        && b.min().y <= a.max().y
}

/// Whether two boxes pass what `required` asks of them.
pub fn boxes_pass(required: Boxes, a: &Rect<f64>, b: &Rect<f64>) -> bool {
    match required {
        Boxes::Meet => meet(a, b),
        Boxes::Inside => holds(b, a),
        Boxes::Contains => holds(a, b),
        Boxes::Equal => holds(a, b) && holds(b, a),
    }
}

/// Whether `local` holds between two geometries, from their bounding boxes alone (an
/// empty geometry has none): `Some` when the boxes decide it, `None` when the geometries
/// must be compared.
///
/// Geometries whose boxes don't meet are disjoint (`sfDisjoint`, `ehDisjoint`), but not
/// always in RCC8's sense: `rcc8dc` asks for boundaries a point lacks.
pub fn decided_by_boxes(local: &str, a: Option<&Rect<f64>>, b: Option<&Rect<f64>>) -> Option<bool> {
    let (a, b) = (a?, b?);
    match boxes(local) {
        None if !meet(a, b) && local != "rcc8dc" => Some(true),
        None => None,
        Some(required) => (!boxes_pass(required, a, b)).then_some(false),
    }
}

/// Whether relation `local` holds between `a` and `b`; `None` for an unknown name.
pub fn relation(local: &str, a: &Geometry<f64>, b: &Geometry<f64>) -> Option<bool> {
    relation_of(local, a, b, a.is_empty() && b.is_empty())
}

/// [`relation`] for anything `geo` relates (a prepared geometry among them), whose
/// emptiness the caller knows.
pub fn relation_of(
    local: &str,
    a: &impl Relate<f64>,
    b: &impl Relate<f64>,
    both_empty: bool,
) -> Option<bool> {
    // Two empty geometries are equal (their DE-9IM matrix is all F, which no equality
    // pattern matches).
    if both_empty && matches!(local, "sfEquals" | "ehEquals" | "rcc8eq") {
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

/// Whether `local` holds between two shapes: `None` (an error) for shapes in different
/// reference systems, which aren't compared, or an unknown name.
pub fn relate(local: &str, a: &Shape, b: &Shape) -> Option<bool> {
    if a.crs != b.crs {
        return None;
    }
    relation(local, &a.geometry, &b.geometry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{WKT_LITERAL, parse};

    fn shape(text: &str) -> Shape {
        parse(WKT_LITERAL, text).unwrap()
    }

    /// Whatever the boxes decide, the geometries agree with: over pairs of shapes that
    /// touch, nest, overlap, are equal written differently or lie apart, for every
    /// relation. The guard of the box decisions the index and the join rely on.
    #[test]
    fn boxes_never_decide_against_the_geometries() {
        let shapes = [
            "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))",
            "POLYGON((0 0, 0 10, 10 10, 10 0, 5 0, 0 0))",
            "POLYGON((2 2, 4 2, 4 4, 2 4, 2 2))",
            "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))",
            "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))",
            "POLYGON((50 50, 60 50, 60 60, 50 60, 50 50))",
            "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0),(2 2, 4 2, 4 4, 2 4, 2 2))",
            "POINT(5 5)",
            "POINT(0 0)",
            "POINT(10 5)",
            "MULTIPOINT((1 1),(30 30))",
            "LINESTRING(-5 5, 15 5)",
            "LINESTRING(0 0, 10 10)",
            "LINESTRING(2 2, 4 4)",
            "GEOMETRYCOLLECTION(POINT(1 1),LINESTRING(3 3, 4 4))",
            "",
            "POINT EMPTY",
        ]
        .map(shape);
        let mut decided = 0;
        for local in RELATIONS {
            for a in &shapes {
                for b in &shapes {
                    let exact = relate(local, a, b);
                    if let Some(by_boxes) =
                        decided_by_boxes(local, a.rect().as_ref(), b.rect().as_ref())
                    {
                        decided += 1;
                        assert_eq!(
                            Some(by_boxes),
                            exact,
                            "{local}({:?}, {:?})",
                            a.geometry,
                            b.geometry
                        );
                    }
                }
            }
        }
        // Over 40 % of the pairs are decided by their boxes (2,978 of 6,936).
        assert!(decided > 2900, "{decided}");
    }
}
