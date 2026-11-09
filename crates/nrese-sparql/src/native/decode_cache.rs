//! The stored terms a query decodes once and keeps for reuse ([`DecodeCache`]): its context's
//! ([`super::Context::term`]) and each worker's while it filters ([`super::Decoder`]). What a
//! cache keeps is charged to the query's budget while it is kept (D16-039): its table before it
//! grows ([`ChargedMap`]) and each kept term's text ([`Grant`], [`Term::heap_bytes`]).
//!
//! A cache is never needed for an answer. Where the budget has no room for an entry, the cache
//! lets go of everything it keeps, with its charge, and keeps nothing more for the rest of its
//! life: the room goes to what the query must hold, and the query decodes each term again as
//! it is needed, the same terms.

use std::sync::Arc;

use nrese_exec::{Budget, ChargedMap, Grant};
use nrese_rdf::Term;

/// Up to `entries` decoded terms by id, `None` for an id that decodes to nothing, charged to a
/// budget while kept.
pub(super) struct DecodeCache {
    /// `None` once a charge was refused: nothing is kept any more.
    kept: Option<Kept>,
    entries: usize,
}

/// What a cache keeps, and its charge.
struct Kept {
    terms: ChargedMap<u64, Option<Term>>,
    /// After the table: fields drop in order, the terms freed before their charge goes.
    text: Grant,
}

impl DecodeCache {
    /// An empty cache of at most `entries` terms charged to `budget`, filled by one thread at a
    /// time with `workers` like it filling theirs at once (the share of the budget's spare each
    /// grant holds, [`Grant::with_workers`]).
    pub(super) fn new(budget: &Arc<Budget>, entries: usize, workers: usize) -> Self {
        Self {
            kept: Some(Kept {
                terms: ChargedMap::new(Arc::clone(budget)),
                text: Grant::with_workers(Arc::clone(budget), workers),
            }),
            entries,
        }
    }

    /// The term kept for `id`, if one is.
    pub(super) fn get(&self, id: u64) -> Option<&Option<Term>> {
        self.kept.as_ref()?.terms.get(&id)
    }

    /// Keeps `term` for `id` where there is room: within `entries`, its text and the table's
    /// growth charged first. Refused, the cache lets go of everything and keeps nothing more.
    pub(super) fn keep(&mut self, id: u64, term: &Option<Term>) {
        let Some(kept) = &mut self.kept else {
            return;
        };
        if kept.terms.len() >= self.entries {
            return;
        }
        let text = term.as_ref().map_or(0, Term::heap_bytes);
        if kept.text.take(text).is_err() {
            self.kept = None;
            return;
        }
        match kept.terms.get_or_insert_with(id, || term.clone()) {
            // Kept already: the text it was charged for is the one kept.
            Ok((_, false)) => kept.text.give_back(text),
            Ok((_, true)) => {}
            Err(_) => self.kept = None,
        }
    }

    /// Whether the cache still keeps terms (no charge of it was refused).
    #[cfg(test)]
    pub(super) fn keeps(&self) -> bool {
        self.kept.is_some()
    }
}

/// A cache charges what it keeps, lets all of it go where the budget is full, and its charge is
/// gone once it is.
#[cfg(test)]
mod tests {
    use super::*;
    use nrese_rdf::Literal;

    fn literal(i: usize, len: usize) -> Option<Term> {
        Some(Literal::new_simple_literal(format!("{i:08}{}", "x".repeat(len - 8))).into())
    }

    #[test]
    fn a_kept_terms_text_is_charged_while_kept() {
        let budget = Arc::new(Budget::unlimited());
        let mut cache = DecodeCache::new(&budget, 1 << 10, 1);
        for i in 0..100 {
            cache.keep(i as u64, &literal(i, 4096));
        }
        assert!(
            budget.used() >= 100 * 4096,
            "100 terms of 4 KiB are charged: {} bytes",
            budget.used()
        );
        assert_eq!(cache.get(7), Some(&literal(7, 4096)), "and kept");
        drop(cache);
        assert_eq!(budget.used(), 0, "dropped, nothing stays charged");
    }

    #[test]
    fn a_full_budget_empties_the_cache_and_stops_it() {
        let limit = 64 << 10;
        let budget = Arc::new(Budget::new(limit));
        let mut cache = DecodeCache::new(&budget, 1 << 10, 1);
        for i in 0..100 {
            cache.keep(i as u64, &literal(i, 4096));
        }
        assert!(!cache.keeps(), "refused, the cache stops");
        assert_eq!(cache.get(0), None, "and keeps nothing");
        assert_eq!(budget.used(), 0, "its charge is gone");
        assert!(budget.peak() <= limit);
        cache.keep(1000, &literal(0, 16));
        assert_eq!(cache.get(1000), None, "nothing more is kept");
    }

    #[test]
    fn at_most_its_entries_are_kept() {
        let budget = Arc::new(Budget::unlimited());
        let mut cache = DecodeCache::new(&budget, 10, 1);
        for i in 0..20 {
            cache.keep(i as u64, &literal(i, 16));
        }
        assert!(cache.get(9).is_some() && cache.get(10).is_none());
        assert!(cache.keeps(), "a full table is not a refusal");
    }
}
