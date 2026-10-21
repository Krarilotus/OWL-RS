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

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};

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
        // Every graph, the listed ones, or every graph but the listed ones: each told apart
        // (a list's length is never u64::MAX - 1).
        let (merged, left_out) = match &self.merge_set {
            None => (None, false),
            Some(crate::dataset::GraphSet::Only(graphs)) => (Some(graphs), false),
            Some(crate::dataset::GraphSet::AllBut(graphs)) => (Some(graphs), true),
        };
        if left_out {
            encoder.number(u64::MAX - 1);
        }
        for graphs in [merged, self.named.as_ref()] {
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

/// The key of `query`'s whole answer written in `format` (the media type with its
/// parameters) on `snapshot`, for a caller whose answer depends on `context` too: its
/// context, the format, the caller's context, the QL rewriting, the query form with a CONSTRUCT's
/// template, and its pattern as parsed (the answer is the query's, however it is planned).
/// `None` if the query isn't cached (no cache, a volatile query, pre-bound variables).
pub(crate) fn output_key(
    snapshot: &nrese_engine::Snapshot,
    query: &nrese_sparql_syntax::Query,
    options: &crate::QueryOptions,
    format: &str,
    context: &str,
) -> Option<Key> {
    use nrese_sparql_syntax::Query;
    if options.pre_bound.is_some() {
        return None;
    }
    let ctx = Context::new(
        snapshot,
        options,
        super::query_dataset(query),
        super::query_base(query),
    );
    let scope = ctx.cache.as_ref()?;
    let mut encoder = ctx.key_start(scope);
    encoder.tag(b'S');
    encoder.text(format);
    encoder.text(context);
    // The QL rewriting changes the answer of the query as parsed (parts see it in the
    // algebra they evaluate): its closure and bounds; its TBox is the snapshot's.
    match super::ql_rewriting(query, options) {
        Some(ql) => {
            encoder.tag(b'q');
            encoder.text(&ql.fingerprint());
        }
        None => encoder.tag(b'-'),
    }
    let pattern = match query {
        Query::Select { pattern, .. } => {
            encoder.tag(b's');
            pattern
        }
        Query::Ask { pattern, .. } => {
            encoder.tag(b'a');
            pattern
        }
        Query::Describe { pattern, .. } => {
            encoder.tag(b'd');
            pattern
        }
        Query::Construct {
            template, pattern, ..
        } => {
            encoder.tag(b'c');
            encoder.number(template.len() as u64);
            for triple in template {
                encoder.triple(triple).ok()?;
            }
            pattern
        }
    };
    encoder.pattern(pattern).ok()?;
    Some(Key {
        snapshot: scope.snapshot,
        part: encoder.bytes.into(),
    })
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
    /// The prefixes' keys, made when the cache is first asked ([`PrefixKeys`]).
    pub(super) keys: OnceCell<Option<PrefixKeys>>,
}

/// The keys of a basic graph pattern's prefixes, chained: prefix k's key holds the digest
/// of prefix k − 1's, the k-th triple with its range hint, and the conjuncts that apply
/// from k on. Each key is a few dozen bytes and all of them O(n) to make, where each
/// spelled out its whole prefix: a pattern of n triples took O(n²) bytes and O(n³) time to
/// key, before and after every join (a path of 1,000 steps, 12 October 2026). Equal
/// prefixes still share an entry: the chain is a function of the prefix, its variables
/// numbered as they first occur; a SHA-256 link keeps distinct prefixes apart.
pub(super) struct PrefixKeys {
    /// Prefix k's key at k − 1; none from a triple or conjunct that can't be keyed on.
    keys: Vec<Option<Key>>,
    /// The variables by number, in the order they are first written.
    vars: Vec<Variable>,
    /// How many of them prefix k's key has numbered, at k − 1.
    numbered: Vec<usize>,
    /// The prefix length from which each variable is bound.
    bound_at: HashMap<Variable, usize>,
}

impl PrefixKeys {
    /// Prefix `length`'s key and its variables by number.
    fn key(&self, length: usize) -> Option<(&Key, &[Variable])> {
        let key = self.keys.get(length.checked_sub(1)?)?.as_ref()?;
        Some((key, &self.vars[..self.numbered[length - 1]]))
    }

    /// The prefix length from which a conjunct reading `read` applies; `None` if no
    /// prefix binds them all.
    fn applies_from(&self, read: &[Variable]) -> Option<usize> {
        read.iter()
            .try_fold(1, |from: usize, v| Some(from.max(*self.bound_at.get(v)?)))
    }
}

impl Context<'_> {
    /// [`PrefixKeys`] for `joins`, made once, in one pass over its order: O(n + the
    /// conjuncts).
    fn prefix_keys<'j>(&self, joins: &'j Joins<'_, '_>) -> Option<&'j PrefixKeys> {
        joins
            .keys
            .get_or_init(|| {
                let scope = self.cache.as_ref()?;
                let mut bound_at = HashMap::new();
                for (step, &i) in joins.order.iter().enumerate() {
                    for v in joins.scans[i].vars() {
                        bound_at.entry(v).or_insert(step + 1);
                    }
                }
                let mut keys = PrefixKeys {
                    keys: Vec::with_capacity(joins.order.len()),
                    vars: Vec::new(),
                    numbered: Vec::with_capacity(joins.order.len()),
                    bound_at,
                };
                let applies: Vec<Option<usize>> = joins
                    .filters
                    .iter()
                    .map(|(_, read)| keys.applies_from(read))
                    .collect();
                let mut encoder = self.key_start(scope);
                encoder.tag(b'Q');
                let start = encoder.bytes.len();
                let mut link = [0u8; 32];
                let mut keyed = true;
                for (step, &i) in joins.order.iter().enumerate() {
                    let length = step + 1;
                    encoder.bytes.truncate(start);
                    encoder.number(length as u64);
                    encoder.bytes.extend_from_slice(&link);
                    keyed = keyed && encoder.triple(&joins.triples[i]).is_ok();
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
                    for ((conjunct, _), from) in joins.filters.iter().zip(&applies) {
                        if keyed && *from == Some(length) {
                            encoder.tag(b'F');
                            keyed = encoder.expression(conjunct).is_ok();
                        }
                    }
                    keys.keys.push(keyed.then(|| {
                        #[cfg(test)]
                        KEY_BYTES.with(|bytes| bytes.set(bytes.get() + encoder.bytes.len()));
                        link = Sha256::digest(&encoder.bytes).into();
                        Key {
                            snapshot: scope.snapshot,
                            part: encoder.bytes.as_slice().into(),
                        }
                    }));
                    keys.numbered.push(encoder.vars.len());
                }
                keys.vars = encoder.vars;
                Some(keys)
            })
            .as_ref()
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
        let (Some(scope), Some(keys)) = (&self.cache, self.prefix_keys(joins)) else {
            return Ok(None);
        };
        let shortest = match whole {
            true => joins.order.len().max(2),
            false => 2,
        };
        for length in (shortest..=joins.order.len()).rev() {
            let Some((key, vars)) = keys.key(length) else {
                continue;
            };
            let Some(part) = scope.cache.lookup(key) else {
                continue;
            };
            let start = Instant::now();
            let solutions = self.solutions_of(&part, vars, None, "hit")?;
            filters.retain(|(_, read)| keys.applies_from(read).is_none_or(|from| from > length));
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
        let (Some(scope), Some(keys)) = (&self.cache, self.prefix_keys(joins)) else {
            return;
        };
        if length < 2 {
            return;
        }
        let Some((key, vars)) = keys.key(length) else {
            return;
        };
        scope
            .cache
            .offer(key, solutions.table.memory_bytes(), cost, || {
                let part = self.to_part(solutions, vars)?;
                Some(crate::cache::Payload::Part(Arc::new(part)))
            });
    }
}

