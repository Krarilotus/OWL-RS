//! SPARQL from algebra. Everything the parser builds prints as text that parses back to
//! the same algebra (up to the numbering of generated names): the writer follows how the
//! parser folds a group's elements (SPARQL 1.1 §18.2.2.6) and wraps an element in braces
//! wherever writing it inline would fold differently. Shapes SPARQL can't state exactly
//! (only engine-built algebra has them) print as an equivalent subquery.

use std::fmt::{self, Display, Formatter};

use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, Term, Variable};

use crate::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression, QueryDataset,
};
use crate::query::{GraphUpdateOperation, Query, Update};
use crate::term::{GraphName, GraphNamePattern, GroundQuadPattern, QuadPattern};

type Aggregates<'p> = &'p [(Variable, AggregateExpression)];

// --- Terms ------------------------------------------------------------------------------

/// A literal; numbers and booleans in their short form where the grammar reads that form
/// back as the same literal.
pub(crate) fn fmt_literal(literal: &Literal, f: &mut Formatter<'_>) -> fmt::Result {
    let value = literal.value();
    let datatype = literal.datatype();
    let short = (datatype == xsd::INTEGER && is_integer(value))
        || (datatype == xsd::DECIMAL && is_decimal(value))
        || (datatype == xsd::DOUBLE && is_double(value))
        || (datatype == xsd::BOOLEAN && matches!(value, "true" | "false"));
    if short {
        f.write_str(value)
    } else {
        literal.fmt(f)
    }
}

pub(crate) fn fmt_iri(iri: &nrese_rdf::NamedNode, f: &mut Formatter<'_>) -> fmt::Result {
    f.write_str("<")?;
    f.write_str(iri.as_str())?;
    f.write_str(">")
}

pub(crate) fn fmt_variable(v: &Variable, f: &mut Formatter<'_>) -> fmt::Result {
    f.write_str("?")?;
    f.write_str(v.as_str())
}

pub(crate) fn fmt_term(term: &Term, f: &mut Formatter<'_>) -> fmt::Result {
    match term {
        Term::Literal(l) => fmt_literal(l, f),
        Term::Triple(t) => {
            write!(f, "<<( {} {} ", t.subject, t.predicate)?;
            fmt_term(&t.object, f)?;
            f.write_str(" )>>")
        }
        other => other.fmt(f),
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn unsigned(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

fn is_integer(s: &str) -> bool {
    digits(unsigned(s))
}

fn is_decimal(s: &str) -> bool {
    unsigned(s)
        .split_once('.')
        .is_some_and(|(whole, fraction)| (whole.is_empty() || digits(whole)) && digits(fraction))
}

fn is_double(s: &str) -> bool {
    let Some(e) = unsigned(s).find(['e', 'E']) else {
        return false;
    };
    let (mantissa, exponent) = (&unsigned(s)[..e], &unsigned(s)[e + 1..]);
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (digits(whole) && (fraction.is_empty() || digits(fraction)))
                || (whole.is_empty() && digits(fraction))
        }
        None => digits(mantissa),
    };
    mantissa_ok && digits(unsigned(exponent))
}

// --- Paths and expressions --------------------------------------------------------------

/// A path, every compound part in parentheses.
pub(crate) fn fmt_path(path: &PropertyPathExpression, f: &mut Formatter<'_>) -> fmt::Result {
    match path {
        PropertyPathExpression::NamedNode(p) => p.fmt(f),
        PropertyPathExpression::Reverse(p) => write!(f, "^({p})"),
        PropertyPathExpression::Sequence(a, b) => write!(f, "({a} / {b})"),
        PropertyPathExpression::Alternative(a, b) => write!(f, "({a} | {b})"),
        PropertyPathExpression::ZeroOrMore(p) => write!(f, "({p})*"),
        PropertyPathExpression::OneOrMore(p) => write!(f, "({p})+"),
        PropertyPathExpression::ZeroOrOne(p) => write!(f, "({p})?"),
        PropertyPathExpression::NegatedPropertySet(set) => {
            f.write_str("!(")?;
            for (i, p) in set.iter().enumerate() {
                if i > 0 {
                    f.write_str(" | ")?;
                }
                p.fmt(f)?;
            }
            f.write_str(")")
        }
    }
}

/// An expression; a variable that stands for one of `aggregates` prints as the aggregate.
pub(crate) struct Expr<'p> {
    pub expression: &'p Expression,
    pub aggregates: Aggregates<'p>,
}

