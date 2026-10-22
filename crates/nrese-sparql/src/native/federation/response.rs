//! One physical response's blank-node identities. The index borrows its labels; only
//! fresh names and a rewritten term's temporary tree are owned here. All storage is
//! admitted before allocation and freed before its reservation.

use std::fmt::Write;
use std::sync::Arc;

use nrese_exec::{BudgetExceeded, ChargedVec, Reservation, Ticker};
use nrese_rdf::{BlankNode, NamedOrBlankNode, Term, Triple};

use super::super::{Context, NativeError, NativeResult};
use crate::service::Check;
use crate::{QueryEvaluationError, ServiceResults};

// A letter, a 128-bit namespace, and a 64-bit ordinal: exact ASCII output width.
const FRESH_BYTES: usize = 1 + 32 + 16;

pub(super) struct Scope<'a> {
    labels: ChargedVec<&'a BlankNode>,
    fresh: ChargedVec<BlankNode>,
    _text: Reservation,
}

impl<'a> Scope<'a> {
    /// O(L log L) for L label occurrences; a sorted borrowed index avoids cloning
    /// remote label strings or estimating an opaque hash-table allocation.
    pub(super) fn new(
        ctx: &Context<'_>,
        results: &'a ServiceResults,
    ) -> NativeResult<Option<Self>> {
        ctx.check()?;
        let mut ticker = Ticker::new(Check(ctx)).map_err(QueryEvaluationError::from)?;
        let mut labels = ChargedVec::new(Arc::clone(&ctx.budget));
        for term in results.rows().iter().flatten().flatten() {
            collect(term, &mut labels, &mut ticker)?;
        }
        if labels.is_empty() {
            return Ok(None);
        }
        labels.sort_unstable();
        let mut unique = 0;
        for i in 0..labels.len() {
            if unique == 0 || labels[i] != labels[unique - 1] {
                labels[unique] = labels[i];
                unique += 1;
            }
        }
        labels.truncate(unique);
        ctx.check()?;
        let mut scope = Self {
            fresh: ChargedVec::with_capacity(Arc::clone(&ctx.budget), labels.len())
                .map_err(QueryEvaluationError::MemoryLimit)?,
            labels,
            _text: Reservation::new(Arc::clone(&ctx.budget)),
        };
        let namespace = rand::random::<u128>();
        for index in 0..scope.labels.len() {
            ticker.tick(1).map_err(QueryEvaluationError::from)?;
            scope
                ._text
                .charge(FRESH_BYTES)
                .map_err(QueryEvaluationError::MemoryLimit)?;
            let mut nonce = namespace;
            let node = loop {
                // Writing this exact-width value cannot grow the reserved String.
                let mut label = String::with_capacity(FRESH_BYTES);
                write!(label, "s{nonce:032x}{index:016x}").expect("writing a String is infallible");
                let node = Term::BlankNode(BlankNode::new_unchecked(label));
                // The dictionary owns an encoded-key scratch buffer even for an
                // absent node. Use its existing bound/admission path for this probe.
                let mut lookup = Reservation::new(ctx.budget.as_ref());
                let stored = ctx
                    .lookup_const_charged(node.as_ref(), &mut lookup)
                    .map_err(QueryEvaluationError::MemoryLimit)?;
                drop(lookup);
                if stored.is_none() && !ctx.computed.borrow().contains(&node) {
                    let Term::BlankNode(node) = node else {
                        unreachable!()
                    };
                    break node;
                }
                // The ordinal keeps names within this response distinct even on retry.
                drop(node);
                ctx.check()?;
                nonce = rand::random();
            };
            scope
                .fresh
                .push(node)
                .map_err(QueryEvaluationError::MemoryLimit)?;
        }
        ctx.check()?;
        Ok(Some(scope))
    }

    fn node(&self, label: &BlankNode) -> &BlankNode {
        &self.fresh[self
            .labels
            .binary_search(&label)
            .expect("all response labels indexed")]
    }

