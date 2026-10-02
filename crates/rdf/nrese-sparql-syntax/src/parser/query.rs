//! Query forms and their solution modifiers (SPARQL 1.1 §18.2.4–18.2.5), and the update
//! operations (SPARQL 1.1 Update §3).

use std::collections::HashSet;

use nrese_rdf::{BlankNode, Iri, NamedNode, NamedOrBlankNode, Term, Triple, Variable};

use super::pattern::new_join;
use super::{ParseResult, Parser};
use crate::algebra::{Expression, GraphPattern, GraphTarget, OrderExpression, QueryDataset};
use crate::query::{GraphUpdateOperation, Query, Update};
use crate::term::{
    GraphName, GraphNamePattern, GroundQuad, GroundQuadPattern, Quad, QuadPattern, TriplePattern,
};

/// What `SELECT` (or `DESCRIBE`) asks for.
enum Selection {
    /// `*`, or the projection of a form without one (all variables in scope).
    Star,
    Members(Vec<Member>),
}

enum Member {
    Variable(Variable),
    Expression(Expression, Variable),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifier {
    None,
    Distinct,
    Reduced,
}

/// `GROUP BY`: the grouping variables, and the expressions bound to some of them.
type Grouping = (Vec<Variable>, Vec<(Expression, Variable)>);

/// The parts after the `WHERE` clause.
#[derive(Default)]
struct SolutionModifiers {
    group: Option<Grouping>,
    having: Option<Expression>,
    order: Option<Vec<OrderExpression>>,
    slice: Option<(usize, Option<usize>)>,
    values: Option<GraphPattern>,
}

/// A base IRI in absolute form (RFC 3986 §5.2.1), its dot segments removed: a reference
/// with an empty path takes the base's path as it is (§5.2.2), so a base with dot segments
/// would make IRIs that read differently once printed. Found by fuzzing (`nrese-fuzz`):
/// `BASE <http:./example.org/> PREFIX : <#>` gave `:x` as `http:./example.org/#x`, which
/// reads back as `http:example.org/#x`.
pub(super) fn absolute(base: Iri<String>) -> Iri<String> {
    base.resolve_unchecked(base.as_str())
}

impl<'a> Parser<'a> {
    // --- Prologue ---------------------------------------------------------------------

    fn prologue(&mut self) -> ParseResult<()> {
        loop {
            if self.keyword("BASE") {
                let at = self.peek_offset();
                let text = self.iriref_text()?;
                let iri = self.resolve(&text, at)?;
                self.base = Some(absolute(Iri::parse_unchecked(iri.into_string())));
            } else if self.keyword("PREFIX") {
                let prefix = self.pname_ns()?.to_owned();
                let iri = self.iriref()?;
                let iri = Iri::parse_unchecked(iri.into_string());
                self.prefixes.insert(prefix, super::Namespace::new(iri));
            } else if self.looking_at_keyword("VERSION") {
                if !self.options.sparql_12 {
                    return Err(self.error("VERSION needs SPARQL 1.2"));
                }
                self.keyword("VERSION");
                let at = self.peek_offset();
                let long = self.looking_at("\"\"\"") || self.looking_at("'''");
                if !self.at_string() || long {
                    return Err(self.error_at(at, "VERSION wants a short string"));
                }
                self.string()?;
            } else {
                return Ok(());
            }
        }
    }

    // --- Queries ----------------------------------------------------------------------

