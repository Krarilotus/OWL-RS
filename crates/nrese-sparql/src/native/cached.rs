//! Plan parts answered from the result cache ([`crate::cache`]): every operator the
//! executor evaluates goes through [`Context::eval_cached`], which looks its part up,
//! waits for a query computing the same part, or computes it and offers the result.
//!
//! A part's key is the context's ([`CacheScope`]: everything a result depends on besides
//! the part: the snapshot, the read model, the dataset as resolved for the user's access,
//! the base IRI and the executor's options), the active graph, then the part's algebra
//! ([`super::cache_key`]). Its result is kept with its variables by number and its
//! computed terms by position, so a hit in another query is renamed and renumbered into
//! that query's own.

use std::sync::Arc;
use std::time::Instant;

use nrese_engine::SnapshotIdentity;
use nrese_exec::{IdTable, computed_index};
use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::TriplePattern;

use super::cache_key::Encoder;
use super::{Context, GraphScope, NativeResult, RangedScan, ScanPattern, Solutions};
use crate::cache::{Claim, Key, Part, PinRequest, ResultCache};
use crate::results::{CancellationToken, QueryEvaluationError};

/// The cache and what every key of a context starts with.
pub(super) struct CacheScope {
    cache: Arc<ResultCache>,
    snapshot: SnapshotIdentity,
    /// What doesn't change between a context and the ones it makes (the base IRI, the
    /// user's access), for [`Context::child_cache`].
    fixed: Arc<[u8]>,
    /// `fixed` and the context's options.
    context: Vec<u8>,
}