    /// Calls `id` with the scoped value; its owning copy is admitted by the computed
    /// term owner. The temporary rewrite is freed before its separate charge releases.
    pub(super) fn with_term(
        &self,
        ctx: &Context<'_>,
        term: &Term,
        id: impl FnOnce(&Term) -> NativeResult<u64>,
    ) -> NativeResult<u64> {
        if !has_blank(term) {
            return id(term);
        }
        let bytes = self.heap(term).ok_or_else(|| overflow(ctx))?;
        let mut held = Reservation::new(Arc::clone(&ctx.budget));
        held.charge(bytes)
            .map_err(QueryEvaluationError::MemoryLimit)?;
        let rewritten = self.rewrite(term);
        let value = id(&rewritten);
        drop(rewritten);
        drop(held);
        ctx.check()?;
        value
    }

    // Exact owned tree components for String::clone and Box::new. A node's struct
    // belongs to its containing row/triple; only its payload is counted here.
    fn heap(&self, term: &Term) -> Option<usize> {
        match term {
            Term::NamedNode(node) => Some(node.as_str().len()),
            Term::BlankNode(node) => Some(self.node(node).as_str().len()),
            Term::Literal(literal) => {
                let datatype = if literal.language().is_some()
                    || literal.datatype() == nrese_rdf::vocab::xsd::STRING
                {
                    0
                } else {
                    literal.datatype().as_str().len()
                };
                literal
                    .value()
                    .len()
                    .checked_add(literal.language().map_or(0, str::len))?
                    .checked_add(datatype)
            }
            Term::Triple(triple) => {
                let subject = match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                    NamedOrBlankNode::BlankNode(node) => self.node(node).as_str().len(),
                };
                size_of::<Triple>()
                    .checked_add(subject)?
                    .checked_add(triple.predicate.as_str().len())?
                    .checked_add(self.heap(&triple.object)?)
            }
        }
    }

    fn rewrite(&self, term: &Term) -> Term {
        match term {
            Term::BlankNode(node) => self.node(node).clone().into(),
            Term::Triple(triple) => {
                let subject: NamedOrBlankNode = match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => node.clone().into(),
                    NamedOrBlankNode::BlankNode(node) => self.node(node).clone().into(),
                };
                Triple::new(
                    subject,
                    triple.predicate.clone(),
                    self.rewrite(&triple.object),
                )
                .into()
            }
            other => other.clone(),
        }
    }
}

fn collect<'a>(
    term: &'a Term,
    labels: &mut ChargedVec<&'a BlankNode>,
    ticker: &mut Ticker<Check<'_>>,
) -> NativeResult<()> {
    ticker.tick(1).map_err(QueryEvaluationError::from)?;
    match term {
        Term::BlankNode(node) => labels
            .push(node)
            .map_err(QueryEvaluationError::MemoryLimit)?,
        Term::Triple(triple) => {
            if let NamedOrBlankNode::BlankNode(node) = &triple.subject {
                labels
                    .push(node)
                    .map_err(QueryEvaluationError::MemoryLimit)?;
            }
            collect(&triple.object, labels, ticker)?;
        }
        _ => {}
    }
    Ok(())
}

fn has_blank(term: &Term) -> bool {
    match term {
        Term::BlankNode(_) => true,
        Term::Triple(triple) => {
            matches!(triple.subject, NamedOrBlankNode::BlankNode(_)) || has_blank(&triple.object)
        }
        _ => false,
    }
}

