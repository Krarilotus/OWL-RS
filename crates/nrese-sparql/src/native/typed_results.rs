//! Owned results before decoding. IDs never outlive their dictionary or computed terms.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use nrese_engine::Snapshot;
use nrese_exec::{Budget, IdTable, UNDEF, computed_id, computed_index};
use nrese_rdf::{Term, Triple, Variable};

use crate::{QueryEvaluationError, QueryResults, QuerySolutionIter, QueryTripleIter};

/// An evaluated query, retaining encoded solutions until their consumer needs terms.
pub enum TypedResults {
    Boolean(bool),
    Solutions(SolutionTable),
    Graph(Vec<Triple>, Arc<Budget>),
}

/// An ID table and its complete decoding context. Row order and multiplicity are kept.
/// The evaluation's reservation remains live until this result is dropped.
pub struct SolutionTable {
    pub(super) snapshot: Snapshot,
    pub(super) variables: Arc<[Variable]>,
    pub(super) table: IdTable,
    pub(super) computed: Arc<Vec<Term>>,
    pub(super) budget: Arc<Budget>,
    // Comparison/output additions retain their own reservation beside the evaluation.
    extra_budgets: Vec<Arc<Budget>>,
}

impl SolutionTable {
    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// O(columns). Decodes only this row, including query-local computed values.
    pub fn row(&self, row: usize) -> Vec<Option<Term>> {
        (0..self.table.width())
            .map(|column| self.term(self.table.get(row, column)))
            .collect()
    }

    pub fn term(&self, id: u64) -> Option<Term> {
        super::decode(&self.snapshot, &self.computed, id)
    }

    /// Bytes still reserved by this evaluation, including retained intermediate state.
    pub fn reserved_bytes(&self) -> usize {
        self.extra_budgets
            .iter()
            .fold(self.budget.used(), |used, budget| {
                used.saturating_add(budget.used())
            })
    }

    /// Align both results to one dictionary/computed-value domain. O(cells + computed
    /// values); dictionary IDs stay untouched when the views share their dictionary.
    /// Unrelated dictionaries are rejected rather than comparing equal raw integers.
    pub fn align(
        mut self,
        mut upper: Self,
        budget: Arc<Budget>,
    ) -> Result<AlignedSolutions, QueryEvaluationError> {
        if self.variables != upper.variables
            || !self.snapshot.shares_dictionary_with(&upper.snapshot)
        {
            return Err(QueryEvaluationError::Argument(
                "bound results need the same variables and dictionary".to_owned(),
            ));
        }
        let computed_bytes = self
            .computed
            .iter()
            .chain(upper.computed.iter())
            .map(|t| crate::cache::term_heap(t).saturating_add(128))
            .sum::<usize>();
        budget.charge(computed_bytes.saturating_mul(3))?;
        let mut values = std::collections::HashMap::new();
        let mut computed = Vec::new();
        let mut remap = |terms: &[Term]| {
            terms
                .iter()
                .map(|term| {
                    if let Some(id) = upper
                        .snapshot
                        .lookup(term.as_ref())
                        .or_else(|| self.snapshot.lookup(term.as_ref()))
                    {
                        return id.raw();
                    }
                    *values.entry(term.clone()).or_insert_with(|| {
                        let id = computed_id(computed.len() as u64);
                        computed.push(term.clone());
                        id
                    })
                })
                .collect::<Vec<_>>()
        };
        let lower_map = remap(&self.computed);
        let upper_map = remap(&upper.computed);
        fn translate(table: IdTable, map: &[u64]) -> IdTable {
            // Keep zero-column cardinality, and don't lose useful order when no ID changes.
            if table.width() == 0
                || map
                    .iter()
                    .enumerate()
                    .all(|(i, &id)| id == computed_id(i as u64))
            {
                return table;
            }
            let mut columns = table.into_columns();
            for column in &mut columns {
                for id in column {
                    if *id != UNDEF
                        && let Some(i) = computed_index(*id)
                    {
                        *id = map[i as usize];
                    }
                }
            }
            IdTable::from_columns(columns)
        }
        self.table = translate(self.table, &lower_map);
        upper.table = translate(upper.table, &upper_map);
        let computed = Arc::new(computed);
        self.computed = Arc::clone(&computed);
        upper.computed = computed;
        // Upper may know newly interned dictionary terms absent at lower's revision.
        self.snapshot = upper.snapshot.clone();
        self.extra_budgets.push(Arc::clone(&budget));
        Ok(AlignedSolutions {
            lower: self,
            upper,
            budget,
        })
    }
}

/// Two solutions whose raw IDs now have the same meaning. Store owns which rows to
/// decide; this boundary owns translation, decoding and retained result storage.
pub struct AlignedSolutions {
    lower: SolutionTable,
    upper: SolutionTable,
    budget: Arc<Budget>,
}

