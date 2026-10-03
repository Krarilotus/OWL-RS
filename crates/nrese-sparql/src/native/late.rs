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
        GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
            safe(left) && safe(right)
        }
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
        let child = self.canonical_child();
        let solutions = child.eval(pattern)?;
        let rows = solutions.table.len();
        let expanded = self.expand_classes(solutions, classes, limit)?;
        if self.trace.is_some() {
            let detail = format!("{rows} rows over representatives");
            self.note(
                "late equality expansion",
                detail,
                None,
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
        super::Context {
            snapshot: std::borrow::Cow::Owned(self.snapshot.with_canonical_equality()),
            model: self.model,
            aliases: RefCell::new(self.aliases.borrow().clone()),
            evaluator: super::Evaluator::default(),
            computed: RefCell::default(),
            computed_ids: RefCell::default(),
            decoded: RefCell::default(),
            numbers: RefCell::default(),
            cancellation: self.cancellation.clone(),
            budget: std::sync::Arc::clone(&self.budget),
            trace: None,
            depth: Cell::new(0),
            limit: Cell::new(None),
            graph: RefCell::new(self.graph.borrow().clone()),
            as_written: self.as_written,
            cross_chunk_rows: self.cross_chunk_rows,
            stream_rows: self.stream_rows,
            merge_set: self.merge_set.clone(),
            named: self.named.clone(),
            synthetic: Cell::new(self.synthetic.get()),
            services: self.services.clone(),
            service_denied: self.service_denied,
            equality_closed: self.equality_closed,
            late: None,
        }
    }

    /// Every row of `solutions` with each value replaced by each identity of its class,
    /// independently per column (a value outside every class stays), up to `limit` rows.
    fn expand_classes(
        &self,
        solutions: super::Solutions,
        classes: &nrese_engine::EqualityClasses,
        limit: Option<usize>,
    ) -> super::NativeResult<super::Solutions> {
        let width = solutions.table.width();
        let limit = limit.unwrap_or(usize::MAX);
        let mut out = nrese_exec::IdTable::new(width);
        let mut values = vec![0u64; width];
        let mut row = vec![0u64; width];
        let mut at = vec![0usize; width];
        for r in 0..solutions.table.len() {
            if r % (1 << 14) == 0 {
                self.check()?;
            }
            for (c, value) in values.iter_mut().enumerate() {
                *value = solutions.table.get(r, c);
            }
            let choices: Vec<&[u64]> = values.iter().map(|v| classes.members_of(v)).collect();
            at.iter_mut().for_each(|i| *i = 0);
            // Every combination, the last column fastest.
            'combinations: loop {
                if out.len() >= limit {
                    break;
                }
                for c in 0..width {
                    row[c] = choices[c][at[c]];
                }
                out.push_row(&row);
                let mut c = width;
                loop {
                    if c == 0 {
                        break 'combinations;
                    }
                    c -= 1;
                    at[c] += 1;
                    if at[c] < choices[c].len() {
                        break;
                    }
                    at[c] = 0;
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        self.consumed(&solutions);
        self.produced(super::Solutions {
            vars: solutions.vars,
            table: out,
            ordered: false,
        })
    }
}