fn overflow(ctx: &Context<'_>) -> NativeError {
    QueryEvaluationError::MemoryLimit(BudgetExceeded {
        limit: ctx.budget.used().saturating_add(ctx.budget.remaining()),
        requested: usize::MAX,
        used: ctx.budget.used(),
        shared: ctx.budget.bounded_by_shared(),
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryOptions;
    use nrese_engine::{Engine, EngineConfig};
    use nrese_exec::{
        Budget,
        measure::{alone, measure_within},
    };
    use nrese_rdf::{Literal, NamedNode, Variable};

    #[test]
    fn n7_response_scope_admits_and_releases_its_actual_storage() {
        if !alone() {
            return;
        }
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let snapshot = engine.snapshot();
        let options = QueryOptions::default();
        let mut ctx = Context::new(&snapshot, &options, None, None);
        let row = vec![Some(
            Triple::new(
                BlankNode::new_unchecked("first"),
                NamedNode::new_unchecked("urn:predicate"),
                Triple::new(
                    BlankNode::new_unchecked("second"),
                    NamedNode::new_unchecked("urn:another-predicate"),
                    Literal::new_language_tagged_literal("é".repeat(512), "de").unwrap(),
                ),
            )
            .into(),
        )];
        let mut results = ServiceResults::new(vec![Variable::new_unchecked("x")], &ctx.budget);
        results
            .push_row(&row, &nrese_exec::StopSource::none())
            .unwrap();
        for limit in [1, 150, 1 << 20] {
            ctx.budget = Arc::new(Budget::new(limit));
            let (result, uncovered) = measure_within(&ctx.budget, || {
                let scope = Scope::new(&ctx, &results)?.expect("the fixture has blank nodes");
                scope.with_term(&ctx, results.rows()[0][0].as_ref().unwrap(), |_| Ok(7))
            });
            assert_eq!(
                uncovered, 0,
                "all response workspace must be admitted first"
            );
            assert_eq!(
                ctx.budget.used(),
                0,
                "all workspace must release after its buffers"
            );
            if limit == 1 << 20 {
                assert!(matches!(result, Ok(7)));
            } else {
                assert!(matches!(
                    result,
                    Err(NativeError::Evaluation(QueryEvaluationError::MemoryLimit(
                        _
                    )))
                ));
            }
        }
    }

    #[test]
    fn n7_fallible_response_callback_keeps_typed_errors_and_releases_scratch() {
        if !alone() {
            return;
        }
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let snapshot = engine.snapshot();
        let options = QueryOptions::default();
        let mut ctx = Context::new(&snapshot, &options, None, None);
        let row = vec![Some(
            Triple::new(
                BlankNode::new_unchecked("remote"),
                NamedNode::new_unchecked("urn:predicate"),
                Literal::new_simple_literal("payload"),
            )
            .into(),
        )];
        let mut results = ServiceResults::new(vec![Variable::new_unchecked("x")], &ctx.budget);
        results
            .push_row(&row, &nrese_exec::StopSource::none())
            .unwrap();
        let plain: Term = Literal::new_simple_literal("plain").into();
        for term in [&plain, results.rows()[0][0].as_ref().unwrap()] {
            for cancelled in [false, true] {
                ctx.budget = Arc::new(Budget::new(1 << 20));
                let called = std::cell::Cell::new(false);
                let (result, uncovered) = measure_within(&ctx.budget, || {
                    let scope = Scope::new(&ctx, &results)?.expect("a response blank node");
                    scope.with_term(&ctx, term, |_| {
                        called.set(true);
                        if cancelled {
                            Err(QueryEvaluationError::Cancelled.into())
                        } else {
                            let refused =
                                ctx.budget.charge(ctx.budget.remaining() + 1).unwrap_err();
                            Err(QueryEvaluationError::MemoryLimit(refused).into())
                        }
                    })
                });
                assert!(called.get(), "the callback, not setup, must fail");
                assert_eq!(
                    uncovered, 0,
                    "callback failure leaves no unadmitted scratch"
                );
                assert_eq!(ctx.budget.used(), 0, "callback failure releases workspace");
                if cancelled {
                    assert!(matches!(
                        result,
                        Err(NativeError::Evaluation(QueryEvaluationError::Cancelled))
                    ));
                } else {
                    assert!(matches!(
                        result,
                        Err(NativeError::Evaluation(QueryEvaluationError::MemoryLimit(
                            _
                        )))
                    ));
                }
            }
        }
    }
}
