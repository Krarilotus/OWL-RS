//! Graph patterns (SPARQL 1.1 §18.2.2): groups and how their elements fold into the
//! algebra, triples with property paths, collections, the SPARQL 1.2 reification syntax,
//! and inline data.

use std::collections::HashSet;
use std::mem::take;

use nrese_rdf::vocab::rdf;
use nrese_rdf::{NamedNode, Variable};

use super::{ParseResult, Parser};
use crate::algebra::{Expression, GraphPattern, PropertyPathExpression};
use crate::term::{GroundTerm, GroundTriple, NamedNodePattern, TermPattern, TriplePattern};

/// A triple pattern or a path between two terms, in the order the text gives them.
#[derive(Debug)]
pub(super) enum TripleOrPath {
    Triple(TriplePattern),
    Path {
        subject: TermPattern,
        path: PropertyPathExpression,
        object: TermPattern,
    },
}

/// An object and the reifiers its annotations name.
#[derive(Clone)]
pub(super) struct ReifiedTerm {
    term: TermPattern,
    reifiers: Vec<TermPattern>,
}

/// The predicate position: a variable, or a path (an IRI is a path of one step).
#[derive(Clone)]
pub(super) enum Verb {
    Variable(Variable),
    Path(PropertyPathExpression),
}

/// A term and the patterns its syntax adds (a collection's list, a blank node's
/// properties).
pub(super) struct Focused<T> {
    focus: T,
    patterns: Vec<TripleOrPath>,
}

type PropertyList = Focused<Vec<(Verb, Vec<ReifiedTerm>)>>;

/// An element of a group after its triples (SPARQL 1.1 §18.2.2.6).
enum Element {
    Optional(GraphPattern, Option<Expression>),
    Lateral(GraphPattern),
    Minus(GraphPattern),
    Bind(Expression, Variable),
    Filter(Expression),
    Other(GraphPattern),
}

impl<'a> Parser<'a> {
    // --- Groups -----------------------------------------------------------------------

    /// `{ … }`: a group, or a subquery.
    pub(super) fn group_graph_pattern(&mut self) -> ParseResult<GraphPattern> {
        self.enter()?;
        self.expect("{")?;
        self.close_blank_node_scope();
        let pattern = if self.looking_at_keyword("SELECT") {
            self.sub_select()?
        } else {
            self.group_graph_pattern_sub()?
        };
        self.close_blank_node_scope();
        self.expect("}")?;
        self.leave();
        Ok(pattern)
    }

    fn group_graph_pattern_sub(&mut self) -> ParseResult<GraphPattern> {
        let mut g = if self.at_triples_start() {
            build_bgp(self.triples_block()?)
        } else {
            GraphPattern::default()
        };
        // A group's filters, joined by `&&` as a balanced tree (as a chain of `&&` is).
        let mut filters: Vec<Expression> = Vec::new();
        while !matches!(self.peek(), Some(b'}') | None) {
            match self.graph_pattern_not_triples()? {
                Element::Optional(p, expression) => {
                    g = GraphPattern::LeftJoin {
                        left: Box::new(g),
                        right: Box::new(p),
                        expression,
                    };
                }
                Element::Lateral(p) => {
                    let mut defined = HashSet::new();
                    defined_variables(&p, &mut defined);
                    let mut overridden = false;
                    g.on_in_scope_variable(|v| overridden |= defined.contains(v));
                    if overridden {
                        return Err(self.error(
                            "the right side of LATERAL assigns a variable already bound on its left",
                        ));
                    }
                    g = GraphPattern::Lateral {
                        left: Box::new(g),
                        right: Box::new(p),
                    };
                }
                Element::Minus(p) => {
                    g = GraphPattern::Minus {
                        left: Box::new(g),
                        right: Box::new(p),
                    };
                }
                Element::Bind(expression, variable) => {
                    let mut bound = false;
                    g.on_in_scope_variable(|v| bound |= *v == variable);
                    if bound {
                        return Err(self.error(format!(
                            "BIND assigns {variable}, which the group already binds"
                        )));
                    }
                    g = GraphPattern::Extend {
                        inner: Box::new(g),
                        variable,
                        expression,
                    };
                }
                Element::Filter(expr) => filters.push(expr),
                Element::Other(p) => g = new_join(g, p),
            }
            self.eat(".");
            if self.at_triples_start() {
                // Element by element: a block's triples continue the basic graph pattern
                // before it (a FILTER between them doesn't end it, and a blank node label
                // may span it), also when the block holds paths (found by fuzzing,
                // `nrese-fuzz`: printed, `Join(g, Join(bgp, path))` became a group of its own,
                // across which the label was refused).
                for element in bgp_elements(self.triples_block()?) {
                    g = new_join(g, element);
                }
            }
        }
        let filter = (!filters.is_empty()).then(|| super::expr::balanced(filters, Expression::And));
        Ok(match filter {
            Some(expr) => GraphPattern::Filter {
                expr,
                inner: Box::new(g),
            },
            None => g,
        })
    }

