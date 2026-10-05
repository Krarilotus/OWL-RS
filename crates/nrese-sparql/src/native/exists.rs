//! `EXISTS` and `NOT EXISTS` over any pattern, anywhere in an expression.
//!
//! `EXISTS { P }` is true for a solution μ if P, with μ's values put in for its variables,
//! has a solution (SPARQL 1.1 §18.6). P is evaluated once for all solutions, not once per
//! solution:
//!
//! - **Uncorrelated.** Where P reads μ only through variables its own patterns bind, μ
//!   agrees with one of P's solutions on the variables they share: a semi-join (UNDEF
//!   compatible with any value).
//! - **Correlated filters.** Filters at the top of P may read other variables of μ
//!   (`FILTER EXISTS { ?y :age ?b FILTER(?b > ?a) }`). P without them is joined to μ's
//!   distinct values of the variables involved, and the filters run on the joined rows.
//! - Deeper correlation, where μ's value would change what P computes (a variable of μ
//!   read inside an OPTIONAL, MINUS or BIND of P that P doesn't bind there, or a subquery
//!   with LIMIT that sees it), is left to the general evaluator: [`correlation_safe`]
//!   decides that before the query starts.
//!
//! An `EXISTS` nested in an expression (`FILTER(?x || EXISTS {…})`, `BIND(EXISTS {…} AS
//! ?b)`, an OPTIONAL's condition) becomes a boolean column that the expression reads in
//! its place ([`Context::with_exists`]).

use std::collections::HashMap;

use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression,
};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern};

use nrese_exec::join::{
    anti_join_in_place, compatible_mask, outer_join_with_undef, semi_join_in_place,
};
use nrese_exec::{IdTable, UNDEF};

use super::{
    Context, GraphScope, NativeResult, Solutions, bound_variables, expr, has_undef, pushdown,
    shared_columns, supported,
};

/// Whether the native executor evaluates `expression` on solutions of `outer` (the
/// variables in scope where it stands), `EXISTS` included.
pub(super) fn supported_expression(expression: &Expression, outer: &[Variable]) -> bool {
    let mut patterns = Vec::new();
    let plain = split(expression, &mut patterns, &mut |n| {
        Variable::new_unchecked(format!("exists {n}"))
    });
    let _ = outer;
    expr::supported(&plain) && patterns.iter().all(|(_, pattern)| supported(pattern))
}

/// The conjuncts of the filters at the top of `pattern`, and what they filter.
fn top_filters(pattern: &GraphPattern) -> (Vec<&Expression>, &GraphPattern) {
    let mut conjuncts = Vec::new();
    let mut inner = pattern;
    while let GraphPattern::Filter { expr, inner: next } = inner {
        pushdown::conjuncts_of(expr, &mut conjuncts);
        inner = next;
    }
    (conjuncts, inner)
}

/// Whether evaluating `pattern` once, and joining its solutions to the outer ones on the
/// variables they share, gives what evaluating it with each outer solution's values put in
/// gives. It does unless a variable of `outer` is read somewhere `pattern` doesn't bind it
/// first: then the outer value would change what `pattern` computes there.
pub(super) fn correlation_safe(pattern: &GraphPattern, outer: &[Variable]) -> bool {
    // Every variable of `outer` among `read` is bound by the pattern that `bound` binds.
    let covered = |read: &[Variable], bound: &GraphPattern| {
        let certain = pushdown::certain(bound);
        read.iter()
            .all(|v| !outer.contains(v) || certain.contains(v))
    };
    match pattern {
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } | GraphPattern::Values { .. } => true,
        GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
            correlation_safe(left, outer) && correlation_safe(right, outer)
        }
        GraphPattern::Graph { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::OrderBy { inner, .. } => correlation_safe(inner, outer),
        GraphPattern::Filter { expr, inner } => {
            correlation_safe(inner, outer) && covered(&deep_variables(expr), inner)
        }
        GraphPattern::Extend {
            inner, expression, ..
        } => correlation_safe(inner, outer) && covered(&deep_variables(expression), inner),
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            let mut read = mentioned(right);
            if let Some(expression) = expression {
                read.extend(deep_variables(expression));
            }
            correlation_safe(left, outer) && correlation_safe(right, outer) && covered(&read, left)
        }
        GraphPattern::Minus { left, right } => {
            correlation_safe(left, outer)
                && correlation_safe(right, outer)
                && covered(&mentioned(right), left)
        }
        // A subquery sees only the variables it projects.
        GraphPattern::Project { inner, variables } => {
            let visible: Vec<Variable> = outer
                .iter()
                .filter(|v| variables.contains(v))
                .cloned()
                .collect();
            correlation_safe(inner, &visible)
        }
        // A LIMIT would cut the rows before the join selects the outer values.
        GraphPattern::Slice { inner, .. } => {
            let mut in_scope = Vec::new();
            bound_variables(inner, &mut in_scope);
            in_scope.iter().all(|v| !outer.contains(v)) && correlation_safe(inner, outer)
        }
        // Groups are formed per key: an outer key value selects its group, if the rows
        // bind the key.
        GraphPattern::Group {
            inner, variables, ..
        } => {
            let keys: Vec<Variable> = outer
                .iter()
                .filter(|v| variables.contains(v))
                .cloned()
                .collect();
            covered(&keys, inner) && correlation_safe(inner, &[])
        }
        _ => false,
    }
}

