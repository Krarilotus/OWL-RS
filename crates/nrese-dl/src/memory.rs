//! The request's memory envelope in the DL engines ([`nrese_exec::Envelope`]). The engines
//! reserve their capacity estimates from it as they grow: context saturation through its
//! observer (`context::Budget::memory`), the hypertableau at its memory checks
//! (`tableau::Config::memory`). Compiling reserves its scratch before it is made where
//! the size is known from the input (a premise's copy), and as each stage's output exists
//! (the normalised clauses, the compiled program): [`Scratch`].
//!
//! The sizes are estimates by capacity and length, like the context core's: hash tables
//! by their load factor, the nested lists of axioms, clauses and sources counted, an
//! expression's own lists not. `tests/scratch_memory.rs` holds them against the
//! allocator.

use std::mem::size_of;

use nrese_exec::{BudgetExceeded, Envelope, Reservation};
use nrese_owl::{Axiom, Normalised, Ontology};

/// Why a DL task stopped when the request's envelope had no room for it: as
/// [`crate::limits::Limit::Envelope`] reports it.
pub const EXHAUSTED: &str =
    "the request's memory envelope (budgets.query_memory) reached (no room for what the run needs)";

/// Compilation scratch held against a request's envelope: released when dropped.
#[derive(Debug, Default)]
pub struct Scratch(Option<Reservation<Envelope>>);

impl Scratch {
    /// Scratch drawing from `envelope`; with none, or one that can't refuse, nothing is
    /// counted (and no size estimated).
    pub fn new(envelope: Option<&Envelope>) -> Self {
        Self(
            envelope
                .and_then(Envelope::bounded)
                .map(|envelope| Reservation::new(envelope.clone())),
        )
    }

    /// Reserves the `bytes` it estimates more, or fails without reserving them; estimates
    /// only where something is counted.
    pub fn charge(&mut self, bytes: impl FnOnce() -> usize) -> Result<(), BudgetExceeded> {
        self.0.as_mut().map_or(Ok(()), |held| held.charge(bytes()))
    }

    /// Returns up to `bytes` of what this holds (a stage's output dropped).
    pub fn release(&mut self, bytes: usize) {
        if let Some(held) = &mut self.0 {
            held.release(bytes);
        }
    }

    pub fn bytes(&self) -> usize {
        self.0.as_ref().map_or(0, Reservation::bytes)
    }
}

/// A hash table of `len` entries of `entry` bytes: buckets for a 7/8 load factor, rounded
/// up to a power of two, and a control byte each.
pub(crate) fn table(len: usize, entry: usize) -> usize {
    if len == 0 {
        return 0;
    }
    len.saturating_mul(8)
        .div_ceil(7)
        .next_power_of_two()
        .saturating_mul(entry + 1)
}

fn vec<T>(v: &[T], capacity: usize) -> usize {
    capacity.max(v.len()) * size_of::<T>()
}

/// An interner of `len` values: each value in its list and again as its index's key.
fn interner<T>(len: usize) -> usize {
    len * size_of::<T>() + table(len, size_of::<T>() + size_of::<u32>())
}

/// What a copy of `ontology` holds. O(axioms + sources).
pub fn ontology_bytes(ontology: &Ontology) -> usize {
    let axioms = vec(&ontology.axioms, ontology.axioms.capacity())
        + ontology.axioms.iter().map(axiom_lists).sum::<usize>();
    let sources = vec(&ontology.sources, ontology.sources.capacity())
        + ontology
            .sources
            .iter()
            .map(|sources| {
                vec(sources, sources.capacity())
                    + sources
                        .iter()
                        .map(|s| vec(&s.triples, s.triples.capacity()))
                        .sum::<usize>()
            })
            .sum::<usize>();
    let data = &ontology.data;
    let literals = table(
        data.literals.len(),
        size_of::<(nrese_owl::Term, nrese_owl::Literal)>(),
    ) + data
        .literals
        .values()
        .map(|l| {
            l.lexical.capacity()
                + l.datatype.as_ref().map_or(0, String::capacity)
                + l.language.as_ref().map_or(0, String::capacity)
        })
        .sum::<usize>();
    let iris = table(data.iris.len(), size_of::<(nrese_owl::Term, String)>())
        + data.iris.values().map(String::capacity).sum::<usize>();
    axioms
        + sources
        + interner::<nrese_owl::ClassExpr>(ontology.classes.len())
        + interner::<nrese_owl::DataRange>(ontology.ranges.len())
        + vec(&ontology.diagnostics, ontology.diagnostics.capacity())
        + literals
        + iris
        + ontology.anonymous.len() * (size_of::<nrese_owl::Term>() + 8)
}

/// The lists an axiom holds besides itself.
fn axiom_lists(axiom: &Axiom) -> usize {
    match axiom {
        Axiom::EquivalentClasses(v) | Axiom::DisjointClasses(v) | Axiom::DisjointUnion(_, v) => {
            vec(v, v.capacity())
        }
        Axiom::SubObjectPropertyOf(v, _)
        | Axiom::EquivalentObjectProperties(v)
        | Axiom::DisjointObjectProperties(v) => vec(v, v.capacity()),
        Axiom::EquivalentDataProperties(v)
        | Axiom::DisjointDataProperties(v)
        | Axiom::SameIndividual(v)
        | Axiom::DifferentIndividuals(v) => vec(v, v.capacity()),
        Axiom::HasKey(_, objects, data) => {
            vec(objects, objects.capacity()) + vec(data, data.capacity())
        }
        _ => 0,
    }
}

/// What `normalised` holds. O(clauses).
pub fn normalised_bytes(normalised: &Normalised) -> usize {
    let clauses = vec(&normalised.clauses, normalised.clauses.capacity())
        + normalised
            .clauses
            .iter()
            .map(|c| {
                vec(&c.body, c.body.capacity())
                    + vec(&c.head, c.head.capacity())
                    + vec(&c.sources, c.sources.capacity())
                    + c.sources
                        .iter()
                        .map(|s| vec(s, s.capacity()))
                        .sum::<usize>()
            })
            .sum::<usize>();
    let f = &normalised.facts;
    let facts = vec(&f.concepts, f.concepts.capacity())
        + vec(&f.roles, f.roles.capacity())
        + vec(&f.data, f.data.capacity())
        + vec(&f.same, f.same.capacity())
        + vec(&f.different, f.different.capacity())
        + vec(&f.not_roles, f.not_roles.capacity())
        + vec(&f.not_data, f.not_data.capacity());
    clauses
        + facts
        + vec(&normalised.fresh, normalised.fresh.capacity())
        + vec(&normalised.unsupported, normalised.unsupported.capacity())
        + interner::<nrese_owl::ClassExpr>(normalised.classes.len())
        + interner::<nrese_owl::DataRange>(normalised.ranges.len())
        + vec(&normalised.rules, normalised.rules.capacity())
        + vec(&normalised.definitions, normalised.definitions.capacity())
}