    fn graph_pattern_not_triples(&mut self) -> ParseResult<Element> {
        if self.peek() == Some(b'{') {
            let mut pattern = self.group_graph_pattern()?;
            while self.keyword("UNION") {
                pattern = GraphPattern::Union {
                    left: Box::new(pattern),
                    right: Box::new(self.group_graph_pattern()?),
                };
            }
            return Ok(Element::Other(pattern));
        }
        let word = self.peek_word();
        let element = match word.to_ascii_uppercase().as_str() {
            "OPTIONAL" => {
                self.keyword("OPTIONAL");
                match self.group_graph_pattern()? {
                    GraphPattern::Filter { expr, inner } => Element::Optional(*inner, Some(expr)),
                    p => Element::Optional(p, None),
                }
            }
            "LATERAL" if self.options.lateral => {
                self.keyword("LATERAL");
                Element::Lateral(self.group_graph_pattern()?)
            }
            "MINUS" => {
                self.keyword("MINUS");
                Element::Minus(self.group_graph_pattern()?)
            }
            "GRAPH" => {
                self.keyword("GRAPH");
                let name = self.var_or_iri()?;
                Element::Other(GraphPattern::Graph {
                    name,
                    inner: Box::new(self.group_graph_pattern()?),
                })
            }
            "SERVICE" => {
                self.keyword("SERVICE");
                let silent = self.keyword("SILENT");
                let name = self.var_or_iri()?;
                Element::Other(GraphPattern::Service {
                    name,
                    inner: Box::new(self.group_graph_pattern()?),
                    silent,
                })
            }
            "FILTER" => {
                self.keyword("FILTER");
                Element::Filter(self.constraint()?)
            }
            "BIND" => {
                self.keyword("BIND");
                self.expect("(")?;
                let expression = self.expression()?;
                self.expect_keyword("AS")?;
                let variable = self.variable()?;
                self.expect(")")?;
                Element::Bind(expression, variable)
            }
            "VALUES" => {
                self.keyword("VALUES");
                Element::Other(self.data_block()?)
            }
            _ => {
                return Err(self.expected(
                    "a triple pattern, '{', OPTIONAL, MINUS, GRAPH, SERVICE, FILTER, BIND, VALUES or '}'",
                ));
            }
        };
        Ok(element)
    }

    pub(super) fn var_or_iri(&mut self) -> ParseResult<NamedNodePattern> {
        if let Some(v) = self.try_variable()? {
            return Ok(v.into());
        }
        Ok(self.iri()?.into())
    }

    // --- Triples ----------------------------------------------------------------------

    /// Whether a triple pattern can start here.
    pub(super) fn at_triples_start(&mut self) -> bool {
        let Some(b) = self.peek() else { return false };
        match b {
            b'?' | b'$' => self.at_variable(),
            b'<' | b'"' | b'\'' | b'[' | b'(' | b':' => true,
            b'_' => self.byte_at(self.pos + 1) == Some(b':'),
            b'0'..=b'9' | b'.' | b'+' | b'-' => self.at_number(true),
            _ => {
                self.at_prefixed_name()
                    || self.looking_at_keyword("true")
                    || self.looking_at_keyword("false")
            }
        }
    }

    /// `TriplesBlock`: triples (with paths) separated by `.`.
    fn triples_block(&mut self) -> ParseResult<Vec<TripleOrPath>> {
        let mut out = Vec::new();
        loop {
            self.triples_same_subject(true, &mut out)?;
            if !self.eat(".") || !self.at_triples_start() {
                return Ok(out);
            }
        }
    }

