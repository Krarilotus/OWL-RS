//! XML Schema 1.1 datatypes with the XPath 3.1 operations SPARQL uses: numbers, booleans,
//! dates, times, durations; and the OWL 2 datatype map's value spaces ([`owl`]).
//!
//! Part of the RDF bundle (`crates/rdf`), replacing `oxsdatatypes`, and independent of RDF.
//! Every value is `Copy`. Parsing accepts exactly the XSD lexical forms. `Display` writes
//! what XPath's cast to `xs:string` gives (what SPARQL's `STR()` returns), and
//! `canonical()` writes the XSD canonical form where the two differ. Operations that can
//! fail (overflow, division by zero) are `checked_*` and return `None`.
//!
//! Limits (XSD allows implementation limits above a minimum; these are above it):
//! `Integer` is 64-bit; `Decimal` is 128-bit fixed point with 18 fractional digits
//! (±1.7 × 10²⁰); dates reach years ±10¹¹; fractional seconds keep 18 digits, and longer
//! fractions are truncated.

#![forbid(unsafe_code)]

mod boolean;
mod date_time;
mod decimal;
mod duration;
mod error;
mod floating;
mod integer;
pub mod owl;

pub use boolean::Boolean;
pub use date_time::{
    Date, DateTime, GDay, GMonth, GMonthDay, GYear, GYearMonth, Time, TimezoneOffset,
};
pub use decimal::Decimal;
pub use duration::{DayTimeDuration, Duration, YearMonthDuration};
pub use error::{ParseError, RangeError};
pub use floating::{Double, Float};
pub use integer::Integer;