impl Context<'_> {
    /// The cache scope of this context: `fixed` (the base IRI, the access) and the
    /// options that decide its results.
    pub(super) fn cache_scope(&self, cache: Arc<ResultCache>, fixed: Arc<[u8]>) -> Arc<CacheScope> {
        let mut encoder = Encoder::default();
        encoder.bytes.extend_from_slice(&fixed);
        encoder.tag(self.model as u8);
        for graphs in [&self.merge_set, &self.named] {
            match graphs {
                Some(graphs) => {
                    encoder.number(graphs.len() as u64);
                    for graph in graphs {
                        encoder.number(graph.raw());
                    }
                }
                None => encoder.number(u64::MAX),
            }
        }
        for flag in [
            self.as_written,
            self.spatial_rewrite,
            self.equality_closed,
            self.late.is_some(),
            self.service_denied,
        ] {
            encoder.tag(u8::from(flag));
        }
        encoder.number(self.cross_chunk_rows as u64);
        encoder.number(self.stream_rows.map_or(u64::MAX, |rows| rows as u64));
        Arc::new(CacheScope {
            cache,
            snapshot: self.snapshot.identity(),
            fixed,
            context: encoder.bytes,
        })
    }

    /// The cache scope of a context made from this one (with its own snapshot and
    /// options).
    pub(super) fn child_cache(&self, child: &Context<'_>) -> Option<Arc<CacheScope>> {
        let scope = self.cache.as_ref()?;
        Some(child.cache_scope(Arc::clone(&scope.cache), Arc::clone(&scope.fixed)))
    }

    /// An encoder holding the start of every key of this context: its options and the
    /// active graph.
    fn key_start(&self, scope: &CacheScope) -> Encoder {
        let mut encoder = Encoder::default();
        encoder.bytes.extend_from_slice(&scope.context);
        match &*self.graph.borrow() {
            GraphScope::Default => encoder.tag(b'd'),
            GraphScope::Union => encoder.tag(b'u'),
            GraphScope::Named(graph) => {
                encoder.tag(b'n');
                encoder.number(graph.raw());
            }
            GraphScope::Variable(variable) => {
                encoder.tag(b'v');
                encoder.var(variable);
            }
            GraphScope::Missing => encoder.tag(b'm'),
        }
        encoder
    }

    /// `pattern`'s key and its variables by number; `None` if it isn't cached.
    fn cache_key(
        &self,
        scope: &CacheScope,
        pattern: &GraphPattern,
    ) -> Option<(Key, Vec<Variable>)> {
        let mut encoder = self.key_start(scope);
        encoder.pattern(pattern).ok()?;
        let key = Key {
            snapshot: scope.snapshot,
            part: encoder.bytes.into(),
        };
        Some((key, encoder.vars))
    }

    fn cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// Evaluates `pattern` through the result cache, if the context has one; `step` is its
    /// step in EXPLAIN's trace, marked when the result came from the cache.
    pub(super) fn eval_cached(
        &self,
        pattern: &GraphPattern,
        step: Option<usize>,
    ) -> NativeResult<Solutions> {
        let pin = self.pin.borrow_mut().take();
        let Some(scope) = &self.cache else {
            return self.eval_operator(pattern);
        };
        // Given rows: nothing to save. A basic graph pattern's joins are parts of their
        // own, the whole pattern's among them ([`Joins`]).
        let trivial = matches!(
            pattern,
            GraphPattern::Values { .. } | GraphPattern::Bgp { .. }
        );
        if trivial && pin.is_none() {
            return self.eval_operator(pattern);
        }
        let Some((key, vars)) = self.cache_key(scope, pattern) else {
            if pin.is_some() {
                return Err(QueryEvaluationError::Argument(
                    "the query can't be pinned: it calls RAND, NOW, UUID, STRUUID or BNODE, \
                     or reads a SERVICE"
                        .to_owned(),
                )
                .into());
            }
            return self.eval_operator(pattern);
        };
        // A LIMIT for a basic graph pattern: its rows are some of the part's, so they
        // aren't kept; the whole part's rows will do.
        if self.limit.get().is_some() {
            return match scope.cache.lookup(&key) {
                Some(part) => self.solutions_of(&part, &vars, step, "hit"),
                None => self.eval_operator(pattern),
            };
        }
        match scope.cache.claim(&key, pin.as_ref()) {
            Claim::Hit(part) => self.solutions_of(&part, &vars, step, "hit"),
            Claim::Wait(flight) => match flight.wait(|| self.cancelled()) {
                Err(()) => Err(QueryEvaluationError::Cancelled.into()),
                Ok(Some(part)) => {
                    scope.cache.note_shared();
                    self.solutions_of(&part, &vars, step, "shared")
                }
                Ok(None) => self.eval_operator(pattern),
            },
            Claim::Bypass if pin.is_some() => Err(QueryEvaluationError::Argument(
                "the store changed while the result was pinned; pin it again".to_owned(),
            )
            .into()),
            Claim::Bypass => self.eval_operator(pattern),
            Claim::Compute(computing) => {
                let start = Instant::now();
                // On an error the claim is dropped: whoever waits computes it.
                let solutions = self.eval_operator(pattern)?;
                let cost = start.elapsed();
                let part = (pin.is_some() || computing.wants(solutions.table.memory_bytes(), cost))
                    .then(|| self.to_part(&solutions, &vars))
                    .flatten();
                if pin.is_some() && part.is_none() {
                    return Err(QueryEvaluationError::Unexpected(
                        "the result has a column its key doesn't name; not pinned".to_owned(),
                    )
                    .into());
                }
                computing
                    .finish(part, cost)
                    .map_err(QueryEvaluationError::Argument)?;
                Ok(solutions)
            }
        }
    }

    /// `solutions` as a part with variables by number (`vars`) and its computed terms by
    /// position; `None` if it has a column the key doesn't number.
    fn to_part(&self, solutions: &Solutions, vars: &[Variable]) -> Option<Part> {
        let numbers = solutions
            .vars
            .iter()
            .map(|v| vars.iter().position(|w| w == v).map(|n| n as u32))
            .collect::<Option<Vec<u32>>>()?;
        let computed = self.computed.borrow();
        if computed.is_empty() {
            return Some(Part {
                vars: numbers,
                table: solutions.table.clone(),
                ordered: solutions.ordered,
                computed: Vec::new(),
            });
        }
        // Computed ids are the query's own: renumber those the table holds from 0.
        let mut local: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        let mut terms = Vec::new();
        let columns = solutions
            .table
            .columns()
            .iter()
            .map(|column| {
                column
                    .iter()
                    .map(|&id| match computed_index(id) {
                        Some(index) => *local.entry(id).or_insert_with(|| {
                            terms.push(computed[index as usize].clone());
                            nrese_exec::computed_id(terms.len() as u64 - 1)
                        }),
                        None => id,
                    })
                    .collect()
            })
            .collect();
        Some(Part {
            vars: numbers,
            table: IdTable::from_columns(columns),
            ordered: solutions.ordered,
            computed: terms,
        })
    }

    /// The solutions of a cached part in this query: its variables by `vars`, its
    /// computed terms as this query's ids. Marks EXPLAIN's `step` with `how`.
    fn solutions_of(
        &self,
        part: &Part,
        vars: &[Variable],
        step: Option<usize>,
        how: &'static str,
    ) -> NativeResult<Solutions> {
        let names = part
            .vars
            .iter()
            .map(|&n| vars.get(n as usize).cloned())
            .collect::<Option<Vec<Variable>>>()
            .ok_or_else(|| {
                QueryEvaluationError::Unexpected(
                    "a cached part numbers another key's variables".into(),
                )
            })?;
        let table = match part.computed.is_empty() {
            true => part.table.clone(),
            false => {
                let ids: Vec<u64> = part.computed.iter().map(|term| self.id(term)).collect();
                IdTable::from_columns(
                    part.table
                        .columns()
                        .iter()
                        .map(|column| {
                            column
                                .iter()
                                .map(|&id| computed_index(id).map_or(id, |i| ids[i as usize]))
                                .collect()
                        })
                        .collect(),
                )
            }
        };
        if let (Some(trace), Some(step)) = (&self.trace, step) {
            trace.borrow_mut()[step].cache = Some(how);
        }
        self.produced(Solutions {
            vars: names,
            table,
            ordered: part.ordered,
        })
    }

    /// Evaluates the query's whole pattern, pinning its result under `pin`'s name.
    pub(super) fn eval_root(
        &self,
        pattern: &GraphPattern,
        pin: Option<&PinRequest>,
    ) -> NativeResult<Solutions> {
        if let Some(pin) = pin {
            if self.cache.is_none() {
                return Err(QueryEvaluationError::Argument(
                    "the result cache is off: nothing can be pinned".to_owned(),
                )
                .into());
            }
            *self.pin.borrow_mut() = Some(pin.clone());
        }
        self.eval(pattern)
    }
}

