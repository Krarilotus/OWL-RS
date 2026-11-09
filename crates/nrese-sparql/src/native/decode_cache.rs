//! The stored terms a query decodes once and keeps for reuse ([`DecodeCache`]): its context's
//! ([`super::Context::term`]) and each worker's while it filters ([`super::Decoder`]). What a
//! cache keeps is charged to the query's budget while it is kept (D16-039): its table before it
//! grows ([`ChargedMap`]) and each kept term's text ([`Grant`], [`Term::heap_bytes`]).
//!
//! A cache is never needed for an answer, so it gives way to what the query must hold:
//! - It keeps a term only where, kept, it would hold no more than the room it leaves free
//!   (sol-loop r1 S2), so what the query must hold later finds at least as much room as the
//!   cache takes. A term that doesn't fit so is decoded again as it is needed.
//! - At each term it is offered, where it holds more than the room left (what the query holds
//!   grew since), it lets go of everything it keeps and starts afresh (sol-loop r2 S2).
//!   Residual: a required charge made while only cached terms are read, with no term decoded
//!   in between, still meets the cache as it was; it holds at most what it left free when it
//!   last kept a term.
//! - Where the budget refuses an entry anyway, the cache lets go of everything it keeps, with
//!   its charge, and keeps nothing more for the rest of its life.
//!
//! Either way the query decodes the same terms; only how often changes.

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
    /// What both draw from, for the room it leaves free.
    budget: Arc<Budget>,
    /// How many caches fill theirs at once ([`DecodeCache::new`]).
    workers: usize,
}

impl Kept {
    fn new(budget: &Arc<Budget>, workers: usize) -> Self {
        Self {
            terms: ChargedMap::new(Arc::clone(budget)),
            text: Grant::with_workers(Arc::clone(budget), workers),
            budget: Arc::clone(budget),
            workers,
        }
    }
}

impl DecodeCache {
    /// An empty cache of at most `entries` terms charged to `budget`, filled by one thread at a
    /// time with `workers` like it filling theirs at once (the share of the budget's spare each
    /// grant holds, [`Grant::with_workers`]).
    pub(super) fn new(budget: &Arc<Budget>, entries: usize, workers: usize) -> Self {
        Self {
            kept: Some(Kept::new(budget, workers)),
            entries,
        }
    }

    /// The term kept for `id`, if one is.
    pub(super) fn get(&self, id: u64) -> Option<&Option<Term>> {
        self.kept.as_ref()?.terms.get(&id)
    }

    /// Keeps `term` for `id` where there is room: within `entries`, holding no more than it
    /// leaves free, its text and the table's growth charged first. Refused, the cache lets go
    /// of everything and keeps nothing more.
    pub(super) fn keep(&mut self, id: u64, term: &Option<Term>) {
        let Some(kept) = &mut self.kept else {
            return;
        };
        if kept.terms.len() >= self.entries {
            return;
        }
        let text = term.as_ref().map_or(0, Term::heap_bytes);
        let held = kept.terms.charged() + kept.text.bytes();
        let remaining = kept.budget.remaining();
        // What the query must hold grew since the cache last kept a term: where the cache now
        // holds more than the room left, it lets everything go, so that room is the query's
        // again (sol-loop r2 S2), and starts afresh.
        if held > remaining {
            let (budget, workers) = (Arc::clone(&kept.budget), kept.workers);
            // The old terms and their charge go before anything new is charged.
            self.kept = None;
            self.kept = Some(Kept::new(&budget, workers));
            return;
        }
        // Kept, the cache would hold `held + text` and leave `remaining - text` free.
        let fits = held
            .checked_add(text.saturating_mul(2))
            .is_some_and(|needed| needed <= remaining);
        if !fits {
            return;
        }
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

    /// 100 terms of 4 KiB offered to a 64 KiB budget: the cache keeps those that leave at
    /// least as much free as it holds, never half the budget or more, and is not refused.
    #[test]
    fn a_cache_holds_no_more_than_it_leaves_free() {
        let limit = 64 << 10;
        let budget = Arc::new(Budget::new(limit));
        let mut cache = DecodeCache::new(&budget, 1 << 10, 1);
        for i in 0..100 {
            cache.keep(i as u64, &literal(i, 4096));
        }
        assert!(cache.keeps(), "it gave way rather than being refused");
        assert!(cache.get(0).is_some(), "the first terms are kept");
        assert!(cache.get(99).is_none(), "the last don't fit so");
        let kept = cache.kept.as_ref().expect("keeps");
        let held = kept.terms.charged() + kept.text.bytes();
        assert!(held <= limit / 2, "it holds {held} bytes of {limit}");
        drop(cache);
        assert_eq!(budget.used(), 0, "dropped, nothing stays charged");
    }

    /// What the query holds grows after a term is kept, until the cache holds more than the
    /// room left: at the next term it is offered, it lets go of everything and starts afresh.
    #[test]
    fn a_cache_lets_go_once_the_query_needs_its_room() {
        let limit = 64 << 10;
        let budget = Arc::new(Budget::new(limit));
        let mut cache = DecodeCache::new(&budget, 1 << 10, 1);
        cache.keep(0, &literal(0, 4096));
        assert!(cache.get(0).is_some(), "kept while there is room");
        // The query's own charge leaves 1 KiB, less than the cache holds.
        let mut answer = nrese_exec::Reservation::new(Arc::clone(&budget));
        answer.charge(budget.remaining() - 1024).unwrap();
        cache.keep(1, &literal(1, 16));
        assert_eq!(cache.get(0), None, "the kept term is let go");
        assert!(cache.keeps(), "and the cache starts afresh, not refused");
        assert_eq!(
            budget.used(),
            answer.bytes(),
            "only the query's own charge is left"
        );
    }

    /// Where the budget refuses what the cache would grow (its table, here: nothing decoded
    /// has text), it lets go of everything and keeps nothing more.
    #[test]
    fn a_refused_cache_empties_and_stops() {
        let budget = Arc::new(Budget::new(64));
        let mut cache = DecodeCache::new(&budget, 1 << 10, 1);
        cache.keep(0, &None);
        assert!(!cache.keeps(), "refused, the cache stops");
        assert_eq!(cache.get(0), None, "and keeps nothing");
        assert_eq!(budget.used(), 0, "its charge is gone");
        cache.keep(1, &literal(0, 16));
        assert_eq!(cache.get(1), None, "nothing more is kept");
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
