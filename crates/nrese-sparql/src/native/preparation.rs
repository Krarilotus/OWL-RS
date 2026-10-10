//! Semantic query preparation, shared by execution, reporting and EXPLAIN.

#[cfg(test)]
#[path = "preparation_tests.rs"]
mod tests;

use super::{
    CancellationToken, Cow, Form, GraphPattern, Query, QueryEvaluationError, QueryOptions,
    ReadModel, Snapshot, pattern_and_form, ql, query_dataset, triple_terms,
};

#[cfg(test)]
thread_local! { static QL_STAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// The QL algebra and the report that goes with it, without planning or evaluating.
pub(crate) fn prepare_ql_query<'q>(
    snapshot: &Snapshot,
    query: &'q Query,
    options: &QueryOptions,
) -> Result<(Cow<'q, Query>, Option<crate::ql::QlReport>), QueryEvaluationError> {
    preparation_alive(options)?;
    if ql_rewriting(query, options).is_none() {
        return Ok((Cow::Borrowed(query), None));
    }
    let canonical;
    let snapshot = if options.equality_canonical {
        canonical = snapshot.with_canonical_equality();
        &canonical
    } else {
        snapshot
    };
    let (pattern, report) = semantic_pattern(snapshot, query, options, &mut Vec::new())?;
    let Cow::Owned(pattern) = pattern else {
        return Ok((Cow::Borrowed(query), report));
    };
    let dataset = query.dataset().cloned();
    let base_iri = query.base_iri().cloned();
    let query = match query {
        Query::Select { .. } => Query::Select {
            dataset,
            pattern,
            base_iri,
        },
        Query::Ask { .. } => Query::Ask {
            dataset,
            pattern,
            base_iri,
        },
        Query::Describe { .. } => Query::Describe {
            dataset,
            pattern,
            base_iri,
        },
        Query::Construct { template, .. } => Query::Construct {
            template: template.clone(),
            dataset,
            pattern,
            base_iri,
        },
    };
    Ok((Cow::Owned(query), report))
}

pub(super) fn preparation_alive(options: &QueryOptions) -> Result<(), QueryEvaluationError> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        Err(QueryEvaluationError::Cancelled)
    } else {
        Ok(())
    }
}

/// Semantic rewrites shared by preparation and standalone execution/EXPLAIN.
pub(super) fn semantic_pattern<'q>(
    snapshot: &Snapshot,
    query: &'q Query,
    options: &QueryOptions,
    rewrites: &mut Vec<&'static str>,
) -> Result<(Cow<'q, GraphPattern>, Option<crate::ql::QlReport>), QueryEvaluationError> {
    preparation_alive(options)?;
    let (pattern, form) = pattern_and_form(query);
    let pattern = if triple_terms::has_open(pattern) {
        rewrites.push("triple-terms");
        Cow::Owned(triple_terms::rewrite(pattern))
    } else {
        Cow::Borrowed(pattern)
    };
    let (rewritten, report) = ql_stage(query, options, snapshot, &pattern, &form);
    preparation_alive(options)?;
    if let Some(report) = &report {
        if report.patterns > 0 {
            rewrites.push("ql-tree-witness");
        }
        if !report.limits.is_empty() {
            rewrites.push("ql-limit");
        }
    }
    Ok((rewritten.map(Cow::Owned).unwrap_or(pattern), report))
}

/// The QL rewriting of a query's pattern: the pattern rewritten (`None` if unchanged), and
/// what it did (`None` where it doesn't apply). A schema with nothing to rewrite leaves the
/// pattern unread.
fn ql_stage(
    query: &Query,
    options: &QueryOptions,
    snapshot: &Snapshot,
    pattern: &GraphPattern,
    form: &Form<'_>,
) -> (Option<GraphPattern>, Option<crate::ql::QlReport>) {
    let Some(ql) = ql_rewriting(query, options) else {
        return (None, None);
    };
    #[cfg(test)]
    QL_STAGES.set(QL_STAGES.get() + 1);
    let tbox = ql.tbox(snapshot, options.access.as_deref());
    // What its `complete` refers to: the closure it rewrites over.
    let regime = match ql.closure().lists {
        true => crate::Regime::Owl2Rl,
        false => crate::Regime::Owl2Ql,
    };
    if tbox.is_empty() {
        let mut report = crate::ql::QlReport::default();
        report.completeness.regime = Some(regime);
        return (None, Some(report));
    }
    let (needed, set) = match form {
        Form::Select => (None, false),
        Form::Ask => (Some(Vec::new()), true),
        Form::Construct(template) => {
            let mut vars = Vec::new();
            GraphPattern::Bgp {
                patterns: template.to_vec(),
            }
            .on_in_scope_variable(|v| vars.push(v.clone()));
            (Some(vars), true)
        }
        Form::Describe => (None, true),
    };
    // A witness the data already has wherever it folds adds nothing (design §3), asked of
    // the data the query reads: for a reader of every graph (the cache is the store's).
    let data = options.access.is_none().then(|| ql::DataCheck {
        ql,
        options: QueryOptions {
            ql: None,
            ..options.clone()
        },
    });
    let (out, mut report) = ql::rewrite_query(
        pattern,
        &tbox,
        snapshot,
        ql.limits(),
        needed,
        set,
        data.as_ref(),
    );
    report.completeness.regime = Some(regime);
    let changed = report.patterns > 0;
    (changed.then_some(out), Some(report))
}

/// The QL rewriting, if it applies to `query`: on, and the query reads the inferred
/// statements of the default graph without a dataset or pre-bound variables, by a reader
/// who sees inferences (docs/design/ql-rewriting.md §1).
pub(super) fn ql_rewriting<'o>(
    query: &Query,
    options: &'o QueryOptions,
) -> Option<&'o crate::ql::QlRewriting> {
    let ql = options.ql.as_deref()?;
    (options.read_model == ReadModel::Materialised
        && options.dataset.is_none()
        && query_dataset(query).is_none()
        // A reader who sees no inferences gets no answers through them.
        && options.access.as_deref().is_none_or(|a| a.inferred)
        && options.pre_bound.is_none())
    .then_some(ql)
}