    pub(super) fn query(&mut self) -> ParseResult<Query> {
        self.prologue()?;
        let word = self.peek_word().to_ascii_uppercase();
        let query = match word.as_str() {
            "SELECT" => {
                let (dataset, pattern) = self.select_query(true)?;
                Query::Select {
                    dataset,
                    pattern,
                    base_iri: self.base.clone(),
                }
            }
            "CONSTRUCT" => self.construct_query()?,
            "DESCRIBE" => self.describe_query()?,
            "ASK" => {
                self.keyword("ASK");
                self.aggregates.push(Vec::new());
                let dataset = self.dataset_clauses("FROM")?;
                let pattern = self.where_clause()?;
                let modifiers = self.solution_modifiers()?;
                Query::Ask {
                    dataset,
                    pattern: self.build_select(
                        Modifier::None,
                        Selection::Star,
                        pattern,
                        modifiers,
                        false,
                    )?,
                    base_iri: self.base.clone(),
                }
            }
            _ => return Err(self.expected("SELECT, CONSTRUCT, DESCRIBE or ASK")),
        };
        if !self.at_end() {
            return Err(self.expected("the end of the query"));
        }
        Ok(query)
    }

    /// `SELECT … WHERE … modifiers` (with a dataset at the top level).
    fn select_query(&mut self, top: bool) -> ParseResult<(Option<QueryDataset>, GraphPattern)> {
        self.expect_keyword("SELECT")?;
        self.aggregates.push(Vec::new());
        let modifier = if self.keyword("DISTINCT") {
            Modifier::Distinct
        } else if self.keyword("REDUCED") {
            Modifier::Reduced
        } else {
            Modifier::None
        };
        let selection = if self.eat("*") {
            Selection::Star
        } else {
            let mut members = Vec::new();
            loop {
                if let Some(v) = self.try_variable()? {
                    members.push(Member::Variable(v));
                } else if self.eat("(") {
                    let allowed = std::mem::replace(&mut self.aggregates_allowed, true);
                    let expression = self.expression();
                    self.aggregates_allowed = allowed;
                    let expression = expression?;
                    self.expect_keyword("AS")?;
                    let v = self.variable()?;
                    self.expect(")")?;
                    members.push(Member::Expression(expression, v));
                } else {
                    break;
                }
            }
            if members.is_empty() {
                return Err(self.expected("'*', variables or (expression AS ?variable)"));
            }
            Selection::Members(members)
        };
        let dataset = if top {
            self.dataset_clauses("FROM")?
        } else {
            None
        };
        let pattern = self.where_clause()?;
        let modifiers = self.solution_modifiers()?;
        Ok((
            dataset,
            self.build_select(modifier, selection, pattern, modifiers, true)?,
        ))
    }

    /// A subquery, inside `{ }`.
    pub(super) fn sub_select(&mut self) -> ParseResult<GraphPattern> {
        Ok(self.select_query(false)?.1)
    }

    fn construct_query(&mut self) -> ParseResult<Query> {
        self.expect_keyword("CONSTRUCT")?;
        self.aggregates.push(Vec::new());
        if self.peek() == Some(b'{') {
            self.pos += 1;
            let template = if self.at_triples_start() {
                self.triples_template()?
            } else {
                Vec::new()
            };
            self.expect("}")?;
            // The template's blank nodes are fresh per solution, apart from the pattern's.
            self.current_blank_nodes.clear();
            let dataset = self.dataset_clauses("FROM")?;
            let pattern = self.where_clause()?;
            let modifiers = self.solution_modifiers()?;
            return Ok(Query::Construct {
                template,
                dataset,
                pattern: self.build_select(
                    Modifier::None,
                    Selection::Star,
                    pattern,
                    modifiers,
                    false,
                )?,
                base_iri: self.base.clone(),
            });
        }
        let dataset = self.dataset_clauses("FROM")?;
        self.expect_keyword("WHERE")?;
        self.expect("{")?;
        let template = if self.at_triples_start() {
            self.triples_template()?
        } else {
            Vec::new()
        };
        self.expect("}")?;
        let modifiers = self.solution_modifiers()?;
        let pattern = GraphPattern::Bgp {
            patterns: template.clone(),
        };
        Ok(Query::Construct {
            template,
            dataset,
            pattern: self.build_select(
                Modifier::None,
                Selection::Star,
                pattern,
                modifiers,
                false,
            )?,
            base_iri: self.base.clone(),
        })
    }