impl fmt::Display for Expr<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt_expression(self.expression, self.aggregates, f)
    }
}

fn fmt_expression(
    e: &Expression,
    aggregates: Aggregates<'_>,
    f: &mut Formatter<'_>,
) -> fmt::Result {
    let sub = |e| Expr {
        expression: e,
        aggregates,
    };
    let binary = |f: &mut Formatter<'_>, a, op: &str, b| write!(f, "({} {op} {})", sub(a), sub(b));
    match e {
        Expression::NamedNode(n) => n.fmt(f),
        Expression::Literal(l) => fmt_literal(l, f),
        Expression::Variable(v) => match aggregates.iter().find(|(a, _)| a == v) {
            Some((_, aggregate)) => fmt_aggregate(aggregate, f),
            None => v.fmt(f),
        },
        Expression::Or(a, b) => binary(f, a, "||", b),
        Expression::And(a, b) => binary(f, a, "&&", b),
        Expression::Equal(a, b) => binary(f, a, "=", b),
        Expression::SameTerm(a, b) => write!(f, "sameTerm({}, {})", sub(a), sub(b)),
        Expression::Greater(a, b) => binary(f, a, ">", b),
        Expression::GreaterOrEqual(a, b) => binary(f, a, ">=", b),
        Expression::Less(a, b) => binary(f, a, "<", b),
        Expression::LessOrEqual(a, b) => binary(f, a, "<=", b),
        Expression::In(a, list) => {
            write!(f, "({} IN ", sub(a))?;
            fmt_list(list, aggregates, f)?;
            f.write_str(")")
        }
        Expression::Add(a, b) => binary(f, a, "+", b),
        Expression::Subtract(a, b) => binary(f, a, "-", b),
        Expression::Multiply(a, b) => binary(f, a, "*", b),
        Expression::Divide(a, b) => binary(f, a, "/", b),
        Expression::UnaryPlus(e) => write!(f, "+({})", sub(e)),
        Expression::UnaryMinus(e) => write!(f, "-({})", sub(e)),
        Expression::Not(inner) => match inner.as_ref() {
            Expression::Equal(a, b) => binary(f, a, "!=", b),
            Expression::In(a, list) => {
                write!(f, "({} NOT IN ", sub(a))?;
                fmt_list(list, aggregates, f)?;
                f.write_str(")")
            }
            Expression::Exists(p) => {
                f.write_str("NOT EXISTS ")?;
                fmt_braced(p, f)
            }
            other => write!(f, "!({})", sub(other)),
        },
        Expression::Exists(p) => {
            f.write_str("EXISTS ")?;
            fmt_braced(p, f)
        }
        Expression::Bound(v) => write!(f, "BOUND({v})"),
        Expression::If(a, b, c) => write!(f, "IF({}, {}, {})", sub(a), sub(b), sub(c)),
        Expression::Coalesce(list) => {
            f.write_str("COALESCE")?;
            fmt_list(list, aggregates, f)
        }
        Expression::FunctionCall(function, args) => {
            match function {
                Function::Custom(iri) => iri.fmt(f)?,
                other => f.write_str(other.keyword().unwrap_or_default())?,
            }
            fmt_list(args, aggregates, f)
        }
    }
}

fn fmt_list(list: &[Expression], aggregates: Aggregates<'_>, f: &mut Formatter<'_>) -> fmt::Result {
    f.write_str("(")?;
    for (i, e) in list.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        fmt_expression(e, aggregates, f)?;
    }
    f.write_str(")")
}

