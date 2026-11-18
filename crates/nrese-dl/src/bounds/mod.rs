//! The upper bound U1 (docs/design/owl2-dl.md §8): a TBox-compiled datalog
//! over-approximation of the clauses, for the store's bounds (package 3.2).
//!
//! - [`upper`]: U1 from `nrese-owl`'s DL-clauses, PAGOdA's datalog strengthening (Zhou et
//!   al., JAIR 2015, §5.1).
//! - [`program`]: what U1 is: rules over triple patterns with provenance.
//! - [`gap`]: the gap between the lower bound L (the OWL 2 RL closure) and U1, per
//!   predicate and per atomic query: what is left for the DL engine.
//! - [`telemetry`]: times and sizes of a run.
//!
//! **The representation.** U1 is a [`Program`] of rules over triple patterns, the shape of
//! `nrese-reasoner`'s rule IR (a body of `(s p o)` atoms over variables and term ids, a
//! head of atoms), with the same term ids as the ontology was read with. So:
//! - `nrese-dl` needs no reasoner: it depends on `nrese-owl` only, and the reasoner can
//!   later depend on `nrese-dl` without a cycle;
//! - the store hands it to its engine by copying atoms one to one, with no text in
//!   between, keeping every rule's [`Provenance`] (the clause and source axioms, and which
//!   approximation made it) beside it by index;
//! - the rule text syntax of `nrese-reasoner` (`ir::parse_rules`) can't name IRIs outside
//!   `rdf:`, `rdfs:`, `owl:` and `xsd:`; Notation3 can, and [`Program::to_n3`] writes it for
//!   the store's user-rule path and for reading.
//!
//! U1's vocabulary is the ontology's: classes as `rdf:type`, properties as themselves,
//! equality as `owl:sameAs`. So L and U1 compare triple by triple, and U1 can be evaluated
//! over L ∪ data (the query path of §8) or over the data alone.

mod clauses;
pub mod gap;
pub mod program;
mod project;
pub mod telemetry;
pub mod upper;

pub use gap::{Answer, AtomicQuery, Bounds, GapReport, PredicateGap};
pub use program::{
    Approximations, Atom, Names, Origin, Program, Provenance, RenderError, Rule, Signature, Slot,
};
pub use telemetry::{ProgramSize, Telemetry};
pub use upper::{Options, compile, compile_with};
