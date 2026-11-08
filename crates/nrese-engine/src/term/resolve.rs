//! What a snapshot resolves its terms through (`notes/2026-11-08-p2-bounded-vocabulary-design.md`
//! §10–§11, the interface the snapshot-views owner accepted): the dictionary, bounded by the
//! snapshot's version, or a private overlay's frozen view ([`OverlayView`]).
//!
//! `Shared` answers exactly as `Snapshot`'s methods did before it, each with the bound they
//! applied. `Private` ignores the bound passed in: its bound is its own (`start + n`), fixed when
//! the overlay opened over a snapshot. A mismatched bound can't reach it in a release build.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "snapshots resolve through it from package 2b")
)]

use std::sync::Arc;

use nrese_rdf::{Quad, QuadRef, Term, TermRef};

use super::dictionary::Dictionary;
use super::overlay::OverlayView;
use super::{StringScan, StringTest, TermId, TermView};
use crate::EncodedQuad;

/// The dictionary (bounded by the caller's version), or an overlay's frozen view.
#[derive(Clone)]
pub(crate) enum Vocabulary {
    Shared(Arc<Dictionary>),
    Private(OverlayView),
}

impl Vocabulary {
    pub(crate) fn is_private(&self) -> bool {
        matches!(self, Self::Private(_))
    }

    /// The contract's b(V) (§4.1): ids below it are the dictionary's, the same in every
    /// vocabulary of that dictionary bounded at or past it. `bound` for shared, `start` for
    /// private.
    pub(crate) fn shared_bound(&self, bound: u64) -> u64 {
        match self {
            Self::Shared(_) => bound,
            Self::Private(view) => view.start(),
        }
    }

    /// Whether ids mean the same terms in both: the same dictionary, and both shared or views
    /// of one overlay. Anything else compares by value (package 5).
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Shared(a), Self::Shared(b)) => Arc::ptr_eq(a, b),
            (Self::Private(a), Self::Private(b)) => a.same_overlay(b),
            _ => false,
        }
    }

    /// The dictionary, for readers of published state only (the checkpoint, statistics'
    /// derived files): `None` for a private vocabulary, whose callers refuse.
    pub(crate) fn dictionary(&self) -> Option<&Arc<Dictionary>> {
        match self {
            Self::Shared(dictionary) => Some(dictionary),
            Self::Private(_) => None,
        }
    }

    pub(crate) fn lookup(&self, term: TermRef<'_>, bound: u64) -> Option<TermId> {
        match self {
            Self::Shared(dictionary) => dictionary.lookup_bounded(term, bound),
            Self::Private(view) => view.lookup(term),
        }
    }

    pub(crate) fn lookup_charged<E>(
        &self,
        term: TermRef<'_>,
        bound: u64,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<TermId>, E> {
        match self {
            Self::Shared(dictionary) => dictionary.lookup_charged(term, bound, charge),
            Self::Private(view) => view.lookup_charged(term, charge),
        }
    }

    pub(crate) fn lookup_quad(&self, quad: QuadRef<'_>, bound: u64) -> Option<EncodedQuad> {
        match self {
            Self::Shared(dictionary) => dictionary.lookup_quad_bounded(quad, bound),
            Self::Private(view) => view.lookup_quad(quad),
        }
    }

    pub(crate) fn triple_term(&self, components: [TermId; 3], bound: u64) -> Option<TermId> {
        match self {
            Self::Shared(dictionary) => dictionary.triple_term_bounded(components, bound),
            Self::Private(view) => view.triple_term(components),
        }
    }

    pub(crate) fn components(&self, id: TermId, bound: u64) -> Option<[TermId; 3]> {
        match self {
            Self::Shared(dictionary) => {
                if id.payload() >= bound {
                    return None;
                }
                dictionary.components(id)
            }
            Self::Private(view) => view.components(id),
        }
    }

    /// Unbounded for shared, as `Snapshot::decode` has been: ids never change meaning.
    pub(crate) fn decode(&self, id: TermId) -> Option<Term> {
        match self {
            Self::Shared(dictionary) => dictionary.decode(id),
            Self::Private(view) => view.decode(id),
        }
    }

    pub(crate) fn decode_charged<E>(
        &self,
        id: TermId,
        bound: u64,
        charge: impl FnOnce(usize) -> Result<(), E>,
    ) -> Result<Option<Term>, E> {
        match self {
            Self::Shared(dictionary) => {
                if id.kind().is_dictionary() && id.payload() >= bound {
                    return Ok(None);
                }
                dictionary.decode_charged(id, charge)
            }
            Self::Private(view) => view.decode_charged(id, charge),
        }
    }

    pub(crate) fn decode_bound(&self, id: TermId, bound: u64) -> Option<usize> {
        match self {
            Self::Shared(dictionary) => {
                if id.kind().is_dictionary() && id.payload() >= bound {
                    return None;
                }
                dictionary.decode_bound(id)
            }
            Self::Private(view) => view.decode_bound(id),
        }
    }

    pub(crate) fn with_view<R>(
        &self,
        id: TermId,
        bound: u64,
        f: impl FnOnce(TermView<'_>) -> R,
    ) -> Option<R> {
        match self {
            Self::Shared(dictionary) => {
                if id.kind().is_dictionary() && id.payload() >= bound {
                    return None;
                }
                dictionary.with_view(id, f)
            }
            Self::Private(view) => view.with_view(id, f),
        }
    }

    pub(crate) fn with_views<R>(
        &self,
        bound: u64,
        f: impl for<'v> FnOnce(&'v dyn Fn(TermId) -> Option<TermView<'v>>) -> R,
    ) -> R {
        match self {
            Self::Shared(dictionary) => dictionary.with_views(bound, f),
            Self::Private(view) => view.with_views(f),
        }
    }

    pub(crate) fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        match self {
            Self::Shared(dictionary) => dictionary.decode_quad(quad),
            Self::Private(view) => view.decode_quad(quad),
        }
    }

    pub(crate) fn matching_strings(&self, test: &StringTest<'_>, bound: u64) -> Vec<TermId> {
        match self {
            Self::Shared(dictionary) => dictionary.matching_strings(test, bound),
            Self::Private(view) => view.matching_strings(test),
        }
    }

    pub(crate) fn matching_strings_scan(
        &self,
        test: &StringTest<'_>,
        bound: u64,
    ) -> (Vec<TermId>, StringScan) {
        match self {
            Self::Shared(dictionary) => dictionary.matching_strings_scan(test, bound),
            Self::Private(view) => view.matching_strings_scan(test),
        }
    }
}
