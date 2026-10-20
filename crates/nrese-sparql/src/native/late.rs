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
            evaluator: super::Evaluator::default(),
            computed: RefCell::default(),
            computed_ids: RefCell::default(),
            computed_table: Cell::new(0),
            decoded: RefCell::default(),
            decoded_terms: Cell::new(0),
            decoded_table: Cell::new(0),
            decodes: std::sync::atomic::AtomicU64::new(0),
            numbers: RefCell::default(),
            cancellation: self.cancellation.clone(),
            budget: std::sync::Arc::clone(&self.budget),
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
            synthetic: Cell::new(self.synthetic.get()),
            services: self.services.clone(),
            service_denied: self.service_denied,
            equality_closed: self.equality_closed,
            late: None,
            canonical: None,
            canonical_refused: None,
            cache: None,
            pin: RefCell::default(),
        };
        child.cache = self.child_cache(&child);
        child
    }

    /// Every row of `solutions` with each value replaced by each identity of its class,
    /// independently per column (a value outside every class stays), up to `limit` rows.
    /// Column by column: each row's combinations in odometer order (the last column
    /// fastest), each column written in one pass.
    fn expand_classes(
        &self,
        mut solutions: super::Solutions,
        classes: &nrese_engine::EqualityClasses,
        limit: Option<usize>,
    ) -> super::NativeResult<super::Solutions> {
        let (width, rows) = (solutions.table.width(), solutions.table.len());
        let limit = limit.unwrap_or(usize::MAX);
        let size = |id: u64| classes.members_of(&id).len();
        // Each row's number of combinations.
        let mut factor = vec![1usize; rows];
        for c in 0..width {
            for (f, &id) in factor.iter_mut().zip(solutions.table.column(c)) {
                *f = f.saturating_mul(size(id));
            }
        }
        if factor.iter().all(|&f| f == 1) && rows <= limit {
            return Ok(solutions);
        }
        // The rows whose combinations fit the limit (the last one maybe in part).
        let mut total = 0usize;
        let mut kept = 0;
        while kept < rows && total < limit {
            total = total.saturating_add(factor[kept]);
            kept += 1;
        }
        self.check()?;
        // The columns of the rows that fit, charged before they are written.
        let rows_out = total.min(limit);
        let mut columns = nrese_exec::Columns::new(rows_out);
        for _ in 0..width {
            columns.push(
                std::iter::repeat_n(nrese_exec::UNDEF, rows_out),
                &self.budget,
            )?;
        }
        // Products of the later columns' sizes, per row, built from the last column back.
        let mut stride = vec![1usize; kept];
        for c in (0..width).rev() {
            let column = solutions.table.column(c);
            let out = columns.column_mut(c);
            let mut at = 0;
            for r in 0..kept {
                let members = classes.members_of(&column[r]);
                let repeat = stride[r];
                let rounds = factor[r] / (members.len() * repeat);
                // Past the limit nothing is written, but every row's stride is kept.
                for _ in 0..rounds {
                    for &member in members {
                        let end = (at + repeat).min(out.len());
                        if at < end {
                            out[at..end].fill(member);
                        }
                        at += repeat;
                    }
                }
                stride[r] *= members.len();
            }
            self.check()?;
        }
        let out = if width == 0 {
            nrese_exec::IdTable::rows_only(rows_out)
        } else {
            nrese_exec::IdTable::from_columns(columns)
        };
        let vars = std::mem::take(&mut solutions.vars);
        self.consumed(solutions);
        self.produced(super::Solutions {
            vars,
            table: out,
            ordered: false,
        })
    }
}
