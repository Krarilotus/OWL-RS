//! Equality classes expanded after the joins (work package W4, stage C).
//!
//! A store that keeps the closure over one representative per `owl:sameAs` class reads
//! every statement about a class once per identity of each of its terms (stage B,
//! `nrese_engine`'s expanding reads). A join of such reads makes every combination of
//! identities, then joins them: a class of k identities on a join variable costs k
//! times the rows at every step. Here the parts of a query whose answer can't tell
//! identities apart are evaluated over canonical reads (each statement once, over
//! representatives) and their solutions expanded once, each value to its class.
//!
//! **Why that is the same answer.** Expanded reads are the canonical ones with every
//! value replaced by each identity of its class, independently per position. A join on a
//! variable keeps the rows whose values are equal, and two identities are equal only if
//! they are one: so the expanded join is the canonical join with each variable's value
//! replaced by each identity of its class, once per variable. The same holds for
//! OPTIONAL without a condition (a row is unmatched in the expanded join exactly when its
//! representative is unmatched in the canonical one) and for UNION.
//!
//! **What is not safe, and stays expanded early:** anything that looks at terms or
//! compares them (FILTER, BIND, expressions, OPTIONAL with a condition), anything that
//! brings terms of its own (VALUES, subqueries with their own modifiers), MINUS (its
//! compatibility test on representatives would remove rows whose identities differ),
//! property paths (a zero-length path pairs a representative with itself, not with the
//! other identities), named graphs, and the operators that count or order (DISTINCT,
//! aggregates, ORDER BY, LIMIT: they run above the expansion, on expanded rows).
//! [`super::Context::late_expansion`] applies only where reads show every copy of a
//! statement (`nrese_engine::Snapshot::expand_late`).

use nrese_sparql_syntax::algebra::GraphPattern;

/// Whether `pattern` has a join for the expansion to save: a basic graph pattern of two
/// or more triple patterns, or a join, OPTIONAL or UNION of patterns.
pub(super) fn joins(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => patterns.len() >= 2,
        GraphPattern::Join { .. } | GraphPattern::LeftJoin { .. } | GraphPattern::Union { .. } => {
            true
        }
        _ => false,
    }
}

/// Whether `pattern`'s answer is the expansion of its answer over representatives
/// (module docs).
pub(super) fn safe(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => {
            super::spatial::split(patterns).is_none()
                && super::search::split(patterns).is_none()
                && patterns.iter().all(super::supported_triple)
        }
        GraphPattern::Join { inputs } | GraphPattern::Union { inputs } => inputs.iter().all(safe),
        GraphPattern::LeftJoin {
            left,
            right,
            expression: None,
        } => safe(left) && safe(right),
        _ => false,
    }
}