    /// `TriplesTemplate` (no paths): the triples of a template, separated by `.`.
    pub(super) fn triples_template(&mut self) -> ParseResult<Vec<TriplePattern>> {
        let mut out = Vec::new();
        loop {
            self.triples_same_subject(false, &mut out)?;
            if !self.eat(".") || !self.at_triples_start() {
                break;
            }
        }
        Ok(out
            .into_iter()
            .filter_map(|t| match t {
                TripleOrPath::Triple(t) => Some(t),
                TripleOrPath::Path { .. } => None, // no paths without `paths`
            })
            .collect())
    }

    /// `TriplesSameSubject(Path)`: a subject and its properties.
    fn triples_same_subject(
        &mut self,
        paths: bool,
        out: &mut Vec<TripleOrPath>,
    ) -> ParseResult<()> {
        if self.at_reified_triple() {
            let subject = self.reified_triple()?;
            let properties = self.property_list(paths, false)?;
            out.extend(properties.patterns);
            out.extend(subject.patterns);
            self.add_properties(subject.focus, properties.focus, out)
        } else if self.at_triples_node() {
            let subject = self.triples_node(paths)?;
            let properties = self.property_list(paths, false)?;
            out.extend(subject.patterns);
            out.extend(properties.patterns);
            self.add_properties(subject.focus, properties.focus, out)
        } else {
            let subject = self.var_or_term()?;
            let properties = self.property_list(paths, true)?;
            out.extend(properties.patterns);
            self.add_properties(subject, properties.focus, out)
        }
    }

    /// The triples of `subject` and its properties; the last one takes the subject and
    /// each verb's last object its verb, without a copy.
    fn add_properties(
        &mut self,
        subject: TermPattern,
        properties: Vec<(Verb, Vec<ReifiedTerm>)>,
        out: &mut Vec<TripleOrPath>,
    ) -> ParseResult<()> {
        let mut subject = Some(subject);
        let verbs = properties.len();
        for (i, (verb, objects)) in properties.into_iter().enumerate() {
            let mut verb = Some(verb);
            let count = objects.len();
            for (j, object) in objects.into_iter().enumerate() {
                let last_object = j + 1 == count;
                let s = if last_object && i + 1 == verbs {
                    subject.take()
                } else {
                    subject.clone()
                };
                let v = if last_object {
                    verb.take()
                } else {
                    verb.clone()
                };
                if let (Some(s), Some(v)) = (s, v) {
                    self.add_triple_or_path(s, v, object, out)?;
                }
            }
        }
        Ok(())
    }

    /// `PropertyList(Path)(NotEmpty)`: verbs and their objects, separated by `;`.
    fn property_list(&mut self, paths: bool, required: bool) -> ParseResult<PropertyList> {
        let mut list = PropertyList {
            focus: Vec::new(),
            patterns: Vec::new(),
        };
        if !required && !self.at_verb(paths) {
            return Ok(list);
        }
        loop {
            let verb = self.verb(paths)?;
            let mut objects = Vec::new();
            loop {
                let object = self.object(paths)?;
                list.patterns.extend(object.patterns);
                objects.push(object.focus);
                if !self.eat(",") {
                    break;
                }
            }
            list.focus.push((verb, objects));
            let mut separated = false;
            while self.eat(";") {
                separated = true;
            }
            if !separated || !self.at_verb(paths) {
                return Ok(list);
            }
        }
    }

    fn at_keyword_a(&mut self) -> bool {
        self.peek() == Some(b'a') && !self.byte_at(self.pos + 1).is_some_and(super::is_word_byte)
    }

    fn at_verb(&mut self, paths: bool) -> bool {
        match self.peek() {
            Some(b'?' | b'$') => self.at_variable(),
            Some(b'^' | b'!' | b'(') => paths,
            _ => self.at_keyword_a() || self.at_iri(),
        }
    }

    fn verb(&mut self, paths: bool) -> ParseResult<Verb> {
        if let Some(v) = self.try_variable()? {
            return Ok(Verb::Variable(v));
        }
        if paths {
            return Ok(Verb::Path(self.path()?));
        }
        Ok(Verb::Path(self.iri_or_a()?.into()))
    }

