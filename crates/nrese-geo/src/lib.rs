//! Geometries (GeoSPARQL 1.1): the geometry literals read into shapes ([`parse`]: WKT,
//! GeoJSON, GML), the topological relations between them ([`relation`]), what bounding
//! boxes decide about a relation ([`decided_by_boxes`]), the index of a store's geometry
//! literals that finds those a box meets ([`GeometryIndex`]), and the join that finds the
//! pairs of two sets a relation holds for ([`join()`]).
//!
//! Nothing here knows terms, snapshots or queries: the engine keeps the geometry literals
//! of its dictionary in a [`GeometryIndex`] by term id, and the query engine evaluates
//! GeoSPARQL's functions and decides which index or join to use.

pub mod formats;
mod index;
mod join;
mod relation;
mod shape;

pub use index::{Entry, GeometryIndex};
pub use join::{Item, JoinError, JoinStats, Side, join};
pub use relation::{
    Boxes, RELATIONS, boxes, boxes_pass, decided_by_boxes, holds_apart, relate, relation,
    relation_of,
};
pub use shape::{
    CRS84, DATATYPES, GEO, GEOJSON_LITERAL, GML_LITERAL, Serialisation, Shape, WKT_LITERAL,
    geometry_heap_bytes, parse, parses_on_this_thread,
};