impl super::Context<'_> {
    /// Whether the triple patterns match in the default graph as a whole (the store's,
    /// or the merge of every graph): where canonical reads stand for the expanded ones.
    pub(super) fn late_scope(&self) -> bool {
        match &*self.graph.borrow() {
            super::GraphScope::Default => true,
            super::GraphScope::Union => self.merge_set.is_none(),
            _ => false,
        }
    }

    /// `pattern` evaluated over canonical reads, its solutions expanded once (module
    /// docs).
    pub(super) fn late_expansion(
        &self,
        pattern: &GraphPattern,
        classes: &std::sync::Arc<nrese_engine::EqualityClasses>,
    ) -> super::NativeResult<super::Solutions> {
        let start = std::time::Instant::now();
        // A LIMIT meant for the next basic graph pattern: any rows will do, so the
        // expanded rows are cut instead.
        let limit = self.limit.take();
        let estimate = match self.trace {
            Some(_) => self.estimate_rows(pattern),
            None => None,
        };
        let child = self.canonical_child();
        let solutions = child.eval(pattern)?;
        let decodes = child.decodes.load(std::sync::atomic::Ordering::Relaxed);
        self.decodes
            .fetch_add(decodes, std::sync::atomic::Ordering::Relaxed);
        let rows = solutions.table.len();
        let expanded = self.expand_classes(solutions, classes, limit)?;
        if self.trace.is_some() {
            let detail = format!("{rows} rows over representatives");
            self.note(
                "late equality expansion",
                detail,
                estimate,
                expanded.table.len(),
                start,
            );
        }
        Ok(expanded)
    }

    /// A context like this one over this snapshot read canonically, without late
    /// expansion of its own.
    fn canonical_child(&self) -> super::Context<'static> {
        use std::cell::{Cell, RefCell};
        let mut child = super::Context {
            snapshot: std::borrow::Cow::Owned(self.snapshot.with_canonical_equality()),
            model: self.model,
            aliases: RefCell::new(self.aliases.borrow().clone()),
            aliases_refused: Cell::new(self.aliases_refused.get()),
            room_unread: Cell::new(false),
            evaluator: super::Evaluator::default(),
            computed: RefCell::default(),
            computed_ids: RefCell::default(),
            decoded: RefCell::new(super::decode_cache::DecodeCache::new(
                &self.budget,
                super::DECODE_CACHE_ENTRIES,
                1,
            )),
            distinct_counts: RefCell::new(nrese_exec::ChargedMap::new(std::sync::Arc::clone(
                &self.budget,
            ))),
            decodes: std::sync::atomic::AtomicU64::new(0),
            numbers: RefCell::default(),
            cancellation: self.cancellation.clone(),
            budget: std::sync::Arc::clone(&self.budget),
            kept: RefCell::new(nrese_exec::Reservation::new(std::sync::Arc::clone(
                &self.budget,
            ))),
            memory: self.memory.clone(),
            trace: None,
            depth: Cell::new(0),
            probed: Cell::default(),
            limit: Cell::new(None),
            graph: RefCell::new(self.graph.borrow().clone()),
            as_written: self.as_written,
            spatial_rewrite: self.spatial_rewrite,
            cross_chunk_rows: self.cross_chunk_rows,
            stream_rows: self.stream_rows,
            index_joins: self.index_joins,
            merge_set: self.merge_set.clone(),
            named: self.named.clone(),
            store_default: self.store_default,
            synthetic: Cell::new(self.synthetic.get()),
            services: self.services.clone(),
            service_denied: self.service_denied,
            equality_closed: self.equality_closed,
            representatives: super::equality::Found::default(),
            late: None,
            canonical: None,
            canonical_refused: None,
            reads_refused: self.reads_refused,
            cache: None,
            pin: RefCell::default(),
            search_scope: self.search_scope,
        };
        child.cache = self.child_cache(&child);
        child
    }

    /// Every row of `solutions` with each value replaced by each identity of its class,
    /// independently per column (a value outside every class stays), up to `limit` rows.
    /// Column by column: each row's combinations in odometer order (the last column
    /// fastest), each column written in one pass. Every buffer it makes is charged before
    /// it is made and allocated fallibly, so an expansion past the query's budget, or past
    /// what memory holds, fails as [`MemoryLimit`](crate::QueryEvaluationError::MemoryLimit).
    fn expand_classes(
        &self,
        solutions: super::Solutions,
        classes: &nrese_engine::EqualityClasses,
        limit: Option<usize>,
    ) -> super::NativeResult<super::Solutions> {
        let (width, rows) = (solutions.table.width(), solutions.table.len());
        let limit = limit.unwrap_or(usize::MAX);
        let size = |id: u64| classes.members_of(&id).len();
        // The first value in a class of two or more identities, column by column. Without
        // one (without columns too) every row is one combination: the rows are the answer,
        // cut to the limit in place.
        let first = (0..width).find_map(|c| {
            let column = solutions.table.column(c);
            column.iter().position(|&id| size(id) > 1).map(|r| (c, r))
        });
        let Some((first_column, first_row)) = first else {
            let mut solutions = solutions;
            if rows > limit {
                solutions.table.slice(0, Some(limit));
            }
            return Ok(solutions);
        };
        // What it makes, charged before it is made, beside the input's charge. A size past
        // `usize` (a saturated total included) is charged as the most bytes there are, which
        // no budget holding the input grants.
        let mut held =
            nrese_exec::Reservation::adopt(&*self.budget, solutions.table.memory_bytes());
        let mut charge = |bytes| {
            held.charge(bytes)
                .map_err(crate::QueryEvaluationError::MemoryLimit)
        };
        // Declared after `held`, as every buffer below: freed before its charge is released.
        let mut solutions = solutions;
        // Each row's number of combinations (`usize::MAX`: that many or more); the values
        // before the first in a class count once.
        charge(bytes::<usize>(rows))?;
        let mut factor = self.room::<usize>(rows)?;
        factor.resize(rows, 1);
        for c in first_column..width {
            let from = if c == first_column { first_row } else { 0 };
            let column = &solutions.table.column(c)[from..];
            for (f, &id) in factor[from..].iter_mut().zip(column) {
                *f = f.saturating_mul(size(id));
            }
        }
        // The rows whose combinations fit the limit (the last one maybe in part), each
        // written whole: a column holds `total` ids until it is cut to the limit.
        let mut total = 0usize;
        let mut kept = 0;
        while kept < rows && total < limit {
            total = total.saturating_add(factor[kept]);
            kept += 1;
        }
        // The columns, their list and the kept rows' strides, all charged before the first
        // is made.
        charge(total.checked_mul(width).map_or(usize::MAX, bytes::<u64>))?;
        charge(bytes::<Vec<u64>>(width))?;
        charge(bytes::<usize>(kept))?;
        self.check()?;
        let mut columns = self.room::<Vec<u64>>(width)?;
        columns.resize_with(width, Vec::new);
        // Products of the later columns' sizes, per row, built from the last column back.
        let mut stride = self.room::<usize>(kept)?;
        stride.resize(kept, 1);
        for c in (0..width).rev() {
            let column = solutions.table.column(c);
            let mut out = self.room::<u64>(total)?;
            for r in 0..kept {
                let members = classes.members_of(&column[r]);
                let repeat = stride[r];
                let rounds = factor[r] / (members.len() * repeat);
                for _ in 0..rounds {
                    for &member in members {
                        out.extend(std::iter::repeat_n(member, repeat));
                    }
                }
                stride[r] *= members.len();
            }
            out.truncate(limit);
            columns[c] = out;
            self.check()?;
        }
        let out = nrese_exec::IdTable::from_columns(columns);
        let vars = std::mem::take(&mut solutions.vars);
        drop((factor, stride, solutions));
        self.check()?;
        held.resize(out.memory_bytes())
            .map_err(crate::QueryEvaluationError::MemoryLimit)?;
        held.transfer();
        Ok(super::Solutions {
            vars,
            table: out,
            ordered: false,
        })
    }

    /// An empty vector with room for `len` items, allocated fallibly: a budget without a
    /// limit grants any size, and the allocator may still refuse it.
    fn room<T>(&self, len: usize) -> super::NativeResult<Vec<T>> {
        let mut items = Vec::new();
        if items.try_reserve_exact(len).is_err() {
            let refused = nrese_exec::BudgetExceeded {
                limit: self.budget.limit(),
                requested: bytes::<T>(len),
                used: self.budget.used(),
                shared: false,
                unread: false,
            };
            return Err(crate::QueryEvaluationError::MemoryLimit(refused).into());
        }
        Ok(items)
    }
}