    fn describe_query(&mut self) -> ParseResult<Query> {
        self.expect_keyword("DESCRIBE")?;
        self.aggregates.push(Vec::new());
        let selection = if self.eat("*") {
            Selection::Star
        } else {
            let mut members = Vec::new();
            loop {
                if let Some(v) = self.try_variable()? {
                    members.push(Member::Variable(v));
                } else if let Some(iri) = self.try_iri()? {
                    let v = self.fresh_variable("describe");
                    members.push(Member::Expression(iri.into(), v));
                } else {
                    break;
                }
            }
            if members.is_empty() {
                return Err(self.expected("'*', variables or IRIs"));
            }
            Selection::Members(members)
        };
        let dataset = self.dataset_clauses("FROM")?;
        let pattern = if self.looking_at_keyword("WHERE") || self.peek() == Some(b'{') {
            self.where_clause()?
        } else {
            GraphPattern::default()
        };
        let modifiers = self.solution_modifiers()?;
        Ok(Query::Describe {
            dataset,
            pattern: self.build_select(Modifier::None, selection, pattern, modifiers, false)?,
            base_iri: self.base.clone(),
        })
    }

    /// `FROM` / `FROM NAMED` (or `USING` / `USING NAMED`) clauses.
    fn dataset_clauses(&mut self, keyword: &str) -> ParseResult<Option<QueryDataset>> {
        let mut dataset: Option<QueryDataset> = None;
        while self.keyword(keyword) {
            let named = self.keyword("NAMED");
            let iri = self.iri()?;
            let dataset = dataset.get_or_insert_with(|| QueryDataset {
                default: Vec::new(),
                named: Some(Vec::new()),
            });
            if named {
                dataset.named.get_or_insert_with(Vec::new).push(iri);
            } else {
                dataset.default.push(iri);
            }
        }
        Ok(dataset)
    }

    fn where_clause(&mut self) -> ParseResult<GraphPattern> {
        self.keyword("WHERE");
        self.group_graph_pattern()
    }

    fn solution_modifiers(&mut self) -> ParseResult<SolutionModifiers> {
        let mut modifiers = SolutionModifiers::default();
        if self.keyword("GROUP") {
            self.expect_keyword("BY")?;
            let mut variables = Vec::new();
            let mut bindings = Vec::new();
            loop {
                let (expression, alias) = if self.eat("(") {
                    let e = self.expression()?;
                    let alias = if self.keyword("AS") {
                        Some(self.variable()?)
                    } else {
                        None
                    };
                    self.expect(")")?;
                    (e, alias)
                } else if let Some(v) = self.try_variable()? {
                    (Expression::Variable(v), None)
                } else if self.at_constraint() {
                    (self.constraint()?, None)
                } else {
                    break;
                };
                match (expression, alias) {
                    (Expression::Variable(v), None) => variables.push(v),
                    (e, alias) => {
                        let v = match alias {
                            Some(v) => v,
                            None => self.fresh_variable("group"),
                        };
                        bindings.push((e, v.clone()));
                        variables.push(v);
                    }
                }
            }
            if variables.is_empty() {
                return Err(self.expected("a GROUP BY condition"));
            }
            modifiers.group = Some((variables, bindings));
        }
        if self.keyword("HAVING") {
            let allowed = std::mem::replace(&mut self.aggregates_allowed, true);
            let mut conditions: Vec<Expression> = Vec::new();
            let result = loop {
                if !self.at_constraint() {
                    break Ok(());
                }
                match self.constraint() {
                    Ok(c) => conditions.push(c),
                    Err(e) => break Err(e),
                }
            };
            self.aggregates_allowed = allowed;
            result?;
            let having = (!conditions.is_empty())
                .then(|| super::expr::balanced(conditions, Expression::And));
            if having.is_none() {
                return Err(self.expected("a HAVING condition"));
            }
            modifiers.having = having;
        }
        if self.keyword("ORDER") {
            self.expect_keyword("BY")?;
            let allowed = std::mem::replace(&mut self.aggregates_allowed, true);
            let order = self.order_conditions();
            self.aggregates_allowed = allowed;
            modifiers.order = Some(order?);
        }
        modifiers.slice = self.limit_offset()?;
        if self.keyword("VALUES") {
            modifiers.values = Some(self.data_block()?);
        }
        Ok(modifiers)
    }