#[cfg(test)]
thread_local! {
    /// The bytes of the prefix keys made ([`PrefixKeys`]): the guard that they are linear in
    /// a pattern's length.
    static KEY_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use nrese_rdf::{GraphName, NamedNode, Quad};
    use nrese_sparql_syntax::SparqlParser;

    use crate::{QueryOptions, QueryResults, ResultCache, evaluate_query};

    /// A path of n steps is a basic graph pattern of n triples, and each prefix of its join
    /// order a part: their keys are linear in n (each spelled out its whole prefix, O(n²)
    /// bytes and O(n³) time: 12 October 2026).
    #[test]
    fn prefix_keys_grow_linearly_with_the_pattern() {
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let node = |name: String| NamedNode::new_unchecked(format!("http://e/{name}"));
        let mut tx = engine.transaction();
        for s in 0..5 {
            for p in 0..7 {
                let quad = Quad::new(
                    node(format!("s{s}")),
                    node(format!("p{p}")),
                    node(format!("s{}", (s + p) % 5)),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let options = QueryOptions {
            result_cache: Some(std::sync::Arc::new(
                ResultCache::new(1 << 24).admitting_all(),
            )),
            ..QueryOptions::default()
        };
        let bytes = |n: usize| {
            let steps: Vec<String> = (0..n).map(|i| format!("<http://e/p{}>", i % 7)).collect();
            let text = format!("SELECT * WHERE {{ ?s {} ?o }}", steps.join("/"));
            let query = SparqlParser::new().parse_query(&text).unwrap();
            super::KEY_BYTES.with(|bytes| bytes.set(0));
            let snapshot = engine.snapshot();
            let QueryResults::Solutions(solutions) =
                evaluate_query(&snapshot, &query, &options).unwrap()
            else {
                unreachable!("a SELECT")
            };
            assert_eq!(solutions.count(), 5);
            super::KEY_BYTES.with(|bytes| bytes.get())
        };
        let (short, long) = (bytes(100), bytes(400));
        assert!(short > 0, "the prefixes were keyed");
        assert!(long < 5 * short, "100 steps: {short} bytes, 400: {long}");
    }
}