    /// An IRI or `a` (`rdf:type`).
    fn iri_or_a(&mut self) -> ParseResult<NamedNode> {
        if self.at_keyword_a() {
            self.pos += 1;
            return Ok(rdf::TYPE.into_owned());
        }
        match self.try_iri()? {
            Some(iri) => Ok(iri),
            None => Err(self.expected("a predicate (an IRI, 'a' or a variable)")),
        }
    }

    /// `Verb` of a triple term or reified triple: a variable, an IRI or `a`.
    fn simple_verb(&mut self) -> ParseResult<NamedNodePattern> {
        if let Some(v) = self.try_variable()? {
            return Ok(v.into());
        }
        Ok(self.iri_or_a()?.into())
    }

    /// `Object(Path)`: a node and its annotations.
    fn object(&mut self, paths: bool) -> ParseResult<Focused<ReifiedTerm>> {
        let node = self.graph_node(paths)?;
        let annotation = self.annotation(paths)?;
        let mut patterns = node.patterns;
        patterns.extend(annotation.patterns);
        Ok(Focused {
            focus: ReifiedTerm {
                term: node.focus,
                reifiers: annotation.focus,
            },
            patterns,
        })
    }

    /// `GraphNode(Path)`.
    fn graph_node(&mut self, paths: bool) -> ParseResult<Focused<TermPattern>> {
        if self.at_reified_triple() {
            return self.reified_triple();
        }
        if self.at_triples_node() {
            return self.triples_node(paths);
        }
        Ok(Focused {
            focus: self.var_or_term()?,
            patterns: Vec::new(),
        })
    }

    /// `[` with properties or `(` with members (not `[]` or `()`).
    fn at_triples_node(&mut self) -> bool {
        let Some(b) = self.peek() else { return false };
        if b != b'[' && b != b'(' {
            return false;
        }
        let save = self.pos;
        self.pos += 1;
        let empty = self.peek() == Some(if b == b'[' { b']' } else { b')' });
        self.pos = save;
        !empty
    }

    fn triples_node(&mut self, paths: bool) -> ParseResult<Focused<TermPattern>> {
        self.enter()?;
        let node = if self.eat("[") {
            let properties = self.property_list(paths, true)?;
            self.expect("]")?;
            let node = TermPattern::from(self.fresh_blank_node());
            let mut patterns = properties.patterns;
            self.add_properties(node.clone(), properties.focus, &mut patterns)?;
            Focused {
                focus: node,
                patterns,
            }
        } else {
            self.expect("(")?;
            let mut members = Vec::new();
            while self.peek() != Some(b')') {
                if self.at_end() {
                    return Err(self.expected("')'"));
                }
                members.push(self.graph_node(paths)?);
            }
            self.expect(")")?;
            let mut patterns = Vec::new();
            let mut list = TermPattern::from(rdf::NIL.into_owned());
            for member in members.into_iter().rev() {
                let node = TermPattern::from(self.fresh_blank_node());
                patterns.push(TripleOrPath::Triple(TriplePattern::new(
                    node.clone(),
                    rdf::FIRST.into_owned(),
                    member.focus,
                )));
                patterns.push(TripleOrPath::Triple(TriplePattern::new(
                    node.clone(),
                    rdf::REST.into_owned(),
                    list,
                )));
                list = node;
                patterns.extend(member.patterns);
            }
            Focused {
                focus: list,
                patterns,
            }
        };
        self.leave();
        Ok(node)
    }

    /// `VarOrTerm`: a variable, an IRI, a literal, a blank node, `()` or a triple term.
    fn var_or_term(&mut self) -> ParseResult<TermPattern> {
        match self.try_term_pattern()? {
            Some(term) => Ok(term),
            None => {
                Err(self.expected("a subject or object (a variable, IRI, literal or blank node)"))
            }
        }
    }