    fn order_conditions(&mut self) -> ParseResult<Vec<OrderExpression>> {
        let mut order = Vec::new();
        loop {
            if self.keyword("ASC") {
                order.push(OrderExpression::Asc(self.bracketted()?));
            } else if self.keyword("DESC") {
                order.push(OrderExpression::Desc(self.bracketted()?));
            } else if let Some(v) = self.try_variable()? {
                order.push(OrderExpression::Asc(v.into()));
            } else if self.at_constraint() {
                order.push(OrderExpression::Asc(self.constraint()?));
            } else {
                break;
            }
        }
        if order.is_empty() {
            return Err(self.expected("an ORDER BY condition"));
        }
        Ok(order)
    }

    fn bracketted(&mut self) -> ParseResult<Expression> {
        self.expect("(")?;
        let e = self.expression()?;
        self.expect(")")?;
        Ok(e)
    }

    fn limit_offset(&mut self) -> ParseResult<Option<(usize, Option<usize>)>> {
        let mut limit = None;
        let mut offset = None;
        for _ in 0..2 {
            if limit.is_none() && self.keyword("LIMIT") {
                limit = Some(self.unsigned("LIMIT")?);
            } else if offset.is_none() && self.keyword("OFFSET") {
                offset = Some(self.unsigned("OFFSET")?);
            }
        }
        Ok((limit.is_some() || offset.is_some()).then(|| (offset.unwrap_or(0), limit)))
    }

    fn unsigned(&mut self, what: &str) -> ParseResult<usize> {
        self.ws();
        let start = self.pos;
        let mut end = start;
        while self.byte_at(end).is_some_and(|b| b.is_ascii_digit()) {
            end += 1;
        }
        if end == start {
            return Err(self.expected(&format!("a non-negative integer after {what}")));
        }
        self.pos = end;
        self.text[start..end]
            .parse()
            .map_err(|_| self.error_at(start, format!("{what} is too large")))
    }