impl AlignedSolutions {
    pub fn lower(&self) -> &IdTable {
        &self.lower.table
    }
    pub fn upper(&self) -> &IdTable {
        &self.upper.table
    }
    pub fn variables(&self) -> &[Variable] {
        self.lower.variables()
    }
    pub fn term(&self, id: u64) -> Option<Term> {
        self.lower.term(id)
    }
    pub fn upper_row(&self, row: usize) -> Vec<Option<Term>> {
        self.upper.row(row)
    }

    /// Uses the shared ID-table set primitive; only U changes order. Scratch is reserved
    /// before sorting/gathering, including the permutation and deduplication mask.
    pub fn deduplicate_upper(&mut self) -> Result<(), QueryEvaluationError> {
        let scratch = self
            .upper
            .table
            .memory_bytes()
            .saturating_add(self.upper.len().saturating_mul(24));
        self.budget.charge(scratch)?;
        self.upper.table.dedup();
        self.budget.release(scratch);
        Ok(())
    }

    /// Appends a proved candidate once, preserving every original lower row in place.
    pub fn append_upper(&mut self, row: usize) -> Result<(), QueryEvaluationError> {
        // Vec growth temporarily coexists with its old allocation. Reserve the full
        // prospective allocation before push, then release the replaced capacity.
        let (new, old) = self
            .lower
            .table
            .columns()
            .iter()
            .filter(|c| c.len() == c.capacity())
            .fold((0usize, 0usize), |(new, old), c| {
                (
                    new.saturating_add(c.capacity().saturating_mul(2).max(4).saturating_mul(8)),
                    old.saturating_add(c.capacity().saturating_mul(8)),
                )
            });
        self.budget
            .charge(new.saturating_add(self.lower.table.width().saturating_mul(8)))?;
        self.lower.table.push_row(&self.upper.table.row(row));
        self.budget
            .release(old.saturating_add(self.lower.table.width().saturating_mul(8)));
        Ok(())
    }

    pub fn into_lower(self) -> SolutionTable {
        self.lower
    }
}

impl TypedResults {
    /// Decode on delivery, without evaluating the query again.
    pub fn into_results(self) -> QueryResults<'static> {
        match self {
            Self::Boolean(value) => QueryResults::Boolean(value),
            Self::Graph(triples, budget) => QueryResults::Graph(
                QueryTripleIter::new(triples.into_iter().map(Ok)).with_budget(budget),
            ),
            Self::Solutions(solutions) => {
                let variables = Arc::clone(&solutions.variables);
                let rows = (0..solutions.len()).map(move |row| Ok(solutions.row(row)));
                QueryResults::Solutions(QuerySolutionIter::new(variables, rows))
            }
        }
    }
}

/// Evaluates through the same native entry as ordinary result delivery.
pub(crate) fn evaluate(
    snapshot: Snapshot,
    query: &nrese_sparql_syntax::Query,
    options: &crate::QueryOptions,
) -> Result<TypedResults, QueryEvaluationError> {
    use super::{Context, Form, construct, native_pattern, query_base, query_dataset, substitute};

    let ctx = Context::new(&snapshot, options, query_dataset(query), query_base(query));
    let (pattern, form, _, _) = native_pattern(query, options, &ctx)?;
    let pattern = match &options.pre_bound {
        Some(values) => {
            for term in values.values() {
                if let Term::BlankNode(b) = term
                    && let Some(id) = snapshot.lookup(term.as_ref())
                {
                    ctx.register_alias(substitute::alias(b.as_str()).as_str(), id.raw());
                }
            }
            substitute::Values { terms: values }.top(&pattern)
        }
        None => pattern,
    };
    let (_, results) = ctx.on_workers(options.workers.as_ref(), |ctx| {
        let solutions = ctx.eval_root(&pattern, options.pin.as_ref())?;
        ctx.check()?;
        match form {
            Form::Ask => Ok(TypedResults::Boolean(!solutions.table.is_empty())),
            Form::Describe => Ok(TypedResults::Graph(
                ctx.describe(&solutions)?,
                Arc::clone(&ctx.budget),
            )),
            Form::Construct(template) => {
                let triples =
                    construct(&snapshot, ctx.computed.take(), solutions, template, || {
                        ctx.check()
                    })
                    .collect::<super::NativeResult<Vec<_>>>()?;
                Ok(TypedResults::Graph(triples, Arc::clone(&ctx.budget)))
            }
            Form::Select => {
                let computed = ctx.computed.take();
                ctx.budget
                    .charge(
                        computed
                            .capacity()
                            .saturating_mul(std::mem::size_of::<Term>())
                            .saturating_add(computed.iter().map(crate::cache::term_heap).sum()),
                    )
                    .map_err(QueryEvaluationError::MemoryLimit)?;
                Ok(TypedResults::Solutions(SolutionTable {
                    snapshot: snapshot.clone(),
                    variables: solutions.vars.into(),
                    table: solutions.table,
                    computed: Arc::new(computed),
                    budget: Arc::clone(&ctx.budget),
                    extra_budgets: Vec::new(),
                }))
            }
        }
    })?;
    Ok(results)
}