    fn try_term_pattern(&mut self) -> ParseResult<Option<TermPattern>> {
        if let Some(v) = self.try_variable()? {
            return Ok(Some(v.into()));
        }
        if self.looking_at("<<(") {
            return Ok(Some(self.triple_term()?.into()));
        }
        if let Some(iri) = self.try_iri()? {
            return Ok(Some(iri.into()));
        }
        if let Some(literal) = self.try_rdf_literal()? {
            return Ok(Some(literal.into()));
        }
        if self.at_number(true) {
            return Ok(Some(self.numeric_literal(true)?.into()));
        }
        if let Some(literal) = self.try_boolean() {
            return Ok(Some(literal.into()));
        }
        if let Some(node) = self.try_blank_node()? {
            return Ok(Some(node.into()));
        }
        if self.peek() == Some(b'(') {
            let save = self.pos;
            self.pos += 1;
            if self.eat(")") {
                return Ok(Some(rdf::NIL.into_owned().into()));
            }
            self.pos = save;
        }
        Ok(None)
    }

    // --- SPARQL 1.2: triple terms, reified triples, annotations ----------------------

    fn require_sparql_12(&mut self, what: &str) -> ParseResult<()> {
        if self.options.sparql_12 {
            Ok(())
        } else {
            Err(self.error(format!("{what} need SPARQL 1.2")))
        }
    }

    /// `<<` that starts a reified triple (not `<<(`).
    fn at_reified_triple(&mut self) -> bool {
        self.looking_at("<<") && !self.looking_at("<<(")
    }

    /// `<<( s p o )>>`.
    pub(super) fn triple_term(&mut self) -> ParseResult<TriplePattern> {
        self.require_sparql_12("triple terms")?;
        self.enter()?;
        self.expect("<<(")?;
        let subject = self.triple_term_part()?;
        let predicate = self.simple_verb()?;
        let object = self.triple_term_part()?;
        self.expect(")>>")?;
        self.leave();
        Ok(TriplePattern {
            subject,
            predicate,
            object,
        })
    }

    /// A part of a triple term. Patterns may hold any term in either position (a
    /// pattern with a literal or triple term as subject matches nothing); values and
    /// expressions may not (see `triple_term_data` and the expression parser).
    fn triple_term_part(&mut self) -> ParseResult<TermPattern> {
        if self.peek() == Some(b'(') {
            return Err(self.expected("a term of a triple term"));
        }
        self.var_or_term()
    }

    /// `<< s p o ~ r >>`: the reifier, which `rdf:reifies` the triple term.
    fn reified_triple(&mut self) -> ParseResult<Focused<TermPattern>> {
        self.require_sparql_12("reified triples")?;
        self.enter()?;
        self.expect("<<")?;
        let subject = self.reified_triple_part()?;
        let predicate = self.simple_verb()?;
        let object = self.reified_triple_part()?;
        let reifier = if self.looking_at("~") {
            self.reifier()?
        } else {
            self.fresh_blank_node().into()
        };
        self.expect(">>")?;
        self.leave();
        let mut patterns = vec![TripleOrPath::Triple(TriplePattern::new(
            reifier.clone(),
            rdf::REIFIES.into_owned(),
            TriplePattern {
                subject: subject.focus,
                predicate,
                object: object.focus,
            },
        ))];
        patterns.extend(subject.patterns);
        patterns.extend(object.patterns);
        Ok(Focused {
            focus: reifier,
            patterns,
        })
    }

    fn reified_triple_part(&mut self) -> ParseResult<Focused<TermPattern>> {
        if self.at_reified_triple() {
            return self.reified_triple();
        }
        if self.peek() == Some(b'(') {
            return Err(self.expected("a term of a reified triple"));
        }
        Ok(Focused {
            focus: self.var_or_term()?,
            patterns: Vec::new(),
        })
    }

    /// `~` and an optional reifier (a variable, IRI or blank node; a fresh blank node if
    /// none).
    fn reifier(&mut self) -> ParseResult<TermPattern> {
        self.require_sparql_12("reifiers")?;
        self.expect("~")?;
        if let Some(v) = self.try_variable()? {
            return Ok(v.into());
        }
        if let Some(iri) = self.try_iri()? {
            return Ok(iri.into());
        }
        if let Some(node) = self.try_blank_node()? {
            return Ok(node.into());
        }
        Ok(self.fresh_blank_node().into())
    }