    /// The algebra of a query's pattern and modifiers (SPARQL 1.1 §18.2.4), checking
    /// what a projection may name.
    fn build_select(
        &mut self,
        modifier: Modifier,
        selection: Selection,
        pattern: GraphPattern,
        modifiers: SolutionModifiers,
        is_select: bool,
    ) -> ParseResult<GraphPattern> {
        let at = self.pos;
        let mut p = pattern;
        let aggregates = self.aggregates.pop().unwrap_or_default();
        let mut group = modifiers.group;
        if group.is_none() && !aggregates.is_empty() {
            group = Some((Vec::new(), Vec::new()));
        }
        let grouped = group.is_some();
        if let Some((variables, bindings)) = group {
            for (expression, variable) in bindings {
                // As for BIND (§18.2.1): the variable must not be in scope already, from the
                // pattern or an earlier condition; the query would print as a BIND that
                // reads back as an error (found by fuzzing, `nrese-fuzz`).
                let mut bound = false;
                p.on_in_scope_variable(|v| bound |= *v == variable);
                if bound {
                    return Err(self.error_at(
                        at,
                        format!("GROUP BY binds {variable}, which is bound already"),
                    ));
                }
                p = GraphPattern::Extend {
                    inner: Box::new(p),
                    variable,
                    expression,
                };
            }
            p = GraphPattern::Group {
                inner: Box::new(p),
                variables,
                aggregates,
            };
        }
        if let Some(expr) = modifiers.having {
            p = GraphPattern::Filter {
                expr,
                inner: Box::new(p),
            };
        }
        if let Some(values) = modifiers.values {
            p = new_join(p, values);
        }
        let mut projection = Vec::new();
        match selection {
            Selection::Members(members) => {
                let mut visible = HashSet::new();
                p.on_in_scope_variable(|v| {
                    visible.insert(v.clone());
                });
                for member in members {
                    let v = match member {
                        Member::Variable(v) => {
                            if grouped && !visible.contains(&v) {
                                return Err(self.error_at(
                                    at,
                                    format!("{v} is projected but neither grouped nor aggregated"),
                                ));
                            }
                            v
                        }
                        Member::Expression(expression, v) => {
                            if visible.contains(&v) {
                                return Err(self.error_at(
                                    at,
                                    format!(
                                        "the SELECT expression assigns {v}, which is already bound"
                                    ),
                                ));
                            }
                            if grouped && !bound_in(&expression, &visible) {
                                return Err(self.error_at(
                                    at,
                                    format!("the expression for {v} uses a variable neither grouped nor aggregated"),
                                ));
                            }
                            p = GraphPattern::Extend {
                                inner: Box::new(p),
                                variable: v.clone(),
                                expression,
                            };
                            v
                        }
                    };
                    if projection.contains(&v) {
                        return Err(self.error_at(at, format!("{v} is projected twice")));
                    }
                    // SPARQL 1.2: a later select expression may use this one's variable.
                    visible.insert(v.clone());
                    projection.push(v);
                }
            }
            Selection::Star => {
                if grouped && is_select {
                    return Err(self.error_at(at, "SELECT * can't be used with GROUP BY"));
                }
                let mut seen = HashSet::new();
                p.on_in_scope_variable(|v| {
                    if seen.insert(v) {
                        projection.push(v.clone());
                    }
                });
                projection.sort();
            }
        }
        if let Some(expression) = modifiers.order {
            p = GraphPattern::OrderBy {
                inner: Box::new(p),
                expression,
            };
        }
        p = GraphPattern::Project {
            inner: Box::new(p),
            variables: projection,
        };
        match modifier {
            Modifier::Distinct => p = GraphPattern::Distinct { inner: Box::new(p) },
            Modifier::Reduced => p = GraphPattern::Reduced { inner: Box::new(p) },
            Modifier::None => {}
        }
        if let Some((start, length)) = modifiers.slice {
            p = GraphPattern::Slice {
                inner: Box::new(p),
                start,
                length,
            };
        }
        Ok(p)
    }

    // --- Updates ----------------------------------------------------------------------

    pub(super) fn update(&mut self) -> ParseResult<Update> {
        let mut operations = Vec::new();
        loop {
            self.prologue()?;
            if self.at_end() {
                break;
            }
            operations.extend(self.update_operation()?);
            // Blank node labels are scoped to one operation.
            self.used_blank_nodes.clear();
            self.current_blank_nodes.clear();
            if !self.eat(";") {
                break;
            }
        }
        if !self.at_end() {
            return Err(self.expected("';' or the end of the update"));
        }
        check_insert_data_blank_nodes(&operations).map_err(|label| {
            self.error_at(
                0,
                format!("the blank node _:{label} is used by two INSERT DATA operations"),
            )
        })?;
        Ok(Update {
            base_iri: self.base.clone(),
            operations,
        })
    }

