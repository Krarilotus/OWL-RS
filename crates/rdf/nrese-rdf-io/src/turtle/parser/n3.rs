//! Notation3 (W3C N3 Community Group, `grammar/n3.g4`) on the Turtle parser's input,
//! arena and term helpers: formulas `{ … }` (their triples carry the formula's blank node,
//! and the empty formula is the literal `true`), quick variables `?x`, paths `a!b` and
//! `a^b`, the verbs `has`, `is … of`, `<-`, `=`, `=>`, `<=`, `[ id <iri> … ]`, and any
//! term in any position.

use std::io::Read;

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, NamedNodeRef, Variable};

use super::{Kind, Lit, Step, Term, Token, Triple, TurtleParser};
use crate::n3::{N3Quad, N3Term};

const OWL_SAME_AS: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
const LOG_IMPLIES: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2000/10/swap/log#implies");
const LOG_IS_IMPLIED_BY: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2000/10/swap/log#isImpliedBy");

impl<R: Read> TurtleParser<'_, R> {
    /// The next N3 statement and its quads, owned; `None` at the end.
    pub(crate) fn next_n3(&mut self) -> Option<Step<N3Quad>> {
        loop {
            if self.handed_out < self.triples.len() {
                let triple = self.triples[self.handed_out];
                self.handed_out += 1;
                return Some(Ok(self.n3_quad(triple)));
            }
            if self.done {
                return None;
            }
            self.arena.clear();
            self.triples.clear();
            self.handed_out = 0;
            if let Err(error) = self.statement() {
                self.done = true;
                return Some(Err(error));
            }
        }
    }

    fn n3_quad(&self, triple: Triple) -> N3Quad {
        N3Quad {
            subject: self.n3_term(triple.subject),
            predicate: self.n3_term(triple.predicate),
            object: self.n3_term(triple.object),
            graph_name: match triple.formula {
                Some(Term::Blank(span)) => {
                    GraphName::BlankNode(BlankNode::new_unchecked(self.span(span)))
                }
                _ => GraphName::DefaultGraph,
            },
        }
    }

    fn span(&self, span: super::Span) -> &str {
        &self.arena[span.start as usize..span.end as usize]
    }

    fn n3_term(&self, term: Term) -> N3Term {
        match term {
            Term::Iri(s) => N3Term::NamedNode(NamedNode::new_unchecked(self.span(s))),
            Term::Static(n) => N3Term::NamedNode(n.into_owned()),
            Term::Blank(s) => N3Term::BlankNode(BlankNode::new_unchecked(self.span(s))),
            Term::Variable(s) => N3Term::Variable(Variable::new_unchecked(self.span(s))),
            Term::Literal { value, kind } => {
                let value = self.span(value);
                N3Term::Literal(match kind {
                    Lit::Simple => Literal::new_simple_literal(value),
                    Lit::Language(tag) => {
                        Literal::new_language_tagged_literal_unchecked(value, self.span(tag))
                    }
                    Lit::Typed(datatype) => Literal::new_typed_literal(
                        value,
                        NamedNode::new_unchecked(self.span(datatype)),
                    ),
                    Lit::TypedStatic(datatype) => Literal::new_typed_literal(value, datatype),
                })
            }
        }
    }

    /// `n3Statement '.' | sparqlDirective` at the top level.
    pub(super) fn n3_statement(&mut self) -> Step<()> {
        let token = self.take()?;
        match token.kind {
            Kind::Eof => {
                self.done = true;
                Ok(())
            }
            Kind::AtPrefix => self.prefix(true),
            Kind::Prefix => self.prefix(false),
            Kind::AtBase => self.base(true),
            Kind::Base => self.base(false),
            _ => {
                self.n3_triples(token, 0)?;
                self.expect(Kind::Dot, "expected '.' at the end of the statement")
                    .map(|_| ())
            }
        }
    }

    /// `triples ::= subject predicateObjectList?`
    fn n3_triples(&mut self, first: Token, depth: usize) -> Step<()> {
        let subject = self.n3_path(first, depth)?;
        if self.n3_peek_is_verb()? {
            self.n3_predicate_object_list(subject, depth)?;
        }
        Ok(())
    }

    /// Whether the next token can start a verb.
    fn n3_peek_is_verb(&mut self) -> Step<bool> {
        Ok(!matches!(
            self.peek()?,
            Kind::Dot
                | Kind::Semicolon
                | Kind::Comma
                | Kind::CloseBrace
                | Kind::CloseBracket
                | Kind::CloseParen
                | Kind::Eof
        ))
    }

    /// `verb objectList (';' (verb objectList)?)*`
    fn n3_predicate_object_list(&mut self, subject: Term, depth: usize) -> Step<()> {
        loop {
            let (predicate, inverse) = self.n3_verb(depth)?;
            loop {
                let token = self.take()?;
                let object = self.n3_path(token, depth)?;
                if inverse {
                    self.emit(object, predicate, subject);
                } else {
                    self.emit(subject, predicate, object);
                }
                if self.peek()? != Kind::Comma {
                    break;
                }
                self.take()?;
            }
            if self.peek()? != Kind::Semicolon {
                return Ok(());
            }
            while self.peek()? == Kind::Semicolon {
                self.take()?;
            }
            if !self.n3_peek_is_verb()? {
                return Ok(());
            }
        }
    }

    /// A verb: its predicate, and whether subject and object swap (`is … of`, `<-`).
    fn n3_verb(&mut self, depth: usize) -> Step<(Term, bool)> {
        let token = self.take()?;
        Ok(match token.kind {
            Kind::A => (Term::Static(rdf::TYPE), false),
            Kind::Equals => (Term::Static(OWL_SAME_AS), false),
            Kind::Implies => (Term::Static(LOG_IMPLIES), false),
            Kind::ImpliedBy => (Term::Static(LOG_IS_IMPLIED_BY), false),
            Kind::Has => {
                let next = self.take()?;
                (self.n3_path(next, depth)?, false)
            }
            Kind::Is => {
                let next = self.take()?;
                let predicate = self.n3_path(next, depth)?;
                self.expect(Kind::Of, "expected 'of' after 'is …'")?;
                (predicate, true)
            }
            Kind::Inverse => {
                let next = self.take()?;
                (self.n3_path(next, depth)?, true)
            }
            _ => (self.n3_path(token, depth)?, false),
        })
    }

    /// `path ::= pathItem ('!' path | '^' path)?`, read left to right: `a!b!c` is the `c`
    /// of the `b` of `a`.
    fn n3_path(&mut self, first: Token, depth: usize) -> Step<Term> {
        let mut node = self.n3_item(first, depth)?;
        loop {
            let forward = match self.peek()? {
                Kind::Bang => true,
                Kind::Caret => false,
                _ => return Ok(node),
            };
            self.take()?;
            let next = self.take()?;
            let property = self.n3_item(next, depth)?;
            let step = self.fresh();
            if forward {
                self.emit(node, property, step);
            } else {
                self.emit(step, property, node);
            }
            node = step;
        }
    }

    /// `pathItem`: an IRI, blank node, variable, collection, blank node or IRI property
    /// list, literal or formula.
    fn n3_item(&mut self, token: Token, depth: usize) -> Step<Term> {
        Ok(match token.kind {
            Kind::IriRef { .. } | Kind::PrefixedName { .. } => Term::Iri(self.iri(&token)?),
            Kind::BlankLabel => Term::Blank(self.blank_label(&token)?),
            Kind::Variable => Term::Variable(self.push_text(&token)?),
            Kind::String { .. }
            | Kind::Integer
            | Kind::Decimal
            | Kind::Double
            | Kind::True
            | Kind::False => self.literal(&token)?,
            Kind::OpenBracket => {
                let depth = self.deeper(depth, token.start)?;
                let node = if self.peek()? == Kind::Id {
                    self.take()?;
                    let iri = self.take()?;
                    Term::Iri(self.iri(&iri)?)
                } else {
                    self.fresh()
                };
                if self.n3_peek_is_verb()? {
                    self.n3_predicate_object_list(node, depth)?;
                }
                self.expect(Kind::CloseBracket, "expected ']'")?;
                node
            }
            Kind::OpenParen => {
                let depth = self.deeper(depth, token.start)?;
                self.n3_collection(depth)?
            }
            Kind::OpenBrace => {
                let depth = self.deeper(depth, token.start)?;
                self.n3_formula(depth)?
            }
            _ => return Err(self.error(token.start, "expected a term")),
        })
    }

    /// `'(' object* ')'` after the '('.
    fn n3_collection(&mut self, depth: usize) -> Step<Term> {
        let mut head = None;
        let mut last: Option<Term> = None;
        while self.peek()? != Kind::CloseParen {
            let token = self.take()?;
            let item = self.n3_path(token, depth)?;
            let node = self.fresh();
            match last {
                None => head = Some(node),
                Some(previous) => self.emit(previous, Term::Static(rdf::REST), node),
            }
            self.emit(node, Term::Static(rdf::FIRST), item);
            last = Some(node);
        }
        self.take()?;
        Ok(match (head, last) {
            (Some(head), Some(last)) => {
                self.emit(last, Term::Static(rdf::REST), Term::Static(rdf::NIL));
                head
            }
            _ => Term::Static(rdf::NIL),
        })
    }

    /// `'{' formulaContent? '}'` after the '{': a blank node naming the formula, whose
    /// triples are in its graph; the empty formula is `true`.
    fn n3_formula(&mut self, depth: usize) -> Step<Term> {
        if self.peek()? == Kind::CloseBrace {
            self.take()?;
            let start = self.arena.len();
            self.arena.push_str("true");
            return Ok(Term::Literal {
                value: self.span_from(start),
                kind: Lit::TypedStatic(xsd::BOOLEAN),
            });
        }
        let formula = self.fresh();
        self.formulas.push(formula);
        loop {
            let token = self.take()?;
            match token.kind {
                Kind::CloseBrace => break,
                Kind::Eof => {
                    return Err(self.error(token.start, "a formula without its closing '}'"));
                }
                Kind::AtPrefix => self.prefix(false)?,
                Kind::AtBase => self.base(false)?,
                // SPARQL-style directives need no '.'.
                Kind::Prefix => {
                    self.prefix(false)?;
                    continue;
                }
                Kind::Base => {
                    self.base(false)?;
                    continue;
                }
                _ => self.n3_triples(token, depth)?,
            }
            match self.peek()? {
                Kind::Dot => {
                    self.take()?;
                }
                Kind::CloseBrace => {}
                _ => {
                    let token = self.take()?;
                    return Err(self.error(token.start, "expected '.' or '}' in a formula"));
                }
            }
        }
        self.formulas.pop();
        Ok(formula)
    }
}