/// Every variable `pattern` mentions where the outside can see it (a subquery's own
/// variables only if it projects them).
pub(super) fn mentioned(pattern: &GraphPattern) -> Vec<Variable> {
    let mut out = Vec::new();
    walk_pattern(pattern, &mut out);
    out
}

fn push(out: &mut Vec<Variable>, variable: &Variable) {
    if !out.contains(variable) {
        out.push(variable.clone());
    }
}

fn walk_term(term: &TermPattern, out: &mut Vec<Variable>) {
    if let TermPattern::Variable(v) = term {
        push(out, v);
    }
}

fn walk_pattern(pattern: &GraphPattern, out: &mut Vec<Variable>) {
    match pattern {
        GraphPattern::Bgp { patterns } => {
            for triple in patterns {
                walk_term(&triple.subject, out);
                if let NamedNodePattern::Variable(v) = &triple.predicate {
                    push(out, v);
                }
                walk_term(&triple.object, out);
            }
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            walk_term(subject, out);
            walk_term(object, out);
        }
        GraphPattern::Graph { name, inner } => {
            if let NamedNodePattern::Variable(v) = name {
                push(out, v);
            }
            walk_pattern(inner, out);
        }
        GraphPattern::Join { left, right }
        | GraphPattern::Lateral { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            walk_pattern(left, out);
            walk_pattern(right, out);
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            walk_pattern(left, out);
            walk_pattern(right, out);
            if let Some(expression) = expression {
                walk_expression(expression, out);
            }
        }
        GraphPattern::Filter { expr, inner } => {
            walk_pattern(inner, out);
            walk_expression(expr, out);
        }
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => {
            walk_pattern(inner, out);
            push(out, variable);
            walk_expression(expression, out);
        }
        GraphPattern::Values { variables, .. } => {
            for v in variables {
                push(out, v);
            }
        }
        GraphPattern::OrderBy { inner, expression } => {
            walk_pattern(inner, out);
            for key in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = key;
                walk_expression(e, out);
            }
        }
        GraphPattern::Project { variables, .. } => {
            for v in variables {
                push(out, v);
            }
        }
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => walk_pattern(inner, out),
        GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } => {
            walk_pattern(inner, out);
            for v in variables {
                push(out, v);
            }
            for (v, aggregate) in aggregates {
                push(out, v);
                if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                    walk_expression(expr, out);
                }
            }
        }
        GraphPattern::Service { inner, name, .. } => {
            if let NamedNodePattern::Variable(v) = name {
                push(out, v);
            }
            walk_pattern(inner, out);
        }
    }
}

/// The variables `expression` reads, those of its `EXISTS` patterns included.
pub(super) fn deep_variables(expression: &Expression) -> Vec<Variable> {
    let mut out = Vec::new();
    walk_expression(expression, &mut out);
    out
}