pub(crate) fn fmt_aggregate(aggregate: &AggregateExpression, f: &mut Formatter<'_>) -> fmt::Result {
    match aggregate {
        AggregateExpression::CountSolutions { distinct } => f.write_str(if *distinct {
            "COUNT(DISTINCT *)"
        } else {
            "COUNT(*)"
        }),
        AggregateExpression::FunctionCall {
            name,
            expr,
            distinct,
        } => {
            match name {
                AggregateFunction::Count => f.write_str("COUNT")?,
                AggregateFunction::Sum => f.write_str("SUM")?,
                AggregateFunction::Avg => f.write_str("AVG")?,
                AggregateFunction::Min => f.write_str("MIN")?,
                AggregateFunction::Max => f.write_str("MAX")?,
                AggregateFunction::Sample => f.write_str("SAMPLE")?,
                AggregateFunction::GroupConcat { .. } => f.write_str("GROUP_CONCAT")?,
                AggregateFunction::Custom(iri) => iri.fmt(f)?,
            }
            f.write_str("(")?;
            if *distinct {
                f.write_str("DISTINCT ")?;
            }
            // An aggregate's argument holds no aggregate.
            fmt_expression(expr, &[], f)?;
            if let AggregateFunction::GroupConcat {
                separator: Some(separator),
            } = name
            {
                write!(
                    f,
                    "; SEPARATOR = {}",
                    Literal::new_simple_literal(separator.as_str())
                )?;
            }
            f.write_str(")")
        }
    }
}

pub(crate) fn fmt_order(
    order: &OrderExpression,
    aggregates: Aggregates<'_>,
    f: &mut Formatter<'_>,
) -> fmt::Result {
    let (keyword, e) = match order {
        OrderExpression::Asc(e) => ("ASC", e),
        OrderExpression::Desc(e) => ("DESC", e),
    };
    write!(
        f,
        "{keyword}({})",
        Expr {
            expression: e,
            aggregates
        }
    )
}

// --- Graph patterns ---------------------------------------------------------------------

/// What the written elements of a group end with, for the next element to know whether
/// writing its triples inline would merge them into the same block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail {
    Nothing,
    /// Triples of a basic graph pattern.
    Triples,
    /// A path (or other triples the block won't merge with following ones).
    Path,
    Other,
}

/// Whether a pattern prints as a subquery (it has solution modifiers).
fn is_subquery(p: &GraphPattern) -> bool {
    matches!(
        p,
        GraphPattern::Project { .. }
            | GraphPattern::Distinct { .. }
            | GraphPattern::Reduced { .. }
            | GraphPattern::Slice { .. }
            | GraphPattern::OrderBy { .. }
            | GraphPattern::Group { .. }
    )
}

/// `{ content }`.
fn fmt_braced(p: &GraphPattern, f: &mut Formatter<'_>) -> fmt::Result {
    f.write_str("{ ")?;
    fmt_group(p, f)?;
    f.write_str(" }")
}

/// The content of a group: `{ ` + this + ` }` parses back to `p`.
pub(crate) fn fmt_group(p: &GraphPattern, f: &mut Formatter<'_>) -> fmt::Result {
    if is_subquery(p) {
        return fmt_select(p, None, Form::SubSelect, f);
    }
    fmt_sequence(p, f).map(|_| ())
}

/// Elements whose fold is `p`.
fn fmt_sequence(p: &GraphPattern, f: &mut Formatter<'_>) -> Result<Tail, fmt::Error> {
    match p {
        GraphPattern::Bgp { patterns } => {
            for (i, t) in patterns.iter().enumerate() {
                if i > 0 {
                    f.write_str(" ")?;
                }
                t.fmt(f)?;
                f.write_str(" .")?;
            }
            Ok(if patterns.is_empty() {
                Tail::Nothing
            } else {
                Tail::Triples
            })
        }
        GraphPattern::Path {
            subject,
            path,
            object,
        } => {
            write!(f, "{subject} {path} {object} .")?;
            Ok(Tail::Path)
        }
        GraphPattern::Join { left, right } => {
            let tail = fmt_prefix(left, f)?;
            fmt_element(right, tail, f)
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            space(fmt_prefix(left, f)?, f)?;
            f.write_str("OPTIONAL { ")?;
            match expression {
                Some(e) => {
                    space(fmt_prefix(right, f)?, f)?;
                    write!(f, "FILTER({e})")?;
                }
                // A group that is a filter would become the optional's condition: keep
                // it a filter with a subquery around.
                None if matches!(right.as_ref(), GraphPattern::Filter { .. }) => {
                    f.write_str("SELECT * WHERE ")?;
                    fmt_braced(right, f)?;
                }
                None => fmt_group(right, f)?,
            }
            f.write_str(" }")?;
            Ok(Tail::Other)
        }
        GraphPattern::Lateral { left, right } => {
            space(fmt_prefix(left, f)?, f)?;
            f.write_str("LATERAL ")?;
            fmt_braced(right, f)?;
            Ok(Tail::Other)
        }
        GraphPattern::Minus { left, right } => {
            space(fmt_prefix(left, f)?, f)?;
            f.write_str("MINUS ")?;
            fmt_braced(right, f)?;
            Ok(Tail::Other)
        }
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => {
            space(fmt_prefix(inner, f)?, f)?;
            write!(f, "BIND({expression} AS {variable})")?;
            Ok(Tail::Other)
        }
        GraphPattern::Filter { expr, inner } => {
            space(fmt_prefix(inner, f)?, f)?;
            write!(f, "FILTER({expr})")?;
            Ok(Tail::Other)
        }
        other => fmt_element(other, Tail::Nothing, f),
    }
}