    /// `Annotation(Path)`: reifiers and `{| … |}` blocks after an object.
    fn annotation(&mut self, paths: bool) -> ParseResult<Focused<Vec<TermPattern>>> {
        let mut out = Focused {
            focus: Vec::new(),
            patterns: Vec::new(),
        };
        loop {
            let reifier = if self.looking_at("~") {
                self.reifier()?
            } else if self.looking_at("{|") {
                self.require_sparql_12("annotations")?;
                self.fresh_blank_node().into()
            } else {
                return Ok(out);
            };
            if self.eat("{|") {
                let properties = self.property_list(paths, true)?;
                self.expect("|}")?;
                self.add_properties(reifier.clone(), properties.focus, &mut out.patterns)?;
                out.patterns.extend(properties.patterns);
            }
            out.focus.push(reifier);
        }
    }

    // --- From the syntax to triple patterns and paths --------------------------------

    fn add_triple_or_path(
        &mut self,
        subject: TermPattern,
        verb: Verb,
        object: ReifiedTerm,
        out: &mut Vec<TripleOrPath>,
    ) -> ParseResult<()> {
        match verb {
            Verb::Variable(p) => {
                add_triple(subject, p.into(), object, out);
                Ok(())
            }
            Verb::Path(path) => self.add_path(subject, path, object, out),
        }
    }

    /// A path in the predicate position: one step is a triple, `^` swaps the ends, `/`
    /// joins steps through a fresh blank node (SPARQL 1.1 §18.2.2.4); the rest stays a
    /// path.
    fn add_path(
        &mut self,
        subject: TermPattern,
        path: PropertyPathExpression,
        object: ReifiedTerm,
        out: &mut Vec<TripleOrPath>,
    ) -> ParseResult<()> {
        match path {
            PropertyPathExpression::NamedNode(p) => {
                add_triple(subject, p.into(), object, out);
                Ok(())
            }
            PropertyPathExpression::Reverse(p) => self.add_path(
                object.term,
                *p,
                ReifiedTerm {
                    term: subject,
                    reifiers: object.reifiers,
                },
                out,
            ),
            PropertyPathExpression::Sequence(a, b) => {
                if !object.reifiers.is_empty() {
                    return Err(self.error("a path of several steps can't be reified"));
                }
                let middle = TermPattern::from(self.fresh_blank_node());
                self.add_path(
                    subject,
                    *a,
                    ReifiedTerm {
                        term: middle.clone(),
                        reifiers: Vec::new(),
                    },
                    out,
                )?;
                self.add_path(
                    middle,
                    *b,
                    ReifiedTerm {
                        term: object.term,
                        reifiers: Vec::new(),
                    },
                    out,
                )
            }
            path => {
                if !object.reifiers.is_empty() {
                    return Err(self.error("a property path can't be reified"));
                }
                out.push(TripleOrPath::Path {
                    subject,
                    path,
                    object: object.term,
                });
                Ok(())
            }
        }
    }

    // --- Property paths ---------------------------------------------------------------

    /// `Path`: alternatives of sequences.
    fn path(&mut self) -> ParseResult<PropertyPathExpression> {
        let mut path = self.path_sequence()?;
        while self.eat("|") {
            path = PropertyPathExpression::Alternative(
                Box::new(path),
                Box::new(self.path_sequence()?),
            );
        }
        Ok(path)
    }

    fn path_sequence(&mut self) -> ParseResult<PropertyPathExpression> {
        let mut path = self.path_elt_or_inverse()?;
        while self.eat("/") {
            path = PropertyPathExpression::Sequence(
                Box::new(path),
                Box::new(self.path_elt_or_inverse()?),
            );
        }
        Ok(path)
    }

    fn path_elt_or_inverse(&mut self) -> ParseResult<PropertyPathExpression> {
        if self.eat("^") {
            return Ok(PropertyPathExpression::Reverse(Box::new(self.path_elt()?)));
        }
        self.path_elt()
    }

    fn path_elt(&mut self) -> ParseResult<PropertyPathExpression> {
        let primary = self.path_primary()?;
        Ok(match self.peek() {
            Some(b'*') => {
                self.pos += 1;
                PropertyPathExpression::ZeroOrMore(Box::new(primary))
            }
            Some(b'+') => {
                self.pos += 1;
                PropertyPathExpression::OneOrMore(Box::new(primary))
            }
            // `?` is a modifier unless a variable name follows it.
            Some(b'?') if !self.at_variable() => {
                self.pos += 1;
                PropertyPathExpression::ZeroOrOne(Box::new(primary))
            }
            _ => primary,
        })
    }

