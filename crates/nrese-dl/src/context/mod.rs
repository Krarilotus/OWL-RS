//! The context-saturation core (docs/design/owl2-dl.md §5): one engine whose rule
//! families switch on step by step, Horn first (package 3.1).
//!
//! The calculus is Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and Horrocks,
//! *Consequence-Based Reasoning for Description Logics with Disjunctions and Number
//! Restrictions* (JAIR 2018), restricted to its Horn rules: Core, Hyper, Succ and Pred
//! over Horn DL-clauses, `⊥` propagated by Pred, with the paper's context structure,
//! term order, redundancy elimination and the cautious expansion strategy (eager as a
//! setting).
//!
//! - [`atoms`]: context terms and atoms in one word each, the term order;
//! - [`program`] and [`compile`]: `nrese-owl`'s DL-clauses as the calculus reads them,
//!   made Horn by renaming fresh names where that is possible ([`horn`]), or the reason
//!   the Horn stage gives up ([`Unsupported`]);
//! - [`state`]: a context's clauses, indexes and redundancy elimination;
//! - [`rules`]: the rules on one context, [`links`] those over links between contexts;
//! - [`engine`]: the contexts, created on demand, and their parallel saturation (ELK's
//!   activation scheme on rayon);
//! - [`abox`]: consistency of assertions (no nominals): individuals' contexts and the
//!   clauses on the edges between them, to a fixpoint;
//! - [`classify`]: classification, the taxonomy and proofs of its subsumptions;
//!   [`canonical`]: the canonical taxonomy of the DL lab; [`profile`]: the telemetry.

pub mod abox;
pub mod atoms;
pub mod canonical;
pub mod classify;
pub mod compile;
pub mod engine;
pub mod horn;
pub mod links;
pub mod profile;
pub mod program;
pub mod rules;
mod settrie;
pub mod state;
pub mod unsupported;

pub use classify::{
    Classification, Local, Options, Saturated, classify, saturate, saturate_normalised, signature,
};
pub use compile::Unsupported;
pub use engine::{Budget, Strategy};
pub use profile::Profile;
pub use state::ClauseRef;