/// Elements whose fold is `p`, to be followed by more: a filter would take in what
/// follows, so it goes in braces.
fn fmt_prefix(p: &GraphPattern, f: &mut Formatter<'_>) -> Result<Tail, fmt::Error> {
    if matches!(p, GraphPattern::Filter { .. }) {
        fmt_braced(p, f)?;
        return Ok(Tail::Other);
    }
    fmt_sequence(p, f)
}

fn space(tail: Tail, f: &mut Formatter<'_>) -> fmt::Result {
    if tail == Tail::Nothing {
        Ok(())
    } else {
        f.write_str(" ")
    }
}

/// `p` as one element after elements ending in `tail`, joined to them.
fn fmt_element(p: &GraphPattern, tail: Tail, f: &mut Formatter<'_>) -> Result<Tail, fmt::Error> {
    match p {
        // Triples join the block before them unless that would merge two basic graph
        // patterns into one.
        GraphPattern::Bgp { patterns } if !patterns.is_empty() && tail != Tail::Triples => {
            space(tail, f)?;
            fmt_sequence(p, f)
        }
        GraphPattern::Path { .. } => {
            space(tail, f)?;
            fmt_sequence(p, f)
        }
        GraphPattern::Union { .. } => {
            space(tail, f)?;
            fmt_union(p, f)?;
            Ok(Tail::Other)
        }
        GraphPattern::Graph { name, inner } => {
            space(tail, f)?;
            write!(f, "GRAPH {name} ")?;
            fmt_braced(inner, f)?;
            Ok(Tail::Other)
        }
        GraphPattern::Service {
            name,
            inner,
            silent,
        } => {
            space(tail, f)?;
            f.write_str(if *silent {
                "SERVICE SILENT "
            } else {
                "SERVICE "
            })?;
            write!(f, "{name} ")?;
            fmt_braced(inner, f)?;
            Ok(Tail::Other)
        }
        GraphPattern::Values {
            variables,
            bindings,
        } => {
            space(tail, f)?;
            f.write_str("VALUES (")?;
            for v in variables {
                write!(f, " {v}")?;
            }
            f.write_str(" ) {")?;
            for row in bindings {
                f.write_str(" (")?;
                for value in row {
                    match value {
                        Some(value) => write!(f, " {value}")?,
                        None => f.write_str(" UNDEF")?,
                    }
                }
                f.write_str(" )")?;
            }
            f.write_str(" }")?;
            Ok(Tail::Other)
        }
        other => {
            space(tail, f)?;
            fmt_braced(other, f)?;
            Ok(Tail::Other)
        }
    }
}

/// `{ a } UNION { b } UNION …`, left-nested unions as one chain.
fn fmt_union(p: &GraphPattern, f: &mut Formatter<'_>) -> fmt::Result {
    match p {
        GraphPattern::Union { left, right } => {
            fmt_union(left, f)?;
            f.write_str(" UNION ")?;
            fmt_braced(right, f)
        }
        other => fmt_braced(other, f),
    }
}

// --- Queries ----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Select,
    SubSelect,
    Construct,
    Describe,
    Ask,
}