    fn path_primary(&mut self) -> ParseResult<PropertyPathExpression> {
        if self.eat("!") {
            return self.negated_property_set();
        }
        if self.peek() == Some(b'(') {
            self.enter()?;
            self.pos += 1;
            let path = self.path()?;
            self.expect(")")?;
            self.leave();
            return Ok(path);
        }
        Ok(self.iri_or_a()?.into())
    }

    /// After `!`: one IRI (or `^IRI`), or `( … | … )`. Forward and inverse members
    /// become a negated set each, joined by `|` when both occur.
    fn negated_property_set(&mut self) -> ParseResult<PropertyPathExpression> {
        let mut forward = Vec::new();
        let mut inverse = Vec::new();
        let mut member = |p: &mut Self| -> ParseResult<()> {
            if p.eat("^") {
                inverse.push(p.iri_or_a()?);
            } else {
                forward.push(p.iri_or_a()?);
            }
            Ok(())
        };
        if self.eat("(") {
            if !self.eat(")") {
                loop {
                    member(self)?;
                    if !self.eat("|") {
                        break;
                    }
                }
                self.expect(")")?;
            }
        } else {
            member(self)?;
        }
        let reverse = |set| {
            PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(
                set,
            )))
        };
        Ok(if inverse.is_empty() {
            PropertyPathExpression::NegatedPropertySet(forward)
        } else if forward.is_empty() {
            reverse(inverse)
        } else {
            PropertyPathExpression::Alternative(
                Box::new(PropertyPathExpression::NegatedPropertySet(forward)),
                Box::new(reverse(inverse)),
            )
        })
    }

    // --- Inline data ------------------------------------------------------------------

    /// `DataBlock` after `VALUES`.
    pub(super) fn data_block(&mut self) -> ParseResult<GraphPattern> {
        let start = self.peek_offset();
        if let Some(variable) = self.try_variable()? {
            self.expect("{")?;
            let mut bindings = Vec::new();
            while !self.eat("}") {
                bindings.push(vec![self.data_block_value()?]);
            }
            return Ok(GraphPattern::Values {
                variables: vec![variable],
                bindings,
            });
        }
        self.expect("(")?;
        let mut variables = Vec::new();
        while !self.eat(")") {
            let variable = self.variable()?;
            if variables.contains(&variable) {
                return Err(self.error_at(start, format!("{variable} twice in VALUES")));
            }
            variables.push(variable);
        }
        self.expect("{")?;
        let mut bindings = Vec::new();
        while !self.eat("}") {
            let row_start = self.peek_offset();
            self.expect("(")?;
            let mut row = Vec::with_capacity(variables.len());
            while !self.eat(")") {
                row.push(self.data_block_value()?);
            }
            if row.len() != variables.len() {
                return Err(self.error_at(
                    row_start,
                    format!(
                        "a row of {} values for {} variables in VALUES",
                        row.len(),
                        variables.len()
                    ),
                ));
            }
            bindings.push(row);
        }
        Ok(GraphPattern::Values {
            variables,
            bindings,
        })
    }

    /// A value of `VALUES`, `None` for `UNDEF`.
    fn data_block_value(&mut self) -> ParseResult<Option<GroundTerm>> {
        if self.keyword("UNDEF") {
            return Ok(None);
        }
        Ok(Some(self.ground_term()?))
    }

    fn ground_term(&mut self) -> ParseResult<GroundTerm> {
        if self.looking_at("<<(") {
            return Ok(self.triple_term_data()?.into());
        }
        if let Some(iri) = self.try_iri()? {
            return Ok(iri.into());
        }
        if let Some(literal) = self.try_rdf_literal()? {
            return Ok(literal.into());
        }
        if self.at_number(true) {
            return Ok(self.numeric_literal(true)?.into());
        }
        if let Some(literal) = self.try_boolean() {
            return Ok(literal.into());
        }
        Err(self.expected("a value (an IRI, a literal or UNDEF)"))
    }

    /// `<<( s p o )>>` of constants.
    fn triple_term_data(&mut self) -> ParseResult<GroundTriple> {
        self.require_sparql_12("triple terms")?;
        self.enter()?;
        self.expect("<<(")?;
        let at = self.peek_offset();
        let subject = match self.ground_term()? {
            GroundTerm::NamedNode(n) => n,
            _ => return Err(self.error_at(at, "the subject of a triple term must be an IRI")),
        };
        let predicate = self.iri_or_a()?;
        let object = self.ground_term()?;
        self.expect(")>>")?;
        self.leave();
        Ok(GroundTriple {
            subject,
            predicate,
            object,
        })
    }
}