    fn update_operation(&mut self) -> ParseResult<Vec<GraphUpdateOperation>> {
        let word = self.peek_word().to_ascii_uppercase();
        Ok(match word.as_str() {
            "LOAD" => {
                self.keyword("LOAD");
                let silent = self.keyword("SILENT");
                let source = self.iri()?;
                let destination = if self.keyword("INTO") {
                    GraphName::NamedNode(self.graph_ref()?)
                } else {
                    GraphName::DefaultGraph
                };
                vec![GraphUpdateOperation::Load {
                    silent,
                    source,
                    destination,
                }]
            }
            "CLEAR" | "DROP" => {
                self.keyword(&word);
                let silent = self.keyword("SILENT");
                let graph = self.graph_ref_all()?;
                vec![if word == "CLEAR" {
                    GraphUpdateOperation::Clear { silent, graph }
                } else {
                    GraphUpdateOperation::Drop { silent, graph }
                }]
            }
            "CREATE" => {
                self.keyword("CREATE");
                let silent = self.keyword("SILENT");
                vec![GraphUpdateOperation::Create {
                    silent,
                    graph: self.graph_ref()?,
                }]
            }
            "ADD" | "MOVE" | "COPY" => {
                self.keyword(&word);
                let silent = self.keyword("SILENT");
                let from = self.graph_or_default()?;
                self.expect_keyword("TO")?;
                let to = self.graph_or_default()?;
                // SPARQL 1.1 Update §3.2.3–3.2.5.
                if from == to {
                    Vec::new()
                } else {
                    let target = |g: &GraphName| match g {
                        GraphName::NamedNode(n) => GraphTarget::NamedNode(n.clone()),
                        GraphName::DefaultGraph => GraphTarget::DefaultGraph,
                    };
                    match word.as_str() {
                        "ADD" => vec![copy_graph(&from, &to)],
                        "MOVE" => vec![
                            GraphUpdateOperation::Drop {
                                silent: true,
                                graph: target(&to),
                            },
                            copy_graph(&from, &to),
                            GraphUpdateOperation::Drop {
                                silent,
                                graph: target(&from),
                            },
                        ],
                        _ => vec![
                            GraphUpdateOperation::Drop {
                                silent: true,
                                graph: target(&to),
                            },
                            copy_graph(&from, &to),
                        ],
                    }
                }
            }
            "INSERT" if self.data_follows("INSERT") => {
                self.keyword("INSERT");
                self.keyword("DATA");
                let at = self.peek_offset();
                let quads = self.quad_pattern()?;
                let data = quads
                    .into_iter()
                    .map(Quad::try_from)
                    .collect::<Result<_, _>>()
                    .map_err(|()| self.error_at(at, "INSERT DATA can't hold variables"))?;
                vec![GraphUpdateOperation::InsertData { data }]
            }
            "DELETE" if self.data_follows("DELETE") => {
                self.keyword("DELETE");
                self.keyword("DATA");
                let at = self.peek_offset();
                let quads = self.quad_pattern()?;
                let data = quads
                    .into_iter()
                    .map(|q| GroundQuad::try_from(Quad::try_from(q)?))
                    .collect::<Result<_, _>>()
                    .map_err(|()| {
                        self.error_at(at, "DELETE DATA can't hold variables or blank nodes")
                    })?;
                vec![GraphUpdateOperation::DeleteData { data }]
            }
            "DELETE" if self.where_follows() => {
                self.keyword("DELETE");
                self.keyword("WHERE");
                let at = self.peek_offset();
                let quads = self.quad_pattern()?;
                let pattern = quads
                    .iter()
                    .map(|q| {
                        let bgp = GraphPattern::Bgp {
                            patterns: vec![TriplePattern {
                                subject: q.subject.clone(),
                                predicate: q.predicate.clone(),
                                object: q.object.clone(),
                            }],
                        };
                        match &q.graph_name {
                            GraphNamePattern::DefaultGraph => bgp,
                            GraphNamePattern::NamedNode(g) => GraphPattern::Graph {
                                name: g.clone().into(),
                                inner: Box::new(bgp),
                            },
                            GraphNamePattern::Variable(g) => GraphPattern::Graph {
                                name: g.clone().into(),
                                inner: Box::new(bgp),
                            },
                        }
                    })
                    .reduce(new_join)
                    .unwrap_or_default();
                let delete = ground_quads(quads)
                    .map_err(|()| self.error_at(at, "DELETE WHERE can't hold blank nodes"))?;
                vec![GraphUpdateOperation::DeleteInsert {
                    delete,
                    insert: Vec::new(),
                    using: None,
                    pattern: Box::new(pattern),
                }]
            }
            "WITH" | "DELETE" | "INSERT" => vec![self.modify()?],
            _ => {
                return Err(self.expected(
                    "an update operation (INSERT, DELETE, LOAD, CLEAR, DROP, CREATE, ADD, MOVE, COPY or WITH)",
                ));
            }
        })
    }

