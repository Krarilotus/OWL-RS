//! Evaluation for consumers that ignore duplicate rows: `SELECT DISTINCT`, and groups whose
//! aggregates are all `DISTINCT`, `MIN`, `MAX` or `SAMPLE`.
//!
//! Two things follow from "duplicates don't count", and both keep intermediate results
//! small where a join multiplies rows that the consumer would merge again.
//!
//! **Sets through the joins** ([`Context::eval_set`]). Every operator keeps only the
//! variables something above it reads, and drops the duplicate rows that leaves, before
//! its result is joined further. `?p :date ?d BIND(YEAR(?d) AS ?y)` with 300 dates in 10
//! years hands 10 rows to the next join, not 300.
//!
//! In a basic graph pattern, a triple pattern whose other variables nothing else reads
//! (`?x :games ?games` when only `?games` is needed) stands for the distinct values of the
//! one it shares, which a group walk over the index lists without reading its matches
//! ([`Context::bgp_set`]): the 51 games of 270 k participations, not the participations.
//!
//! **Aggregates per OPTIONAL branch** ([`Context::group_detached`]). In
//!
//! ```sparql
//! SELECT ?p (COUNT(DISTINCT ?doc) AS ?docs) (GROUP_CONCAT(DISTINCT ?place) AS ?places)
//! WHERE { ?p a :Person OPTIONAL { ?p :in ?doc } OPTIONAL { ?p :at ?place } }
//! GROUP BY ?p
//! ```
//!
//! the two OPTIONALs know nothing of each other, and evaluated as written each person has
//! documents × places rows. An OPTIONAL is *detached* if its variables feed aggregates
//! only (no group key, no other OPTIONAL, and no aggregate that also reads another
//! detached OPTIONAL) and it joins on variables that are bound in every row (on a
//! variable that may be unbound it would bind it, which changes the row). The rest is the
//! *core*. The groups and the core's aggregates come
//! from the core alone; each detached OPTIONAL is joined to the core's distinct rows by
//! itself, and its aggregates are computed from that. A `HAVING` conjunct over group keys
//! and core aggregates is applied before the detached OPTIONALs are read, so they are
//! read for the surviving groups only.
//!
//! Why that is the same result: a left join never removes or changes a row of its left
//! side, so every row of `core ⟕ branch` appears in the full join, extended by the other
//! branches, at least once; and the aggregate ignores how often.
//!
//! Row order: duplicates are dropped keeping first occurrences, so the order-dependent
//! results (`GROUP_CONCAT`, `SAMPLE`, the first of equal `MIN`/`MAX`) come out as they do
//! from the full join.

use nrese_exec::{IdTable, UNDEF, group::group_rows};
use nrese_rdf::Variable;
use nrese_sparql_syntax::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern,
};
use nrese_sparql_syntax::term::TriplePattern;

use super::{
    Agg, Context, NativeResult, Solutions, as_path, bound_variables, expression_variables,
    pushdown, triple_variables,
};

/// The variables each aggregate reads, if none of the aggregates counts duplicate rows.
pub(super) fn insensitive_arguments(
    aggregates: &[(Variable, AggregateExpression)],
) -> Option<Vec<Vec<Variable>>> {
    aggregates
        .iter()
        .map(|(_, aggregate)| match aggregate {
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } if (*distinct
                || matches!(
                    name,
                    AggregateFunction::Min | AggregateFunction::Max | AggregateFunction::Sample
                ))
                && !pushdown::per_solution(expr) =>
            {
                Some(expression_variables(expr))
            }
            _ => None,
        })
        .collect()
}

/// Whether `pattern` combines patterns, so that evaluating it as a set can save work.
pub(super) fn joins(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Join { .. } | GraphPattern::LeftJoin { .. } | GraphPattern::Union { .. } => {
            true
        }
        GraphPattern::Extend { inner, .. } | GraphPattern::Filter { inner, .. } => joins(inner),
        _ => false,
    }
}

fn in_scope(pattern: &GraphPattern) -> Vec<Variable> {
    let mut variables = Vec::new();
    bound_variables(pattern, &mut variables);
    variables
}

fn add(to: &mut Vec<Variable>, variables: impl IntoIterator<Item = Variable>) {
    for variable in variables {
        if !to.contains(&variable) {
            to.push(variable);
        }
    }
}

fn shares(a: &[Variable], b: &[Variable]) -> bool {
    a.iter().any(|v| b.contains(v))
}

/// The group a row belongs to, as a column next to the row's values. No variable of a
/// query can have this name.
fn group_column() -> Variable {
    Variable::new_unchecked("group of the row")
}