fn add_triple(
    subject: TermPattern,
    predicate: NamedNodePattern,
    object: ReifiedTerm,
    out: &mut Vec<TripleOrPath>,
) {
    let triple = TriplePattern {
        subject,
        predicate,
        object: object.term,
    };
    for reifier in object.reifiers {
        out.push(TripleOrPath::Triple(TriplePattern::new(
            reifier,
            rdf::REIFIES.into_owned(),
            triple.clone(),
        )));
    }
    out.push(TripleOrPath::Triple(triple));
}

/// The triples of a block as a basic graph pattern, paths joined in between.
pub(super) fn build_bgp(patterns: Vec<TripleOrPath>) -> GraphPattern {
    bgp_elements(patterns)
        .into_iter()
        .reduce(new_join)
        .unwrap_or_default()
}

/// A triples block's basic graph patterns and paths, in order.
fn bgp_elements(patterns: Vec<TripleOrPath>) -> Vec<GraphPattern> {
    let mut bgp = Vec::new();
    let mut elements = Vec::new();
    for pattern in patterns {
        match pattern {
            TripleOrPath::Triple(t) => bgp.push(t),
            TripleOrPath::Path {
                subject,
                path,
                object,
            } => {
                if !bgp.is_empty() {
                    elements.push(GraphPattern::Bgp {
                        patterns: take(&mut bgp),
                    });
                }
                elements.push(GraphPattern::Path {
                    subject,
                    path,
                    object,
                });
            }
        }
    }
    if !bgp.is_empty() {
        elements.push(GraphPattern::Bgp { patterns: bgp });
    }
    elements
}

/// `Join`, dropping empty basic graph patterns and merging adjacent ones.
pub(super) fn new_join(left: GraphPattern, right: GraphPattern) -> GraphPattern {
    match (left, right) {
        (GraphPattern::Bgp { patterns }, other) | (other, GraphPattern::Bgp { patterns })
            if patterns.is_empty() =>
        {
            other
        }
        (GraphPattern::Bgp { patterns: mut left }, GraphPattern::Bgp { patterns: right }) => {
            left.extend(right);
            GraphPattern::Bgp { patterns: left }
        }
        (left, right) => GraphPattern::Join {
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

/// The variables a pattern assigns with `AS`, `BIND` or `VALUES` (what `LATERAL` may not
/// assign again).
fn defined_variables<'p>(pattern: &'p GraphPattern, set: &mut HashSet<&'p Variable>) {
    match pattern {
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } => {}
        GraphPattern::Join { left, right }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Lateral { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            defined_variables(left, set);
            defined_variables(right, set);
        }
        GraphPattern::Extend {
            inner, variable, ..
        } => {
            set.insert(variable);
            defined_variables(inner, set);
        }
        GraphPattern::Group {
            variables,
            aggregates,
            inner,
        } => {
            for (v, _) in aggregates {
                set.insert(v);
            }
            let mut inner_set = HashSet::new();
            defined_variables(inner, &mut inner_set);
            set.extend(inner_set.into_iter().filter(|v| variables.contains(v)));
        }
        GraphPattern::Values { variables, .. } => set.extend(variables),
        GraphPattern::Project { variables, inner } => {
            let mut inner_set = HashSet::new();
            defined_variables(inner, &mut inner_set);
            set.extend(inner_set.into_iter().filter(|v| variables.contains(v)));
        }
        GraphPattern::Graph { inner, .. }
        | GraphPattern::Service { inner, .. }
        | GraphPattern::Filter { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => defined_variables(inner, set),
    }
}