/// A query form's pattern: projection and modifiers from the algebra above the `WHERE`
/// pattern, as the parser built them (SPARQL 1.1 §18.2.4).
fn fmt_select(
    pattern: &GraphPattern,
    dataset: Option<&QueryDataset>,
    form: Form,
    f: &mut Formatter<'_>,
) -> fmt::Result {
    let mut p = pattern;
    let mut slice = None;
    let mut modifier = "";
    let mut projection = None;
    let mut order = None;
    if let GraphPattern::Slice {
        inner,
        start,
        length,
    } = p
    {
        slice = Some((*start, *length));
        p = inner;
    }
    match p {
        GraphPattern::Distinct { inner } => {
            modifier = " DISTINCT";
            p = inner;
        }
        GraphPattern::Reduced { inner } => {
            modifier = " REDUCED";
            p = inner;
        }
        _ => {}
    }
    if let GraphPattern::Project { inner, variables } = p {
        projection = Some(variables.as_slice());
        p = inner;
    }
    if let GraphPattern::OrderBy { inner, expression } = p {
        order = Some(expression.as_slice());
        p = inner;
    }

    // Above a group, the select expressions, VALUES and HAVING belong to the query, not
    // to its WHERE pattern.
    let mut expressions: Vec<(&Variable, &Expression)> = Vec::new();
    let mut values = None;
    let mut having = None;
    let mut group = None;
    {
        let mut q = p;
        let mut extends = Vec::new();
        while let GraphPattern::Extend {
            inner,
            variable,
            expression,
        } = q
        {
            if !projection.is_some_and(|vars| vars.contains(variable)) {
                break;
            }
            extends.push((variable, expression));
            q = inner;
        }
        let mut found_values = None;
        if let GraphPattern::Join { left, right } = q
            && matches!(right.as_ref(), GraphPattern::Values { .. })
            && leads_to_group(left)
        {
            found_values = Some(right.as_ref());
            q = left;
        }
        let mut found_having = None;
        if let GraphPattern::Filter { expr, inner } = q
            && matches!(inner.as_ref(), GraphPattern::Group { .. })
        {
            found_having = Some(expr);
            q = inner;
        }
        if let GraphPattern::Group { .. } = q {
            extends.reverse();
            expressions = extends;
            values = found_values;
            having = found_having;
            group = Some(q);
            p = q;
        } else if form == Form::Describe {
            // `DESCRIBE <iri>`: the IRI bound to a projected variable.
            let mut q = p;
            let mut iris = Vec::new();
            while let GraphPattern::Extend {
                inner,
                variable,
                expression: expression @ Expression::NamedNode(_),
            } = q
            {
                if !projection.is_some_and(|vars| vars.contains(variable)) {
                    break;
                }
                iris.push((variable, expression));
                q = inner;
            }
            iris.reverse();
            expressions = iris;
            p = q;
        }
    }
    let (where_pattern, group_variables, aggregates): (&GraphPattern, &[Variable], Aggregates<'_>) =
        match group {
            Some(GraphPattern::Group {
                inner,
                variables,
                aggregates,
            }) => (inner, variables, aggregates),
            _ => (p, &[], &[]),
        };

    match form {
        Form::Select | Form::SubSelect => {
            f.write_str("SELECT")?;
            f.write_str(modifier)?;
            match projection {
                Some(vars) if !vars.is_empty() => {
                    for v in vars {
                        match expressions.iter().find(|(e, _)| *e == v) {
                            Some((_, e)) => write!(
                                f,
                                " ({} AS {v})",
                                Expr {
                                    expression: e,
                                    aggregates
                                }
                            )?,
                            None => write!(f, " {v}")?,
                        }
                    }
                }
                // A group with no projection above: its variables and aggregates.
                None if group.is_some() => {
                    for v in group_variables {
                        write!(f, " {v}")?;
                    }
                    for (v, aggregate) in aggregates {
                        f.write_str(" (")?;
                        fmt_aggregate(aggregate, f)?;
                        write!(f, " AS {v})")?;
                    }
                }
                _ => f.write_str(" *")?,
            }
        }
        Form::Describe => {
            f.write_str("DESCRIBE")?;
            match projection {
                Some(vars) if !vars.is_empty() => {
                    for v in vars {
                        match expressions.iter().find(|(e, _)| *e == v) {
                            Some((_, e)) => write!(f, " {e}")?,
                            None => write!(f, " {v}")?,
                        }
                    }
                }
                _ => f.write_str(" *")?,
            }
        }
        Form::Construct | Form::Ask => {}
    }
    if let Some(dataset) = dataset {
        write!(f, "{dataset}")?;
    }
    f.write_str(" WHERE ")?;
    fmt_braced(where_pattern, f)?;
    if !group_variables.is_empty() {
        f.write_str(" GROUP BY")?;
        for v in group_variables {
            write!(f, " {v}")?;
        }
    }
    if let Some(expr) = having {
        write!(
            f,
            " HAVING ({})",
            Expr {
                expression: expr,
                aggregates
            }
        )?;
    }
    if let Some(order) = order {
        f.write_str(" ORDER BY")?;
        for o in order {
            f.write_str(" ")?;
            fmt_order(o, aggregates, f)?;
        }
    }
    if let Some((start, length)) = slice {
        if let Some(length) = length {
            write!(f, " LIMIT {length}")?;
        }
        if start > 0 || length.is_none() {
            write!(f, " OFFSET {start}")?;
        }
    }
    if let Some(values) = values {
        f.write_str(" ")?;
        fmt_element(values, Tail::Nothing, f)?;
    }
    Ok(())
}

/// Whether `p` is a group under at most a `HAVING` filter.
fn leads_to_group(p: &GraphPattern) -> bool {
    match p {
        GraphPattern::Group { .. } => true,
        GraphPattern::Filter { inner, .. } => matches!(inner.as_ref(), GraphPattern::Group { .. }),
        _ => false,
    }
}

fn fmt_base(base: Option<&nrese_rdf::Iri<String>>, f: &mut Formatter<'_>) -> fmt::Result {
    match base {
        Some(base) => write!(f, "BASE <{}> ", base.as_str()),
        None => Ok(()),
    }
}

pub(crate) fn fmt_query(query: &Query, f: &mut Formatter<'_>) -> fmt::Result {
    fmt_base(query.base_iri(), f)?;
    match query {
        Query::Select {
            dataset, pattern, ..
        } => fmt_select(pattern, dataset.as_ref(), Form::Select, f),
        Query::Construct {
            template,
            dataset,
            pattern,
            ..
        } => {
            f.write_str("CONSTRUCT {")?;
            for t in template {
                write!(f, " {t} .")?;
            }
            f.write_str(" }")?;
            fmt_select(pattern, dataset.as_ref(), Form::Construct, f)
        }
        Query::Describe {
            dataset, pattern, ..
        } => fmt_select(pattern, dataset.as_ref(), Form::Describe, f),
        Query::Ask {
            dataset, pattern, ..
        } => {
            f.write_str("ASK")?;
            fmt_select(pattern, dataset.as_ref(), Form::Ask, f)
        }
    }
}

// --- Updates ----------------------------------------------------------------------------

pub(crate) fn fmt_update(update: &Update, f: &mut Formatter<'_>) -> fmt::Result {
    fmt_base(update.base_iri.as_ref(), f)?;
    for (i, operation) in update.operations.iter().enumerate() {
        if i > 0 {
            f.write_str(" ;\n")?;
        }
        fmt_operation(operation, f)?;
    }
    Ok(())
}

/// Quads as triples of the default graph and `GRAPH` blocks, in their order.
fn fmt_quads<'q, G: PartialEq + fmt::Display + 'q, T: fmt::Display + 'q>(
    quads: impl Iterator<Item = (Option<&'q G>, T)>,
    f: &mut Formatter<'_>,
) -> fmt::Result {
    f.write_str("{")?;
    let mut open: Option<&G> = None;
    for (graph, triple) in quads {
        if open != graph {
            if open.is_some() {
                f.write_str(" }")?;
            }
            if let Some(g) = graph {
                write!(f, " GRAPH {g} {{")?;
            }
            open = graph;
        }
        write!(f, " {triple} .")?;
    }
    if open.is_some() {
        f.write_str(" }")?;
    }
    f.write_str(" }")
}

/// A quad's triple, for [`fmt_quads`].
struct TripleOf<'q, S, P, O>(&'q S, &'q P, &'q O);