/// The bytes of `len` items of `T` (`usize::MAX` past `usize`).
fn bytes<T>(len: usize) -> usize {
    len.saturating_mul(std::mem::size_of::<T>())
}

#[cfg(test)]
mod tests {
    use super::super::{Context, Solutions};
    use crate::QueryOptions;
    use nrese_engine::{EncodedTriple, Engine, EngineConfig};
    use nrese_exec::measure::{alone, measure_within};
    use nrese_exec::{Envelope, IdTable, Reservation};
    use nrese_rdf::{NamedNode, Variable};

    /// What an expansion holds beyond its charge doesn't grow with its rows, on each path:
    /// rows expanded over a class of two identities, rows with no value in a class, rows
    /// without columns, each cut to a limit or not. Beyond its charge it holds no more than
    /// a table's list of columns, which a table's charge leaves out
    /// (`IdTable::memory_bytes`). Each input is charged before it is made, inside the
    /// measurement, and consuming what it answers releases its own charge, never another
    /// holder's.
    #[test]
    fn an_expansion_holds_no_more_than_it_charges() {
        if !alone() {
            return;
        }
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let node = |name: &str| NamedNode::new_unchecked(format!("urn:{name}"));
        let [same_as, a, b, outside] =
            ["sameAs", "a", "b", "outside"].map(|name| tx.intern(node(name).as_ref().into()));
        tx.insert_inferred(EncodedTriple {
            subject: b,
            predicate: same_as,
            object: a,
        });
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        let snapshot = engine.snapshot();
        let classes = snapshot.equality_classes().expect("a class");
        let (class, _) = classes.iter().next().expect("a class");
        let outside = outside.raw();
        let envelope = Envelope::unlimited();
        let mut options = QueryOptions::default();
        options.resources.memory = envelope.clone();
        let ctx = Context::new(&snapshot, &options, None, None);
        // Another holder's charge, held across every expansion: more than any input's, so
        // releasing an input's bytes it never charged shows.
        let mut other = Reservation::new(&*ctx.budget);
        other.charge(1 << 20).unwrap();
        // What expanding `rows` rows, each `row`, holds beyond its charge, and the rows it
        // answers.
        let expand = |row: &[u64], rows: usize, limit: Option<usize>| -> (usize, usize) {
            let width = row.len();
            let vars: Vec<Variable> = (0..width)
                .map(|i| Variable::new_unchecked(format!("v{i}")))
                .collect();
            let before = ctx.budget.used();
            let (expanded, excess) = measure_within(&envelope, || {
                let mut charged = Reservation::new(&*ctx.budget);
                // One id a column of each row; without columns, one id a row after the first.
                let ids = if width == 0 { rows - 1 } else { rows * width };
                charged.charge(ids * 8).unwrap();
                let mut table = IdTable::with_capacity(width, rows);
                for _ in 0..rows {
                    table.push_row(row);
                }
                charged.transfer();
                let input = Solutions {
                    vars,
                    table,
                    ordered: false,
                };
                ctx.expand_classes(input, &classes, limit)
                    .ok()
                    .expect("expanded")
            });
            let answered = expanded.table.len();
            ctx.consumed(expanded);
            assert_eq!(
                ctx.budget.used(),
                before,
                "{width} columns, {rows} rows: consuming the answer moved another holder's charge"
            );
            (excess, answered)
        };
        for (path, row, per_row) in [
            ("expanded", &[class, outside][..], 2),
            ("in no class", &[outside, outside][..], 1),
            ("without columns", &[][..], 1),
        ] {
            for cut in [false, true] {
                let held = |rows: usize| {
                    let limit = cut.then_some(rows / 2);
                    let (excess, answered) = expand(row, rows, limit);
                    let answers = limit.unwrap_or(rows * per_row);
                    assert_eq!(answered, answers, "{path}, {rows} rows, limit {limit:?}");
                    excess
                };
                let (few, many) = (held(4_096), held(32_768));
                assert_eq!(
                    many, few,
                    "{path}, cut {cut}: uncharged bytes grow with the rows"
                );
                let list = row.len() * std::mem::size_of::<Vec<u64>>();
                assert!(
                    few <= list,
                    "{path}, cut {cut}: {few} bytes uncharged, more than a list of columns"
                );
            }
        }
    }
}