    /// Whether `keyword DATA` comes next.
    fn data_follows(&mut self, keyword: &str) -> bool {
        let save = self.pos;
        let found = self.keyword(keyword) && self.looking_at_keyword("DATA");
        self.pos = save;
        found
    }

    /// Whether `DELETE WHERE` comes next.
    fn where_follows(&mut self) -> bool {
        let save = self.pos;
        let found = self.keyword("DELETE") && self.looking_at_keyword("WHERE");
        self.pos = save;
        found
    }

    /// `(WITH iri)? DELETE {…} INSERT {…} USING … WHERE {…}`.
    fn modify(&mut self) -> ParseResult<GraphUpdateOperation> {
        let with = if self.keyword("WITH") {
            Some(self.iri()?)
        } else {
            None
        };
        let mut delete = Vec::new();
        let mut insert = Vec::new();
        let mut any = false;
        if self.keyword("DELETE") {
            let at = self.peek_offset();
            let quads = self.quad_pattern()?;
            delete = ground_quads(quads)
                .map_err(|()| self.error_at(at, "a DELETE template can't hold blank nodes"))?;
            self.current_blank_nodes.clear();
            any = true;
        }
        if self.keyword("INSERT") {
            insert = self.quad_pattern()?;
            self.current_blank_nodes.clear();
            any = true;
        }
        if !any {
            return Err(self.expected("DELETE or INSERT"));
        }
        let mut using = self.dataset_clauses("USING")?;
        self.expect_keyword("WHERE")?;
        let pattern = self.group_graph_pattern()?;
        if let Some(with) = with {
            // WITH names the graph of the templates' default-graph quads and, without
            // USING, the graph the pattern reads.
            for q in &mut delete {
                if q.graph_name == GraphNamePattern::DefaultGraph {
                    q.graph_name = with.clone().into();
                }
            }
            for q in &mut insert {
                if q.graph_name == GraphNamePattern::DefaultGraph {
                    q.graph_name = with.clone().into();
                }
            }
            if using.is_none() {
                using = Some(QueryDataset {
                    default: vec![with],
                    named: None,
                });
            }
        }
        Ok(GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern: Box::new(pattern),
        })
    }

    /// `{ Quads }`: triples of the default graph and `GRAPH g { … }` blocks.
    fn quad_pattern(&mut self) -> ParseResult<Vec<QuadPattern>> {
        self.expect("{")?;
        let mut quads = Vec::new();
        loop {
            if self.at_triples_start() {
                for t in self.triples_template()? {
                    quads.push(QuadPattern {
                        subject: t.subject,
                        predicate: t.predicate,
                        object: t.object,
                        graph_name: GraphNamePattern::DefaultGraph,
                    });
                }
            }
            if self.keyword("GRAPH") {
                let graph: GraphNamePattern = self.var_or_iri()?.into();
                self.expect("{")?;
                if self.at_triples_start() {
                    for t in self.triples_template()? {
                        quads.push(QuadPattern {
                            subject: t.subject,
                            predicate: t.predicate,
                            object: t.object,
                            graph_name: graph.clone(),
                        });
                    }
                }
                self.expect("}")?;
                self.eat(".");
            } else {
                break;
            }
        }
        self.expect("}")?;
        Ok(quads)
    }

    fn graph_ref(&mut self) -> ParseResult<NamedNode> {
        self.expect_keyword("GRAPH")?;
        self.iri()
    }