impl<S: fmt::Display, P: fmt::Display, O: fmt::Display> fmt::Display for TripleOf<'_, S, P, O> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.0, self.1, self.2)
    }
}

struct TermOf<'t>(&'t Term);

impl fmt::Display for TermOf<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt_term(self.0, f)
    }
}

fn named_graph(g: &GraphName) -> Option<&GraphName> {
    match g {
        GraphName::DefaultGraph => None,
        named => Some(named),
    }
}

fn graph_pattern_name<'g>(
    g: &'g GraphNamePattern,
    with: Option<&GraphNamePattern>,
) -> Option<&'g GraphNamePattern> {
    match g {
        GraphNamePattern::DefaultGraph => None,
        g if Some(g) == with => None,
        g => Some(g),
    }
}

pub(crate) fn fmt_operation(
    operation: &GraphUpdateOperation,
    f: &mut Formatter<'_>,
) -> fmt::Result {
    match operation {
        GraphUpdateOperation::InsertData { data } => {
            f.write_str("INSERT DATA ")?;
            fmt_quads(
                data.iter().map(|q| {
                    (
                        named_graph(&q.graph_name),
                        TripleOf(&q.subject, &q.predicate, &TermOf(&q.object)).to_string(),
                    )
                }),
                f,
            )
        }
        GraphUpdateOperation::DeleteData { data } => {
            f.write_str("DELETE DATA ")?;
            fmt_quads(
                data.iter().map(|q| {
                    (
                        named_graph(&q.graph_name),
                        TripleOf(&q.subject, &q.predicate, &q.object).to_string(),
                    )
                }),
                f,
            )
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => {
            // `WITH g` is what leaves the named graphs of the dataset open.
            let with = match using {
                Some(QueryDataset {
                    default,
                    named: None,
                }) if default.len() == 1 => Some(GraphNamePattern::NamedNode(default[0].clone())),
                _ => None,
            };
            if let Some(GraphNamePattern::NamedNode(g)) = &with {
                write!(f, "WITH {g} ")?;
            }
            if !delete.is_empty() || insert.is_empty() {
                f.write_str("DELETE ")?;
                fmt_quads(
                    delete.iter().map(|q: &GroundQuadPattern| {
                        (
                            graph_pattern_name(&q.graph_name, with.as_ref()),
                            TripleOf(&q.subject, &q.predicate, &q.object).to_string(),
                        )
                    }),
                    f,
                )?;
                f.write_str(" ")?;
            }
            if !insert.is_empty() {
                f.write_str("INSERT ")?;
                fmt_quads(
                    insert.iter().map(|q: &QuadPattern| {
                        (
                            graph_pattern_name(&q.graph_name, with.as_ref()),
                            TripleOf(&q.subject, &q.predicate, &q.object).to_string(),
                        )
                    }),
                    f,
                )?;
                f.write_str(" ")?;
            }
            if with.is_none()
                && let Some(using) = using
            {
                for g in &using.default {
                    write!(f, "USING {g} ")?;
                }
                for g in using.named.iter().flatten() {
                    write!(f, "USING NAMED {g} ")?;
                }
            }
            f.write_str("WHERE ")?;
            fmt_braced(pattern, f)
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => {
            f.write_str(if *silent { "LOAD SILENT " } else { "LOAD " })?;
            source.fmt(f)?;
            if let GraphName::NamedNode(g) = destination {
                write!(f, " INTO GRAPH {g}")?;
            }
            Ok(())
        }
        GraphUpdateOperation::Clear { silent, graph } => {
            f.write_str(if *silent { "CLEAR SILENT " } else { "CLEAR " })?;
            graph.fmt(f)
        }
        GraphUpdateOperation::Drop { silent, graph } => {
            f.write_str(if *silent { "DROP SILENT " } else { "DROP " })?;
            graph.fmt(f)
        }
        GraphUpdateOperation::Create { silent, graph } => {
            f.write_str(if *silent { "CREATE SILENT " } else { "CREATE " })?;
            write!(f, "GRAPH {graph}")
        }
    }
}