fn walk_expression(expression: &Expression, out: &mut Vec<Variable>) {
    match expression {
        Expression::Variable(v) | Expression::Bound(v) => push(out, v),
        Expression::NamedNode(_) | Expression::Literal(_) => {}
        Expression::Exists(pattern) => walk_pattern(pattern, out),
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            walk_expression(a, out);
            walk_expression(b, out);
        }
        Expression::In(a, list) => {
            walk_expression(a, out);
            list.iter().for_each(|e| walk_expression(e, out));
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            walk_expression(a, out);
        }
        Expression::If(a, b, c) => {
            walk_expression(a, out);
            walk_expression(b, out);
            walk_expression(c, out);
        }
        Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
            list.iter().for_each(|e| walk_expression(e, out));
        }
    }
}

/// `expression` with each `EXISTS` replaced by the variable `fresh` names, and the patterns
/// with their variables.
fn split(
    expression: &Expression,
    patterns: &mut Vec<(Variable, GraphPattern)>,
    fresh: &mut dyn FnMut(usize) -> Variable,
) -> Expression {
    let mut go = |e: &Expression| split(e, patterns, fresh);
    let mut boxed = |e: &Expression| Box::new(go(e));
    match expression {
        Expression::Exists(pattern) => {
            let variable = fresh(patterns.len());
            patterns.push((variable.clone(), (**pattern).clone()));
            Expression::Variable(variable)
        }
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Variable(_)
        | Expression::Bound(_) => expression.clone(),
        Expression::Or(a, b) => Expression::Or(boxed(a), boxed(b)),
        Expression::And(a, b) => Expression::And(boxed(a), boxed(b)),
        Expression::Equal(a, b) => Expression::Equal(boxed(a), boxed(b)),
        Expression::SameTerm(a, b) => Expression::SameTerm(boxed(a), boxed(b)),
        Expression::Greater(a, b) => Expression::Greater(boxed(a), boxed(b)),
        Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(boxed(a), boxed(b)),
        Expression::Less(a, b) => Expression::Less(boxed(a), boxed(b)),
        Expression::LessOrEqual(a, b) => Expression::LessOrEqual(boxed(a), boxed(b)),
        Expression::Add(a, b) => Expression::Add(boxed(a), boxed(b)),
        Expression::Subtract(a, b) => Expression::Subtract(boxed(a), boxed(b)),
        Expression::Multiply(a, b) => Expression::Multiply(boxed(a), boxed(b)),
        Expression::Divide(a, b) => Expression::Divide(boxed(a), boxed(b)),
        Expression::UnaryPlus(a) => Expression::UnaryPlus(boxed(a)),
        Expression::UnaryMinus(a) => Expression::UnaryMinus(boxed(a)),
        Expression::Not(a) => Expression::Not(boxed(a)),
        Expression::If(a, b, c) => Expression::If(boxed(a), boxed(b), boxed(c)),
        Expression::In(a, list) => {
            let a = boxed(a);
            Expression::In(a, list.iter().map(|e| *boxed(e)).collect())
        }
        Expression::Coalesce(list) => {
            Expression::Coalesce(list.iter().map(|e| *boxed(e)).collect())
        }
        Expression::FunctionCall(function, list) => {
            Expression::FunctionCall(function.clone(), list.iter().map(|e| *boxed(e)).collect())
        }
    }
}

