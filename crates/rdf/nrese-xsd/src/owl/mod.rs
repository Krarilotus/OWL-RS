//! The OWL 2 datatype map (W3C *OWL 2 Structural Specification*, §4): its datatypes and
//! facets, the lexical-to-value mapping of literals onto data values with OWL 2's identity,
//! and the value-space operations a datatype theory decides data ranges with (sets of
//! values closed under union, intersection and complement, which count and enumerate).
//!
//! Numbers are exact (`Rational`): two literals are one value iff their values are equal,
//! never by rounding. A literal whose value can't be represented is an error, never a
//! nearby value.

mod datatype;
mod line;
mod rational;
mod regular;
mod set;
pub mod text;
mod texts;
mod value;
mod xml;

pub use datatype::{Datatype, Facet};
pub use line::{Cut, Line, Segment};
pub use rational::{NumberError, Rational};
pub use regular::PatternError;
pub use set::{Count, ValueSet, facet};
pub use texts::MAX_PATTERNS;
pub use value::{LiteralError, Value};