impl Context<'_> {
    /// The columns of `solutions` that `needed` names, without duplicate rows (the first
    /// of each stays, in order).
    fn distinct_on(&self, solutions: Solutions, needed: &[Variable]) -> NativeResult<Solutions> {
        let kept: Vec<usize> = (0..solutions.vars.len())
            .filter(|&column| needed.contains(&solutions.vars[column]))
            .collect();
        let rows = solutions.table.len();
        let mut table = if kept.is_empty() {
            // No column left: one row if there was any.
            IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows.min(1)))
        } else if kept.len() == solutions.vars.len() {
            self.consumed(&solutions);
            solutions.table
        } else {
            let projected = solutions.table.project(&kept);
            self.consumed(&solutions);
            projected
        };
        if !kept.is_empty() {
            table.dedup_preserving_order();
        }
        self.produced(Solutions {
            vars: kept.iter().map(|&c| solutions.vars[c].clone()).collect(),
            table,
            ordered: solutions.ordered,
        })
    }

    /// The solutions of `pattern` for a consumer that reads only `needed` and ignores
    /// duplicate rows: those variables' columns, each row once.
    pub(super) fn eval_set(
        &self,
        pattern: &GraphPattern,
        needed: &[Variable],
    ) -> NativeResult<Solutions> {
        let solutions = match pattern {
            GraphPattern::Join { left, right } => {
                let (left_scope, right_scope) = (in_scope(left), in_scope(right));
                let mut keep = needed.to_vec();
                add(
                    &mut keep,
                    left_scope.into_iter().filter(|v| right_scope.contains(v)),
                );
                match (as_path(left), as_path(right)) {
                    (_, Some(path)) => {
                        let bound = self.eval_set(left, &keep)?;
                        let reached = self.path_from(&bound, &path)?;
                        self.join(bound, reached)?
                    }
                    (Some(path), None) => {
                        let bound = self.eval_set(right, &keep)?;
                        let reached = self.path_from(&bound, &path)?;
                        self.join(reached, bound)?
                    }
                    (None, None) => {
                        let (left, right) =
                            (self.eval_set(left, &keep)?, self.eval_set(right, &keep)?);
                        self.join(left, right)?
                    }
                }
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } if !expression.as_ref().is_some_and(pushdown::per_solution) => {
                let (left_scope, right_scope) = (in_scope(left), in_scope(right));
                let mut keep = needed.to_vec();
                add(
                    &mut keep,
                    left_scope.into_iter().filter(|v| right_scope.contains(v)),
                );
                add(&mut keep, expression.iter().flat_map(expression_variables));
                let left = self.eval_set(left, &keep)?;
                let right = match as_path(right) {
                    Some(path) => self.path_from(&left, &path)?,
                    None => self.eval_set(right, &keep)?,
                };
                self.left_join(left, right, expression.as_ref())?
            }
            GraphPattern::Union { left, right } => {
                let (left, right) = (self.eval_set(left, needed)?, self.eval_set(right, needed)?);
                self.union(left, right)?
            }
            // A value per row (RAND, BNODE, ...) differs between rows that are otherwise
            // duplicates: such a BIND or FILTER sees every row.
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } if !pushdown::per_solution(expression) && joins(inner) => {
                if !needed.contains(variable) {
                    // Nothing reads the value, and a BIND neither adds nor removes rows.
                    return self.eval_set(inner, needed);
                }
                let mut keep = needed.to_vec();
                add(&mut keep, expression_variables(expression));
                let solutions = self.eval_set(inner, &keep)?;
                self.extend(solutions, variable, expression)?
            }
            GraphPattern::Filter { expr, inner }
                if !pushdown::per_solution(expr) && joins(inner) =>
            {
                let mut keep = needed.to_vec();
                add(&mut keep, expression_variables(expr));
                let solutions = self.eval_set(inner, &keep)?;
                self.filter(solutions, expr)?
            }
            GraphPattern::Bgp { patterns } if !self.as_written => {
                match self.bgp_set(patterns, needed)? {
                    Some(solutions) => solutions,
                    None => self.eval(pattern)?,
                }
            }
            other => self.eval(other)?,
        };
        self.distinct_on(solutions, needed)
    }

    /// A BGP as a set over `needed`, from one triple pattern that has exactly one variable
    /// something else reads, in the place its index lists next: that pattern gives the
    /// distinct values of the variable by a group walk, and the other patterns join to
    /// them (as matches or as probes). `None` where no pattern qualifies.
    pub(super) fn bgp_set(
        &self,
        patterns: &[TriplePattern],
        needed: &[Variable],
    ) -> NativeResult<Option<Solutions>> {
        // Readers of a variable: the patterns that hold it, and the consumer if it needs it.
        let readers = |v: &Variable| {
            patterns
                .iter()
                .filter(|p| triple_variables(p).contains(v))
                .count()
                + usize::from(needed.contains(v))
        };
        for (i, triple) in patterns.iter().enumerate() {
            let Some(scan) = self.scan_pattern(triple) else {
                continue;
            };
            if scan.merged() || !scan.in_default_graph() || scan.repeats_variable() {
                continue;
            }
            let vars = scan.vars();
            let shared: Vec<&Variable> = vars.iter().filter(|v| readers(v) > 1).collect();
            let ([kept], true) = (shared.as_slice(), vars.len() > 1) else {
                continue;
            };
            let Some(position) = (0..3).find(|&c| scan.slots[c].is_var(kept)) else {
                continue;
            };
            let permutation = scan.permutation_for(Some(kept));
            if scan.first_free(permutation) != Some(position) {
                continue;
            }
            let Some(groups) =
                self.snapshot
                    .group_counts_in(self.model, &scan.quad_pattern(), permutation)
            else {
                continue;
            };
            let mut table = IdTable::new(1);
            for (id, _) in groups {
                table.push_row(&[id.raw()]);
            }
            let values = self.produced(Solutions {
                vars: vec![(*kept).clone()],
                table: table.assume_sorted_by(vec![0]),
                ordered: false,
            })?;
            let rest: Vec<TriplePattern> = patterns
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, p)| p.clone())
                .collect();
            if rest.is_empty() {
                return Ok(Some(values));
            }
            return Ok(Some(self.bgp_from(values, &rest, &mut Vec::new())?));
        }
        Ok(None)
    }

    /// A group whose aggregates ignore duplicates (`arguments`: the variables each reads),
    /// with the OPTIONALs that feed aggregates only evaluated apart from the rest (the
    /// module's documentation says when and why). `None` if no OPTIONAL can be detached.
    /// `having` is the group's `HAVING`: its conjuncts over keys and core aggregates are
    /// applied here (the caller still applies all of it).
    pub(super) fn group_detached(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        arguments: &[Vec<Variable>],
        having: Option<&Expression>,
    ) -> NativeResult<Option<Solutions>> {
        // The chain of OPTIONALs on top of the group's pattern, in the order they apply.
        let mut branches = Vec::new();
        let mut base = inner;
        while let GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } = base
        {
            branches.push((&**right, expression.as_ref()));
            base = left;
        }
        branches.reverse();
        // What each OPTIONAL reads (its pattern's variables and its condition's) and what
        // it introduces.
        let mut scope = in_scope(base);
        // An OPTIONAL that shares a variable which may be unbound on its left binds it
        // where it is: it changes rows, and stays in the core.
        let certain = pushdown::certain(base);
        let (mut reads, mut introduces, mut joins_bound) = (Vec::new(), Vec::new(), Vec::new());
        for (right, expression) in &branches {
            let mut read = in_scope(right);
            let new: Vec<Variable> = read
                .iter()
                .filter(|v| !scope.contains(v))
                .cloned()
                .collect();
            joins_bound.push(read.iter().all(|v| new.contains(v) || certain.contains(v)));
            add(
                &mut read,
                expression.iter().flat_map(|e| expression_variables(e)),
            );
            add(&mut scope, new.iter().cloned());
            reads.push(read);
            introduces.push(new);
        }
        let mut detached: Vec<bool> = (0..branches.len())
            .map(|i| {
                !introduces[i].is_empty()
                    && joins_bound[i]
                    && !shares(&introduces[i], variables)
                    && !branches[i].1.is_some_and(pushdown::per_solution)
                    && (i + 1..branches.len()).all(|j| !shares(&introduces[i], &reads[j]))
            })
            .collect();
        // An aggregate over two detached OPTIONALs needs their product: both stay.
        for argument in arguments {
            let touched: Vec<usize> = (0..branches.len())
                .filter(|&i| detached[i] && shares(&introduces[i], argument))
                .collect();
            if touched.len() > 1 {
                touched.into_iter().for_each(|i| detached[i] = false);
            }
        }
        if !detached.contains(&true) {
            return Ok(None);
        }
        let branch_of = |argument: &[Variable]| {
            (0..branches.len()).find(|&i| detached[i] && shares(&introduces[i], argument))
        };

        // The core: the pattern without the detached OPTIONALs, as a set over the group
        // keys, the core's aggregate arguments, and what the detached OPTIONALs join on.
        let mut core = base.clone();
        for (i, (right, expression)) in branches.iter().enumerate() {
            if !detached[i] {
                core = GraphPattern::LeftJoin {
                    left: Box::new(core),
                    right: Box::new((*right).clone()),
                    expression: expression.cloned(),
                };
            }
        }
        let mut needed = variables.to_vec();
        for (i, read) in reads.iter().enumerate() {
            if detached[i] {
                add(
                    &mut needed,
                    read.iter().filter(|v| !introduces[i].contains(v)).cloned(),
                );
            }
        }
        for argument in arguments {
            let from = branch_of(argument);
            add(
                &mut needed,
                argument
                    .iter()
                    .filter(|v| from.is_none_or(|i| !introduces[i].contains(v)))
                    .cloned(),
            );
        }
        let start = std::time::Instant::now();
        let core = self.eval_set(&core, &needed)?;
        if self.trace.is_some() {
            let detail = format!(
                "the pattern without {} detached OPTIONAL(s), as a set",
                detached.iter().filter(|d| **d).count()
            );
            self.note("group core", detail, None, core.table.len(), start);
        }

        // The groups, from the core.
        let key_columns: Vec<Vec<u64>> = variables
            .iter()
            .map(|v| match core.column(v) {
                Some(column) => core.table.column(column).to_vec(),
                None => vec![UNDEF; core.table.len()],
            })
            .collect();
        let key_table = IdTable::from_columns(key_columns);
        let groups = group_rows(&key_table, &(0..variables.len()).collect::<Vec<_>>());
        let group_count = groups.len();
        let members_of = |group_of: &mut dyn Iterator<Item = usize>| {
            let mut members: Vec<Vec<usize>> = vec![Vec::new(); group_count];
            for (row, group) in group_of.enumerate() {
                members[group].push(row);
            }
            members
        };
        // Aggregates in the caller's order; each column is filled by the part that
        // computes it.
        let mut values: Vec<Option<Vec<u64>>> = vec![None; aggregates.len()];
        let compute = |solutions: &Solutions,
                       members: &[Vec<usize>],
                       selected: &[usize],
                       values: &mut Vec<Option<Vec<u64>>>|
         -> NativeResult<()> {
            let chosen: Vec<(Variable, AggregateExpression)> =
                selected.iter().map(|&a| aggregates[a].clone()).collect();
            let computed = self.aggregate_groups(solutions, members, &chosen)?;
            for (position, &a) in selected.iter().enumerate() {
                values[a] = Some(
                    computed
                        .iter()
                        .map(|group| match &group[position] {
                            Agg::Id(id) => *id,
                            Agg::Term(term) => self.id(term),
                        })
                        .collect(),
                );
            }
            Ok(())
        };
        let core_aggregates: Vec<usize> = (0..aggregates.len())
            .filter(|&a| branch_of(&arguments[a]).is_none())
            .collect();
        let members = members_of(&mut groups.group_of.iter().map(|&g| g as usize));
        let start = std::time::Instant::now();
        compute(&core, &members, &core_aggregates, &mut values)?;
        if self.trace.is_some() {
            let detail = format!("{} aggregate(s) of the core", core_aggregates.len());
            self.note("aggregate", detail, None, group_count, start);
        }

        // HAVING over what is known by now drops groups before the OPTIONALs are read.
        let mut keep = vec![true; group_count];
        if let Some(having) = having {
            let mut vars = variables.to_vec();
            let mut columns = groups.keys.clone().into_columns();
            for &a in &core_aggregates {
                vars.push(aggregates[a].0.clone());
                columns.push(values[a].clone().unwrap_or_default());
            }
            if !columns.is_empty() {
                let known = Solutions {
                    vars,
                    table: IdTable::from_columns(columns),
                    ordered: false,
                };
                let mut conjuncts = Vec::new();
                pushdown::conjuncts_of(having, &mut conjuncts);
                for conjunct in conjuncts {
                    let reads = expression_variables(conjunct);
                    if pushdown::movable(conjunct) && reads.iter().all(|v| known.vars.contains(v)) {
                        let mask = self.filter_mask(&known, conjunct)?;
                        keep.iter_mut()
                            .zip(mask)
                            .for_each(|(keep, pass)| *keep &= pass);
                    }
                }
            }
        }

        // Each detached OPTIONAL with aggregates: the distinct rows of the kept groups
        // it joins on, left-joined with it, aggregated per group.
        let kept_groups = keep.iter().filter(|k| **k).count();
        let group = group_column();
        for (i, (right, expression)) in branches.iter().enumerate() {
            let selected: Vec<usize> = (0..aggregates.len())
                .filter(|&a| detached[i] && branch_of(&arguments[a]) == Some(i))
                .collect();
            if selected.is_empty() {
                continue;
            }
            let mut from_core: Vec<Variable> = Vec::new();
            add(
                &mut from_core,
                reads[i]
                    .iter()
                    .filter(|v| core.column(v).is_some())
                    .cloned(),
            );
            for &a in &selected {
                add(
                    &mut from_core,
                    arguments[a]
                        .iter()
                        .filter(|v| core.column(v).is_some())
                        .cloned(),
                );
            }
            let rows: Vec<usize> = (0..core.table.len())
                .filter(|&row| keep[groups.group_of[row] as usize])
                .collect();
            let mut columns = vec![
                rows.iter()
                    .map(|&row| u64::from(groups.group_of[row]))
                    .collect::<Vec<u64>>(),
            ];
            for variable in &from_core {
                let column = core
                    .table
                    .column(core.column(variable).expect("filtered above"));
                columns.push(rows.iter().map(|&row| column[row]).collect());
            }
            // Data closed under sameAs: a plain pattern gives every identity of a key the
            // same rows, so the join, which feeds duplicate-insensitive aggregates only,
            // runs on one representative per identity class (`equality`).
            let by_representative: Vec<Variable> = match self.representatives() {
                Some(_) if expression.is_none() && matches!(right, GraphPattern::Bgp { .. }) => {
                    from_core
                        .iter()
                        .filter(|v| reads[i].contains(v))
                        .filter(|v| selected.iter().all(|&a| !arguments[a].contains(v)))
                        .cloned()
                        .collect()
                }
                _ => Vec::new(),
            };
            let representatives = self.representatives();
            let to_representatives = |vars: &[Variable], columns: &mut [Vec<u64>]| {
                if let Some(representatives) = &representatives {
                    for (variable, column) in vars.iter().zip(columns.iter_mut()) {
                        if by_representative.contains(variable) {
                            for id in column.iter_mut() {
                                *id = super::equality::representative(representatives, *id);
                            }
                        }
                    }
                }
            };
            let mut vars = vec![group.clone()];
            vars.extend(from_core.iter().cloned());
            to_representatives(&vars, &mut columns);
            let mut table = IdTable::from_columns(columns);
            table.dedup_preserving_order();
            let left = self.produced(Solutions {
                vars,
                table,
                ordered: false,
            })?;
            let mut right_needed = from_core.clone();
            add(&mut right_needed, reads[i].iter().cloned());
            for &a in &selected {
                add(&mut right_needed, arguments[a].iter().cloned());
            }
            let mut right = match as_path(right) {
                Some(path) => self.path_from(&left, &path)?,
                None => self.eval_set(right, &right_needed)?,
            };
            if !by_representative.is_empty() {
                let mut columns = std::mem::take(&mut right.table).into_columns();
                to_representatives(&right.vars, &mut columns);
                right.table = IdTable::from_columns(columns);
                right.table.dedup_preserving_order();
            }
            let start = std::time::Instant::now();
            let joined = self.left_join(left, right, *expression)?;
            let column = joined.column(&group).expect("the left side's first column");
            let members = members_of(&mut joined.table.column(column).iter().map(|&g| g as usize));
            if self.trace.is_some() {
                let detail = format!(
                    "a detached OPTIONAL joined to {} kept group(s)",
                    kept_groups
                );
                self.note("optional", detail, None, joined.table.len(), start);
            }
            let start = std::time::Instant::now();
            compute(&joined, &members, &selected, &mut values)?;
            if self.trace.is_some() {
                let detail = format!("{} aggregate(s) of the detached OPTIONAL", selected.len());
                self.note("aggregate", detail, None, kept_groups, start);
            }
            self.consumed(&joined);
        }

        // Detached OPTIONALs without aggregates contribute nothing. The kept groups, with
        // the aggregates in the caller's order.
        let kept: Vec<usize> = (0..group_count).filter(|&g| keep[g]).collect();
        let mut columns: Vec<Vec<u64>> = groups
            .keys
            .columns()
            .iter()
            .map(|column| kept.iter().map(|&g| column[g]).collect())
            .collect();
        let mut vars = variables.to_vec();
        for (a, (target, _)) in aggregates.iter().enumerate() {
            let column = values[a].take().unwrap_or_else(|| vec![UNDEF; group_count]);
            columns.push(kept.iter().map(|&g| column[g]).collect());
            vars.push(target.clone());
        }
        self.consumed(&core);
        self.produced(Solutions {
            vars,
            table: IdTable::from_columns(columns),
            ordered: false,
        })
        .map(Some)
    }
}