impl Context<'_> {
    /// A variable no query can name, for a column the executor adds for itself.
    fn synthetic(&self, what: &str) -> Variable {
        let n = self.synthetic.get();
        self.synthetic.set(n + 1);
        Variable::new_unchecked(format!("{what} {n}"))
    }

    /// `solutions` with a boolean column for each `EXISTS` in `expression`, and the
    /// expression reading those columns instead; the columns are the last `count`.
    pub(super) fn with_exists(
        &self,
        mut solutions: Solutions,
        expression: &Expression,
    ) -> NativeResult<(Solutions, Expression, usize)> {
        let mut patterns = Vec::new();
        let plain = split(expression, &mut patterns, &mut |_| self.synthetic("exists"));
        let yes = self.id(&Term::from(Literal::from(true)));
        let no = self.id(&Term::from(Literal::from(false)));
        for (variable, pattern) in &patterns {
            let mask = self.exists_mask(&solutions, pattern)?;
            let mut columns = std::mem::take(&mut solutions.table).into_columns();
            columns.push(mask.iter().map(|&m| if m { yes } else { no }).collect());
            solutions.table = IdTable::from_columns(columns);
            solutions.vars.push(variable.clone());
        }
        Ok((solutions, plain, patterns.len()))
    }

    /// Keeps the rows for which `EXISTS { pattern }` is `keep_matching`.
    pub(super) fn exists(
        &self,
        mut solutions: Solutions,
        pattern: &GraphPattern,
        keep_matching: bool,
    ) -> NativeResult<Solutions> {
        let start = std::time::Instant::now();
        let input = solutions.table.len();
        // EXPLAIN: how the EXISTS ran, with the filter's estimate (all rows for EXISTS,
        // nine in ten for NOT EXISTS, as `pushdown::selectivity` has it).
        let note = |operator: &str, detail: String, rows: usize| {
            if self.trace.is_some() {
                let kept = if keep_matching { 1.0 } else { 0.9 };
                let estimate = Some((input as f64 * kept).round() as u64);
                self.note(operator, detail, estimate, rows, start);
            }
        };
        let joined = if keep_matching {
            "semi join"
        } else {
            "anti join"
        };
        let (conjuncts, inner) = top_filters(pattern);
        let correlated = conjuncts
            .iter()
            .any(|c| !self.local(c, pattern) && self.reads_outer(c, &solutions));
        let mask = if correlated {
            self.exists_mask(&solutions, pattern)?
        } else {
            if !correlation_safe(inner, &solutions.vars) {
                let mask = self.exists_substituted(&solutions, pattern)?;
                solutions
                    .table
                    .retain_mask(&mask.iter().map(|m| *m == keep_matching).collect::<Vec<_>>());
                let detail = "the pattern per distinct value of the row it reads".to_owned();
                note("exists per value", detail, solutions.table.len());
                return Ok(solutions);
            }
            // One evaluation, one semi- or anti-join. Only the shared variables' distinct
            // values matter: the pattern as a set over them (`sets`), which can make a
            // pattern a sorted group walk of its shared variable.
            let mut shared: Vec<Variable> = Vec::new();
            bound_variables(pattern, &mut shared);
            shared.retain(|v| solutions.column(v).is_some());
            // In GRAPH ?g the graph is a shared variable too, which the set would drop.
            let in_default = matches!(*self.graph.borrow(), GraphScope::Default);
            let found = if self.as_written || shared.is_empty() || !in_default {
                self.eval(pattern)?
            } else {
                self.eval_set(pattern, &shared)?
            };
            let (lk, rk) = shared_columns(&solutions, &found);
            let names: Vec<String> = lk.iter().map(|&c| solutions.vars[c].to_string()).collect();
            let detail = format!(
                "EXISTS once, as a set of {} row(s) over {}",
                found.table.len(),
                names.join(" ")
            );
            if !has_undef(&solutions.table, &lk) && !has_undef(&found.table, &rk) {
                match keep_matching {
                    true => semi_join_in_place(&mut solutions.table, &found.table, &lk, &rk),
                    false => anti_join_in_place(&mut solutions.table, &found.table, &lk, &rk),
                }
                self.consumed(&found);
                note(joined, detail, solutions.table.len());
                return Ok(solutions);
            }
            // An unbound variable isn't put into the pattern: any value matches.
            let mask = compatible_mask(&solutions.table, &found.table, &lk, &rk, false);
            self.consumed(&found);
            let kept = mask.iter().filter(|m| **m == keep_matching).count();
            note(joined, detail, kept);
            mask
        };
        if correlated {
            let kept = mask.iter().filter(|m| **m == keep_matching).count();
            let detail = "the pattern once, joined to the rows' distinct values".to_owned();
            note("correlated exists", detail, kept);
        }
        solutions
            .table
            .retain_mask(&mask.iter().map(|m| *m == keep_matching).collect::<Vec<_>>());
        Ok(solutions)
    }

    /// Whether a top filter conjunct of `pattern` reads only variables that the filtered
    /// pattern binds: it can run before the join with the outer solutions.
    fn local(&self, conjunct: &Expression, pattern: &GraphPattern) -> bool {
        let (_, inner) = top_filters(pattern);
        let certain = pushdown::certain(inner);
        deep_variables(conjunct).iter().all(|v| certain.contains(v))
    }

    fn reads_outer(&self, conjunct: &Expression, solutions: &Solutions) -> bool {
        deep_variables(conjunct)
            .iter()
            .any(|v| solutions.column(v).is_some())
    }

    /// Per row of `solutions`, whether `EXISTS { pattern }` holds, by substitution
    /// ([`super::substitute`]): the pattern with the row's values put in, evaluated once
    /// per distinct value of the outer variables it mentions.
    fn exists_substituted(
        &self,
        solutions: &Solutions,
        pattern: &GraphPattern,
    ) -> NativeResult<Vec<bool>> {
        let visible = mentioned(pattern);
        let columns: Vec<(usize, Variable)> = solutions
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| visible.contains(v))
            .map(|(i, v)| (i, v.clone()))
            .collect();
        // Under `GRAPH ?g` evaluated for all graphs at once, each row's graph is its `?g`:
        // the pattern holds or not in that graph, not in any.
        let graph_column = match &*self.graph.borrow() {
            GraphScope::Variable(v) => solutions.column(v),
            _ => None,
        };
        let mut answers: HashMap<Vec<u64>, bool> = HashMap::new();
        let mut mask = Vec::with_capacity(solutions.table.len());
        for row in 0..solutions.table.len() {
            self.check()?;
            let mut key: Vec<u64> = columns
                .iter()
                .map(|(c, _)| solutions.table.get(row, *c))
                .collect();
            let graph = graph_column
                .map(|c| solutions.table.get(row, c))
                .filter(|&id| id != UNDEF);
            key.push(graph.unwrap_or(UNDEF));
            if let Some(&holds) = answers.get(&key) {
                mask.push(holds);
                continue;
            }
            let mut terms = HashMap::new();
            for ((_, variable), &id) in columns.iter().zip(&key) {
                if id == UNDEF {
                    continue;
                }
                if let Some(term) = self.term(id) {
                    if let Term::BlankNode(b) = &term {
                        self.register_alias(super::substitute::alias(b.as_str()).as_str(), id);
                    }
                    terms.insert(variable.clone(), term);
                }
            }
            let substituted = super::substitute::Values { terms: &terms }.pattern(pattern);
            let found = match graph {
                Some(id) => self.in_graph(
                    GraphScope::Named(nrese_engine::TermId::from_raw(id)),
                    &substituted,
                )?,
                None => self.eval(&substituted)?,
            };
            let holds = !found.table.is_empty();
            self.consumed(&found);
            answers.insert(key, holds);
            mask.push(holds);
        }
        Ok(mask)
    }

    /// Per row of `solutions`, whether `EXISTS { pattern }` holds.
    pub(super) fn exists_mask(
        &self,
        solutions: &Solutions,
        pattern: &GraphPattern,
    ) -> NativeResult<Vec<bool>> {
        if solutions.table.is_empty() {
            return Ok(Vec::new());
        }
        let (conjuncts, inner) = top_filters(pattern);
        if !correlation_safe(inner, &solutions.vars) {
            return self.exists_substituted(solutions, pattern);
        }
        let (local, correlated): (Vec<&Expression>, Vec<&Expression>) = conjuncts
            .into_iter()
            .partition(|c| self.local(c, pattern) || !self.reads_outer(c, solutions));
        if correlated.is_empty() {
            let found = self.eval(pattern)?;
            let (lk, rk) = shared_columns(solutions, &found);
            let mask = compatible_mask(&solutions.table, &found.table, &lk, &rk, false);
            self.consumed(&found);
            return Ok(mask);
        }
        let mut found = self.eval(inner)?;
        for conjunct in local {
            found = self.filter(found, conjunct)?;
        }
        // The outer values the pattern and the correlated filters read, each set once.
        let mut read = Vec::new();
        for conjunct in &correlated {
            for v in deep_variables(conjunct) {
                push(&mut read, &v);
            }
        }
        let keys: Vec<usize> = solutions
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| found.column(v).is_some() || read.contains(v))
            .map(|(i, _)| i)
            .collect();
        let mut index: HashMap<Vec<u64>, u64> = HashMap::new();
        let mut key_of_row = Vec::with_capacity(solutions.table.len());
        let mut distinct = IdTable::new(keys.len() + 1);
        let mut row = vec![0u64; keys.len() + 1];
        for r in 0..solutions.table.len() {
            let key: Vec<u64> = keys.iter().map(|&c| solutions.table.get(r, c)).collect();
            let next = index.len() as u64;
            let id = *index.entry(key).or_insert_with_key(|key| {
                row[..keys.len()].copy_from_slice(key);
                row[keys.len()] = next;
                distinct.push_row(&row);
                next
            });
            key_of_row.push(id);
        }
        let key_variable = self.synthetic("exists key");
        let mut vars: Vec<Variable> = keys.iter().map(|&c| solutions.vars[c].clone()).collect();
        vars.push(key_variable.clone());
        let distinct = self.produced(Solutions {
            vars,
            table: distinct,
            ordered: false,
        })?;
        let mut joined = self.join(distinct, found)?;
        for conjunct in correlated {
            joined = self.filter(joined, conjunct)?;
        }
        let column = joined.column(&key_variable).expect("the key column");
        let mut holds = vec![false; index.len()];
        for &id in joined.table.column(column) {
            holds[id as usize] = true;
        }
        self.consumed(&joined);
        Ok(key_of_row.iter().map(|&id| holds[id as usize]).collect())
    }

    /// `OPTIONAL { right } FILTER(expression)` where the condition holds an `EXISTS`: the
    /// compatible pairs, the condition (with its `EXISTS` evaluated on each pair) keeps
    /// some, and a left row none is kept for appears once with the right side unbound.
    pub(super) fn left_join_with_exists(
        &self,
        left: Solutions,
        right: Solutions,
        expression: &Expression,
    ) -> NativeResult<Solutions> {
        let ordered = left.ordered;
        let rows = left.table.len();
        let row_variable = self.synthetic("left row");
        let mut numbered = left;
        let mut columns = std::mem::take(&mut numbered.table).into_columns();
        columns.push((0..rows as u64).collect());
        numbered.table = IdTable::from_columns(columns);
        numbered.vars.push(row_variable.clone());
        let (lk, rk) = shared_columns(&numbered, &right);
        let vars = super::joined_vars(&numbered, &right, &rk);
        let max_rows = self.row_limit(vars.len());
        let pairs = outer_join_with_undef(
            &numbered.table,
            &right.table,
            &lk,
            &rk,
            None,
            false,
            max_rows,
        )
        .map_err(|e| self.too_large(e.max_rows.saturating_add(1), vars.len()))?;
        self.consumed(&right);
        let pairs = self.produced(Solutions {
            vars,
            table: pairs,
            ordered: false,
        })?;
        let (mut pairs, plain, added) = self.with_exists(pairs, expression)?;
        let mask = self.filter_mask(&pairs, &plain)?;
        pairs.table.retain_mask(&mask);
        // The pairs' columns: the left's, the row number, the right's payload, then the
        // EXISTS columns; the output drops the last three kinds' extras.
        let row_column = numbered.vars.len() - 1;
        let width = pairs.vars.len() - added;
        let mut matched = vec![false; rows];
        for &r in pairs.table.column(row_column) {
            matched[r as usize] = true;
        }
        // The kept pairs, in left order, with the left rows none was kept for in between.
        let mut out = IdTable::new(width - 1);
        let mut line = vec![UNDEF; width - 1];
        let mut next = 0;
        let mut unmatched_before = |upto: usize, out: &mut IdTable, line: &mut [u64]| {
            while next < upto {
                if !matched[next] {
                    line.fill(UNDEF);
                    for (c, value) in line.iter_mut().enumerate().take(row_column) {
                        *value = numbered.table.get(next, c);
                    }
                    out.push_row(line);
                }
                next += 1;
            }
        };
        for p in 0..pairs.table.len() {
            unmatched_before(pairs.table.get(p, row_column) as usize, &mut out, &mut line);
            for (c, value) in line.iter_mut().enumerate() {
                let source = if c < row_column { c } else { c + 1 };
                *value = pairs.table.get(p, source);
            }
            out.push_row(&line);
        }
        unmatched_before(rows, &mut out, &mut line);
        self.consumed(&pairs);
        self.consumed(&numbered);
        let vars: Vec<Variable> = pairs.vars[..width]
            .iter()
            .filter(|v| **v != row_variable)
            .cloned()
            .collect();
        debug_assert_eq!(vars.len(), width - 1);
        self.produced(Solutions {
            vars,
            table: out,
            ordered,
        })
    }
}