    fn graph_ref_all(&mut self) -> ParseResult<GraphTarget> {
        if self.keyword("DEFAULT") {
            Ok(GraphTarget::DefaultGraph)
        } else if self.keyword("NAMED") {
            Ok(GraphTarget::NamedGraphs)
        } else if self.keyword("ALL") {
            Ok(GraphTarget::AllGraphs)
        } else {
            Ok(GraphTarget::NamedNode(self.graph_ref()?))
        }
    }

    fn graph_or_default(&mut self) -> ParseResult<GraphName> {
        if self.keyword("DEFAULT") {
            return Ok(GraphName::DefaultGraph);
        }
        self.keyword("GRAPH");
        Ok(GraphName::NamedNode(self.iri()?))
    }
}

fn ground_quads(quads: Vec<QuadPattern>) -> Result<Vec<GroundQuadPattern>, ()> {
    quads.into_iter().map(GroundQuadPattern::try_from).collect()
}

/// Whether every variable `expression` reads is in `visible` (aggregates and `EXISTS`
/// stand for themselves).
fn bound_in(expression: &Expression, visible: &HashSet<Variable>) -> bool {
    match expression {
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Bound(_)
        | Expression::Coalesce(_)
        | Expression::Exists(_) => true,
        Expression::Variable(v) => visible.contains(v),
        Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
            bound_in(e, visible)
        }
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
        | Expression::Divide(a, b) => bound_in(a, visible) && bound_in(b, visible),
        Expression::In(a, list) => {
            bound_in(a, visible) && list.iter().all(|e| bound_in(e, visible))
        }
        Expression::FunctionCall(_, args) => args.iter().all(|e| bound_in(e, visible)),
        Expression::If(a, b, c) => {
            bound_in(a, visible) && bound_in(b, visible) && bound_in(c, visible)
        }
    }
}

/// `ADD`'s copy of every triple of `from` into `to`.
fn copy_graph(from: &GraphName, to: &GraphName) -> GraphUpdateOperation {
    let [s, p, o] = ["s", "p", "o"].map(Variable::new_unchecked);
    let bgp = GraphPattern::Bgp {
        patterns: vec![TriplePattern::new(s.clone(), p.clone(), o.clone())],
    };
    GraphUpdateOperation::DeleteInsert {
        delete: Vec::new(),
        insert: vec![QuadPattern::new(
            s,
            p,
            o,
            GraphNamePattern::from(to.clone()),
        )],
        using: None,
        pattern: Box::new(match from {
            GraphName::NamedNode(from) => GraphPattern::Graph {
                name: from.clone().into(),
                inner: Box::new(bgp),
            },
            GraphName::DefaultGraph => bgp,
        }),
    }
}

/// Two `INSERT DATA` operations of one request may not share a blank node label; the
/// first shared label, if any.
fn check_insert_data_blank_nodes(operations: &[GraphUpdateOperation]) -> Result<(), String> {
    fn triple_nodes<'t>(triple: &'t Triple, out: &mut HashSet<&'t BlankNode>) {
        if let NamedOrBlankNode::BlankNode(b) = &triple.subject {
            out.insert(b);
        }
        term_nodes(&triple.object, out);
    }
    fn term_nodes<'t>(term: &'t Term, out: &mut HashSet<&'t BlankNode>) {
        match term {
            Term::BlankNode(b) => {
                out.insert(b);
            }
            Term::Triple(t) => triple_nodes(t, out),
            Term::NamedNode(_) | Term::Literal(_) => {}
        }
    }
    let mut seen: HashSet<&BlankNode> = HashSet::new();
    for operation in operations {
        if let GraphUpdateOperation::InsertData { data } = operation {
            let mut here = HashSet::new();
            for quad in data {
                if let NamedOrBlankNode::BlankNode(b) = &quad.subject {
                    here.insert(b);
                }
                term_nodes(&quad.object, &mut here);
            }
            if let Some(shared) = seen.intersection(&here).next() {
                return Err(shared.as_str().to_owned());
            }
            seen.extend(here);
        }
    }
    Ok(())
}
