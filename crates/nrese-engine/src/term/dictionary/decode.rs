//! Complete admitted decoding. Encoding, inline formatting and legacy syntax remain at
//! their existing owners; this module owns the allocation lifetimes around those calls.

use std::ops::Deref;

use nrese_exec::{Budget, BudgetExceeded, Reservation};
use nrese_rdf::{NamedOrBlankNode, Term, Triple};

use super::{Dictionary, Inner, Key, TAG_TRIPLE, decode_key, inline_to_literal, triple_ids};
use crate::term::TermId;

#[cfg(test)]
mod tests;

impl Dictionary {
    /// [`Self::decode`] with admission before allocating. Its ID domain and values are
    /// identical to `decode`, including inline and legacy/current triple terms. This is
    /// dictionary knowledge, not a graph-read or snapshot-membership check.
    ///
    /// Bound and allocation share one recursive dictionary read guard. Temporary buffers
    /// are freed before their charge is returned; returned heap remains added to `held`.
    /// On refusal or an unknown ID only this call's added charge is released. The caller
    /// must keep the returned term's added charge until that term is freed.
    pub fn decode_within<B: Deref<Target = Budget>>(
        &self,
        id: TermId,
        held: &mut Reservation<B>,
    ) -> Result<Option<Term>, BudgetExceeded> {
        let before = held.bytes();
        let result = if id.kind().is_dictionary() {
            // A caller may already hold a read (`with_views`). Do not queue a plain read
            // behind a writer which is itself waiting for that caller's guard.
            decode(&self.inner.read_recursive(), id, held)
        } else {
            inline(id, held)
        };
        if !matches!(result, Ok(Some(_))) {
            // Every key/partial term has dropped inside `decode` before releasing credit.
            held.release(held.bytes() - before);
        }
        result
    }
}

fn inline<B: Deref<Target = Budget>>(
    id: TermId,
    held: &mut Reservation<B>,
) -> Result<Option<Term>, BudgetExceeded> {
    let Some(peak) = super::super::inline::decode_peak(id) else {
        return Ok(None);
    };
    held.charge(peak)?;
    let term = inline_to_literal(id).map(Term::from);
    let retained = term.as_ref().map_or(0, Term::heap_bytes);
    held.release(peak - retained);
    Ok(term)
}

fn decode<B: Deref<Target = Budget>>(
    inner: &Inner,
    id: TermId,
    held: &mut Reservation<B>,
) -> Result<Option<Term>, BudgetExceeded> {
    if !id.kind().is_dictionary() {
        return inline(id, held);
    }
    let index = id.payload();
    if index >= inner.len() {
        return Ok(None);
    }
    // Keep the common atomic path as one admission, the existing key decoder, then a
    // trim. No decoded-term prewalk, second dictionary lookup or per-byte budget traffic.
    let peak = key_peak(inner, index).ok_or_else(|| overflow(held.budget()))?;
    held.charge(peak)?;
    let key = inner.key(index);
    if let Some([s, p, o]) = triple_ids(&key) {
        drop(key);
        held.release(peak);
        let Some(Term::NamedNode(predicate)) = decode(inner, p, held)? else {
            return Ok(None);
        };
        let Some(subject) =
            decode(inner, s, held)?.and_then(|t| NamedOrBlankNode::try_from(t).ok())
        else {
            return Ok(None);
        };
        let Some(object) = decode(inner, o, held)? else {
            return Ok(None);
        };
        held.charge(size_of::<Triple>())?;
        return Ok(Some(Triple::new(subject, predicate, object).into()));
    }
    let extra = if key.first() == Some(&TAG_TRIPLE) {
        let extra = legacy_peak(key.len()).ok_or_else(|| overflow(held.budget()))?;
        held.charge(extra)?;
        extra
    } else {
        0
    };
    let term = decode_key(&key);
    drop(key);
    // The atomic key bounds its strings; legacy admission also covers the parser's two
    // simultaneous trees and scratch. All of those temporaries have now been freed.
    let retained = term.heap_bytes();
    held.release(peak + extra - retained);
    Ok(Some(term))
}

/// Checked version of the existing atomic bound, for the representation pinned above.
fn key_peak(inner: &Inner, index: u64) -> Option<usize> {
    match inner.base.as_ref().filter(|base| index < base.len) {
        Some(base) if base.codec.is_some() => {
            let stored = base.key(index).len();
            let moved = stored.checked_mul(8)?.checked_add(9)?;
            Some(
                stored
                    .checked_mul(3)?
                    .checked_add(moved)?
                    .max(moved.checked_mul(2)?),
            )
        }
        _ => Some(match inner.key(index) {
            Key::Borrowed(key) => key.len(),
            Key::Owned(_) => unreachable!("only compressed bases allocate keys"),
        }),
    }
}

/// Old `R` keys use the existing N-Triples parser (`parse_triple_term`), not a second
/// parser. For n key bytes: the copied text and formatted line, seven scratch strings
/// (each < 2n, plus one old/new growth overlap), both the parser's tree and its owned
/// output (< 2n text), and error/fallback text fit within 32(n+32) bytes. Each triple
/// consumes at least six delimiter bytes; two trees need at most 2(n/6+1) Triple boxes.
/// The additional fixed room covers the wrapper's two constant IRIs and syntax errors.
/// This is deliberately a peak admission for the retained legacy representation only;
/// current ID triples admit their actual components/boxes incrementally instead.
fn legacy_peak(n: usize) -> Option<usize> {
    let text = n.checked_add(32)?.checked_mul(32)?;
    let boxes = (n / 6)
        .checked_add(1)?
        .checked_mul(2 * size_of::<Triple>())?;
    text.checked_add(boxes)?.checked_add(256)
}

fn overflow(budget: &Budget) -> BudgetExceeded {
    BudgetExceeded {
        limit: budget.limit(),
        requested: usize::MAX,
        used: budget.used(),
        shared: false,
    }
}