/// A basic graph pattern as [`Context::bgp`] joins it: its triples, their scans and range
/// hints, the join order, and the filter conjuncts it was given. A prefix of the order is
/// a part too (QLever caches every join of its plan): its rows are the prefix's triples
/// joined, with the conjuncts applied that read only their variables.
pub(super) struct Joins<'p, 'e> {
    pub(super) triples: &'p [TriplePattern],
    pub(super) scans: &'p [ScanPattern],
    pub(super) ranged: &'p [Option<RangedScan<'p>>],
    pub(super) order: &'p [usize],
    /// The conjuncts as given, before the joins applied any.
    pub(super) filters: &'p [(&'e Expression, Vec<Variable>)],
}

impl Joins<'_, '_> {
    /// The variables the first `length` patterns of the order bind.
    fn bound(&self, length: usize) -> Vec<Variable> {
        let mut bound = Vec::new();
        for &i in &self.order[..length] {
            for v in self.scans[i].vars() {
                if !bound.contains(&v) {
                    bound.push(v);
                }
            }
        }
        bound
    }
}

impl Context<'_> {
    /// The key of the first `length` patterns of `joins`' order; `None` if it isn't
    /// cached.
    fn prefix_key(
        &self,
        scope: &CacheScope,
        joins: &Joins<'_, '_>,
        length: usize,
    ) -> Option<(Key, Vec<Variable>)> {
        let mut encoder = self.key_start(scope);
        encoder.tag(b'Q');
        encoder.number(length as u64);
        for &i in &joins.order[..length] {
            encoder.triple(&joins.triples[i]).ok()?;
            match joins.ranged[i] {
                Some((_, ranges)) => {
                    encoder.number(ranges.len() as u64);
                    for (low, high) in ranges {
                        encoder.number(low.raw());
                        encoder.number(high.raw());
                    }
                }
                None => encoder.number(u64::MAX),
            }
        }
        let bound = joins.bound(length);
        for (conjunct, read) in joins.filters {
            if read.iter().all(|v| bound.contains(v)) {
                encoder.tag(b'F');
                encoder.expression(conjunct).ok()?;
            }
        }
        let key = Key {
            snapshot: scope.snapshot,
            part: encoder.bytes.into(),
        };
        Some((key, encoder.vars))
    }

    /// The longest prefix of `joins`' order (of at least two patterns) that the result
    /// cache holds, and its length; the conjuncts it applied are removed from `filters`.
    /// Under a LIMIT (`whole`) only the whole pattern's rows will do: a prefix's would
    /// be cut by the joins after it.
    pub(super) fn cached_prefix(
        &self,
        joins: &Joins<'_, '_>,
        filters: &mut Vec<(&Expression, Vec<Variable>)>,
        whole: bool,
    ) -> NativeResult<Option<(Solutions, usize)>> {
        let Some(scope) = &self.cache else {
            return Ok(None);
        };
        let shortest = match whole {
            true => joins.order.len().max(2),
            false => 2,
        };
        for length in (shortest..=joins.order.len()).rev() {
            let Some((key, vars)) = self.prefix_key(scope, joins, length) else {
                return Ok(None);
            };
            let Some(part) = scope.cache.lookup(&key) else {
                continue;
            };
            let start = Instant::now();
            let solutions = self.solutions_of(&part, &vars, None, "hit")?;
            let bound = joins.bound(length);
            filters.retain(|(_, read)| !read.iter().all(|v| bound.contains(v)));
            if let Some(trace) = &self.trace {
                let detail = joins.order[..length]
                    .iter()
                    .map(|&i| joins.triples[i].to_string())
                    .collect::<Vec<_>>()
                    .join(" . ");
                trace.borrow_mut().push(crate::query::PlanStep {
                    depth: self.depth.get(),
                    operator: "joins".to_owned(),
                    detail,
                    estimated_rows: None,
                    rows: solutions.table.len() as u64,
                    micros: start.elapsed().as_micros() as u64,
                    cache: Some("hit"),
                });
            }
            return Ok(Some((solutions, length)));
        }
        Ok(None)
    }

    /// Offers the rows of the first `length` patterns of `joins`' order, joined in `cost`,
    /// to the result cache.
    pub(super) fn offer_prefix(
        &self,
        joins: &Joins<'_, '_>,
        length: usize,
        solutions: &Solutions,
        cost: std::time::Duration,
    ) {
        let Some(scope) = &self.cache else {
            return;
        };
        if length < 2 {
            return;
        }
        let Some((key, vars)) = self.prefix_key(scope, joins, length) else {
            return;
        };
        scope
            .cache
            .offer(&key, solutions.table.memory_bytes(), cost, || {
                self.to_part(solutions, &vars)
            });
    }
}
