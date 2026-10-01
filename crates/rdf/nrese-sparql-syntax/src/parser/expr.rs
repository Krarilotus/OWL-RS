//! Expressions (SPARQL 1.1 §17, grammar rules 110–129): operators, built-in functions,
//! aggregates and calls of functions named by IRIs.
//!
//! Binary operators of the same precedence associate to the left (`10 - 5 - 2` is 3). A
//! number with a sign directly before its digits is a signed literal (`-1`), as the
//! grammar's tokens make it; directly after an operand (`?x -1`) it is a subtraction.

use nrese_rdf::{NamedNode, Variable};

use super::{ParseResult, Parser};
use crate::algebra::{AggregateExpression, AggregateFunction, Expression, Function};

/// How many arguments a built-in function takes.
#[derive(Clone, Copy)]
enum Arity {
    Exactly(usize),
    Between(usize, usize),
    /// `CONCAT`, `COALESCE`: an expression list, possibly empty.
    List,
}

/// The built-in functions called by a keyword, with the SPARQL version they need.
fn builtin(word: &str) -> Option<(Function, Arity, bool)> {
    use Arity::{Between, Exactly, List};
    let (function, arity, sparql_12) = match word {
        "STR" => (Function::Str, Exactly(1), false),
        "LANG" => (Function::Lang, Exactly(1), false),
        "LANGMATCHES" => (Function::LangMatches, Exactly(2), false),
        "DATATYPE" => (Function::Datatype, Exactly(1), false),
        "IRI" | "URI" => (Function::Iri, Exactly(1), false),
        "BNODE" => (Function::BNode, Between(0, 1), false),
        "RAND" => (Function::Rand, Exactly(0), false),
        "ABS" => (Function::Abs, Exactly(1), false),
        "CEIL" => (Function::Ceil, Exactly(1), false),
        "FLOOR" => (Function::Floor, Exactly(1), false),
        "ROUND" => (Function::Round, Exactly(1), false),
        "CONCAT" => (Function::Concat, List, false),
        "SUBSTR" => (Function::SubStr, Between(2, 3), false),
        "STRLEN" => (Function::StrLen, Exactly(1), false),
        "REPLACE" => (Function::Replace, Between(3, 4), false),
        "UCASE" => (Function::UCase, Exactly(1), false),
        "LCASE" => (Function::LCase, Exactly(1), false),
        "ENCODE_FOR_URI" => (Function::EncodeForUri, Exactly(1), false),
        "CONTAINS" => (Function::Contains, Exactly(2), false),
        "STRSTARTS" => (Function::StrStarts, Exactly(2), false),
        "STRENDS" => (Function::StrEnds, Exactly(2), false),
        "STRBEFORE" => (Function::StrBefore, Exactly(2), false),
        "STRAFTER" => (Function::StrAfter, Exactly(2), false),
        "YEAR" => (Function::Year, Exactly(1), false),
        "MONTH" => (Function::Month, Exactly(1), false),
        "DAY" => (Function::Day, Exactly(1), false),
        "HOURS" => (Function::Hours, Exactly(1), false),
        "MINUTES" => (Function::Minutes, Exactly(1), false),
        "SECONDS" => (Function::Seconds, Exactly(1), false),
        "TIMEZONE" => (Function::Timezone, Exactly(1), false),
        "TZ" => (Function::Tz, Exactly(1), false),
        "NOW" => (Function::Now, Exactly(0), false),
        "UUID" => (Function::Uuid, Exactly(0), false),
        "STRUUID" => (Function::StrUuid, Exactly(0), false),
        "MD5" => (Function::Md5, Exactly(1), false),
        "SHA1" => (Function::Sha1, Exactly(1), false),
        "SHA256" => (Function::Sha256, Exactly(1), false),
        "SHA384" => (Function::Sha384, Exactly(1), false),
        "SHA512" => (Function::Sha512, Exactly(1), false),
        "STRLANG" => (Function::StrLang, Exactly(2), false),
        "STRDT" => (Function::StrDt, Exactly(2), false),
        "ISIRI" | "ISURI" => (Function::IsIri, Exactly(1), false),
        "ISBLANK" => (Function::IsBlank, Exactly(1), false),
        "ISLITERAL" => (Function::IsLiteral, Exactly(1), false),
        "ISNUMERIC" => (Function::IsNumeric, Exactly(1), false),
        "REGEX" => (Function::Regex, Between(2, 3), false),
        "TRIPLE" => (Function::Triple, Exactly(3), true),
        "SUBJECT" => (Function::Subject, Exactly(1), true),
        "PREDICATE" => (Function::Predicate, Exactly(1), true),
        "OBJECT" => (Function::Object, Exactly(1), true),
        "ISTRIPLE" => (Function::IsTriple, Exactly(1), true),
        "LANGDIR" => (Function::LangDir, Exactly(1), true),
        "HASLANG" => (Function::HasLang, Exactly(1), true),
        "HASLANGDIR" => (Function::HasLangDir, Exactly(1), true),
        "STRLANGDIR" => (Function::StrLangDir, Exactly(3), true),
        "ADJUST" => (Function::Adjust, Exactly(2), false),
        _ => return None,
    };
    Some((function, arity, sparql_12))
}

/// The aggregates called by a keyword.
fn aggregate_function(word: &str) -> Option<AggregateFunction> {
    Some(match word {
        "COUNT" => AggregateFunction::Count,
        "SUM" => AggregateFunction::Sum,
        "MIN" => AggregateFunction::Min,
        "MAX" => AggregateFunction::Max,
        "AVG" => AggregateFunction::Avg,
        "SAMPLE" => AggregateFunction::Sample,
        "GROUP_CONCAT" => AggregateFunction::GroupConcat { separator: None },
        _ => return None,
    })
}

/// Keywords that start a built-in call other than the functions above.
fn is_special_builtin(word: &str) -> bool {
    matches!(
        word,
        "BOUND" | "IF" | "COALESCE" | "SAMETERM" | "EXISTS" | "NOT"
    )
}

impl<'a> Parser<'a> {
    pub(super) fn expression(&mut self) -> ParseResult<Expression> {
        self.enter()?;
        let mut e = self.and_expression()?;
        while self.eat("||") {
            e = Expression::Or(Box::new(e), Box::new(self.and_expression()?));
        }
        self.leave();
        Ok(e)
    }

    fn and_expression(&mut self) -> ParseResult<Expression> {
        let mut e = self.relational_expression()?;
        while self.eat("&&") {
            e = Expression::And(Box::new(e), Box::new(self.relational_expression()?));
        }
        Ok(e)
    }

    fn relational_expression(&mut self) -> ParseResult<Expression> {
        let left = self.additive_expression()?;
        type Make = fn(Box<Expression>, Box<Expression>) -> Expression;
        // The longest token at `<` may be an IRI (`?x<?a&&?b>?y`), which is no operator.
        let iri_next = self.peek() == Some(b'<') && self.iriref_token_at(self.pos);
        let operator: Option<(Make, bool)> = if self.eat("=") {
            Some((Expression::Equal, false))
        } else if self.eat("!=") {
            Some((Expression::Equal, true))
        } else if self.eat("<=") {
            Some((Expression::LessOrEqual, false))
        } else if self.eat(">=") {
            Some((Expression::GreaterOrEqual, false))
        } else if !iri_next && self.eat("<") {
            Some((Expression::Less, false))
        } else if self.eat(">") {
            Some((Expression::Greater, false))
        } else {
            None
        };
        if let Some((make, negated)) = operator {
            let e = make(Box::new(left), Box::new(self.additive_expression()?));
            return Ok(if negated {
                Expression::Not(Box::new(e))
            } else {
                e
            });
        }
        if self.keyword("IN") {
            let list = self.expression_list()?;
            return Ok(Expression::In(Box::new(left), list));
        }
        if self.looking_at_keyword("NOT") {
            let save = self.pos;
            self.keyword("NOT");
            if self.keyword("IN") {
                let list = self.expression_list()?;
                return Ok(Expression::Not(Box::new(Expression::In(
                    Box::new(left),
                    list,
                ))));
            }
            self.pos = save;
        }
        Ok(left)
    }

    fn additive_expression(&mut self) -> ParseResult<Expression> {
        let mut e = self.multiplicative_expression()?;
        loop {
            // `?x -1`: the sign belongs to the number token, and the grammar makes it an
            // operator; the literal starts the next operand's products.
            if self.at_number(true) && matches!(self.peek(), Some(b'+' | b'-')) {
                let minus = self.bytes[self.pos] == b'-';
                self.pos += 1;
                let mut right = Expression::Literal(self.numeric_literal(false)?);
                loop {
                    if self.eat("*") {
                        right = Expression::Multiply(
                            Box::new(right),
                            Box::new(self.unary_expression()?),
                        );
                    } else if self.eat("/") {
                        right =
                            Expression::Divide(Box::new(right), Box::new(self.unary_expression()?));
                    } else {
                        break;
                    }
                }
                e = if minus {
                    Expression::Subtract(Box::new(e), Box::new(right))
                } else {
                    Expression::Add(Box::new(e), Box::new(right))
                };
            } else if self.eat("+") {
                e = Expression::Add(Box::new(e), Box::new(self.multiplicative_expression()?));
            } else if self.eat("-") {
                e = Expression::Subtract(Box::new(e), Box::new(self.multiplicative_expression()?));
            } else {
                return Ok(e);
            }
        }
    }

    fn multiplicative_expression(&mut self) -> ParseResult<Expression> {
        let mut e = self.unary_expression()?;
        loop {
            if self.eat("*") {
                e = Expression::Multiply(Box::new(e), Box::new(self.unary_expression()?));
            } else if self.eat("/") {
                e = Expression::Divide(Box::new(e), Box::new(self.unary_expression()?));
            } else {
                return Ok(e);
            }
        }
    }

    fn unary_expression(&mut self) -> ParseResult<Expression> {
        match self.peek() {
            Some(b'!') if self.byte_at(self.pos + 1) != Some(b'=') => {
                self.pos += 1;
                // SPARQL 1.2 allows `!!x`; 1.1 wants a primary expression after `!`.
                let inner = if self.options.sparql_12 {
                    self.enter()?;
                    let inner = self.unary_expression()?;
                    self.leave();
                    inner
                } else {
                    self.primary_expression()?
                };
                Ok(Expression::Not(Box::new(inner)))
            }
            Some(b'+') if !self.at_number(true) => {
                self.pos += 1;
                Ok(Expression::UnaryPlus(Box::new(self.primary_expression()?)))
            }
            Some(b'-') if !self.at_number(true) => {
                self.pos += 1;
                Ok(Expression::UnaryMinus(Box::new(self.primary_expression()?)))
            }
            _ => self.primary_expression(),
        }
    }

    fn primary_expression(&mut self) -> ParseResult<Expression> {
        let Some(b) = self.peek() else {
            return Err(self.expected("an expression"));
        };
        match b {
            b'(' => self.bracketted_expression(),
            b'<' if self.looking_at("<<(") => self.expression_triple_term(),
            b'?' | b'$' => Ok(self.variable()?.into()),
            b'"' | b'\'' => Ok(self
                .try_rdf_literal()?
                .ok_or_else(|| self.expected("a literal"))?
                .into()),
            b'0'..=b'9' | b'.' | b'+' | b'-' => Ok(self.numeric_literal(true)?.into()),
            _ if self.at_iri() => self.iri_or_function(),
            _ => {
                if let Some(literal) = self.try_boolean() {
                    return Ok(literal.into());
                }
                self.builtin_call()
            }
        }
    }

    fn bracketted_expression(&mut self) -> ParseResult<Expression> {
        self.expect("(")?;
        let e = self.expression()?;
        self.expect(")")?;
        Ok(e)
    }

    /// `Constraint`: a bracketed expression, a built-in call or a function call.
    pub(super) fn constraint(&mut self) -> ParseResult<Expression> {
        if self.peek() == Some(b'(') {
            return self.bracketted_expression();
        }
        if self.at_iri() {
            let at = self.peek_offset();
            let e = self.iri_or_function()?;
            if matches!(e, Expression::NamedNode(_)) {
                return Err(self.error_at(at, "expected a function call, not an IRI"));
            }
            return Ok(e);
        }
        self.builtin_call()
    }

    /// Whether a constraint (or a `GROUP BY` condition) can start here.
    pub(super) fn at_constraint(&mut self) -> bool {
        match self.peek() {
            Some(b'(') => true,
            Some(_) if self.at_iri() => true,
            Some(b) if b.is_ascii_alphabetic() => self.at_builtin_call(),
            _ => false,
        }
    }

    fn at_builtin_call(&mut self) -> bool {
        let word = self.peek_word().to_ascii_uppercase();
        builtin(&word).is_some() || aggregate_function(&word).is_some() || is_special_builtin(&word)
    }

    /// An IRI, the call of the function it names, or a custom aggregate.
    fn iri_or_function(&mut self) -> ParseResult<Expression> {
        let iri = self.iri()?;
        if self.peek() != Some(b'(') {
            return Ok(iri.into());
        }
        if self.options.custom_aggregates.contains(&iri) {
            return self.custom_aggregate(iri);
        }
        let args = self.arg_list()?;
        Ok(Expression::FunctionCall(Function::Custom(iri), args))
    }

    /// `ArgList`: `()` or `( e, … )`.
    fn arg_list(&mut self) -> ParseResult<Vec<Expression>> {
        self.expect("(")?;
        if self.eat(")") {
            return Ok(Vec::new());
        }
        if self.looking_at_keyword("DISTINCT") {
            return Err(self.error("DISTINCT is for aggregates, not for function calls"));
        }
        let mut args = vec![self.expression()?];
        while self.eat(",") {
            args.push(self.expression()?);
        }
        self.expect(")")?;
        Ok(args)
    }

    /// `ExpressionList`: `()` or `( e, … )`.
    fn expression_list(&mut self) -> ParseResult<Vec<Expression>> {
        self.expect("(")?;
        let mut list = Vec::new();
        if self.eat(")") {
            return Ok(list);
        }
        loop {
            list.push(self.expression()?);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(list)
    }

    fn builtin_call(&mut self) -> ParseResult<Expression> {
        let at = self.peek_offset();
        let word = self.peek_word().to_ascii_uppercase();
        if word.is_empty() {
            return Err(self.expected("an expression"));
        }
        if let Some(function) = aggregate_function(&word) {
            self.pos += word.len();
            return self.aggregate(function, at);
        }
        match word.as_str() {
            "BOUND" => {
                self.pos += word.len();
                self.expect("(")?;
                let v = self.variable()?;
                self.expect(")")?;
                return Ok(Expression::Bound(v));
            }
            "IF" => {
                self.pos += word.len();
                let [a, b, c] = self.fixed_args::<3>()?;
                return Ok(Expression::If(Box::new(a), Box::new(b), Box::new(c)));
            }
            "COALESCE" => {
                self.pos += word.len();
                return Ok(Expression::Coalesce(self.expression_list()?));
            }
            "SAMETERM" => {
                self.pos += word.len();
                let [a, b] = self.fixed_args::<2>()?;
                return Ok(Expression::SameTerm(Box::new(a), Box::new(b)));
            }
            "EXISTS" => {
                self.pos += word.len();
                return Ok(Expression::Exists(Box::new(self.exists_pattern()?)));
            }
            "NOT" => {
                self.pos += word.len();
                self.expect_keyword("EXISTS")?;
                let pattern = self.exists_pattern()?;
                return Ok(Expression::Not(Box::new(Expression::Exists(Box::new(
                    pattern,
                )))));
            }
            _ => {}
        }
        let Some((function, arity, sparql_12)) = builtin(&word) else {
            return Err(self.expected("an expression"));
        };
        if sparql_12 && !self.options.sparql_12 {
            return Err(self.error_at(at, format!("{word} needs SPARQL 1.2")));
        }
        if function == Function::Adjust && !self.options.adjust {
            return Err(self.error_at(at, "ADJUST (SEP-0002) is not enabled"));
        }
        self.pos += word.len();
        let args = match arity {
            Arity::List => self.expression_list()?,
            Arity::Exactly(0) => {
                self.expect("(")?;
                self.expect(")")?;
                Vec::new()
            }
            Arity::Exactly(n) | Arity::Between(n, _) => {
                let max = match arity {
                    Arity::Between(_, max) => max,
                    _ => n,
                };
                self.expect("(")?;
                let mut args = Vec::with_capacity(max);
                if n > 0 || !self.looking_at(")") {
                    args.push(self.expression()?);
                    while args.len() < max && self.eat(",") {
                        args.push(self.expression()?);
                    }
                }
                self.expect(")")?;
                if args.len() < n {
                    return Err(self.error_at(
                        at,
                        format!("{word} takes at least {n} arguments, not {}", args.len()),
                    ));
                }
                args
            }
        };
        Ok(Expression::FunctionCall(function, args))
    }

    /// `( a, b, … )` with exactly `N` expressions.
    fn fixed_args<const N: usize>(&mut self) -> ParseResult<[Expression; N]> {
        self.expect("(")?;
        let mut args = Vec::with_capacity(N);
        for i in 0..N {
            if i > 0 {
                self.expect(",")?;
            }
            args.push(self.expression()?);
        }
        self.expect(")")?;
        Ok(args
            .try_into()
            .unwrap_or_else(|_| unreachable!("N arguments")))
    }

    /// The pattern of `EXISTS`: no aggregates inside, whatever surrounds it.
    fn exists_pattern(&mut self) -> ParseResult<crate::algebra::GraphPattern> {
        let allowed = std::mem::replace(&mut self.aggregates_allowed, false);
        let pattern = self.group_graph_pattern();
        self.aggregates_allowed = allowed;
        pattern
    }

    /// An aggregate after its keyword; the variable that stands for its value.
    fn aggregate(&mut self, function: AggregateFunction, at: usize) -> ParseResult<Expression> {
        self.expect("(")?;
        let distinct = self.keyword("DISTINCT");
        let aggregate = if matches!(function, AggregateFunction::Count) && self.eat("*") {
            AggregateExpression::CountSolutions { distinct }
        } else {
            let expr = self.aggregate_argument()?;
            let name = match function {
                AggregateFunction::GroupConcat { .. } if self.eat(";") => {
                    self.expect_keyword("SEPARATOR")?;
                    self.expect("=")?;
                    AggregateFunction::GroupConcat {
                        separator: Some(self.string()?),
                    }
                }
                other => other,
            };
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            }
        };
        self.expect(")")?;
        self.register_aggregate(aggregate, at)
    }

    /// A custom aggregate's call after its IRI.
    fn custom_aggregate(&mut self, name: NamedNode) -> ParseResult<Expression> {
        let at = self.peek_offset();
        self.expect("(")?;
        let distinct = self.keyword("DISTINCT");
        let expr = self.aggregate_argument()?;
        self.expect(")")?;
        self.register_aggregate(
            AggregateExpression::FunctionCall {
                name: AggregateFunction::Custom(name),
                expr,
                distinct,
            },
            at,
        )
    }

    /// The argument of an aggregate: no aggregate inside.
    fn aggregate_argument(&mut self) -> ParseResult<Expression> {
        let allowed = std::mem::replace(&mut self.aggregates_allowed, false);
        let e = self.expression();
        self.aggregates_allowed = allowed;
        e
    }

    /// The variable for `aggregate` in the `SELECT` being read: an equal aggregate
    /// shares one.
    fn register_aggregate(
        &mut self,
        aggregate: AggregateExpression,
        at: usize,
    ) -> ParseResult<Expression> {
        if !self.aggregates_allowed || self.aggregates.is_empty() {
            return Err(self.error_at(
                at,
                "an aggregate may only stand in SELECT, HAVING and ORDER BY",
            ));
        }
        if let Some((v, _)) = self
            .aggregates
            .last()
            .and_then(|list| list.iter().find(|(_, a)| *a == aggregate))
        {
            return Ok(v.clone().into());
        }
        let variable: Variable = self.fresh_variable("agg");
        if let Some(list) = self.aggregates.last_mut() {
            list.push((variable.clone(), aggregate));
        }
        Ok(variable.into())
    }

    /// `<<( s p o )>>` in an expression: `TRIPLE(s, p, o)`.
    fn expression_triple_term(&mut self) -> ParseResult<Expression> {
        if !self.options.sparql_12 {
            return Err(self.error("triple terms need SPARQL 1.2"));
        }
        self.enter()?;
        self.expect("<<(")?;
        let at = self.peek_offset();
        let subject = self.expression_triple_term_part()?;
        if !matches!(subject, Expression::NamedNode(_) | Expression::Variable(_)) {
            return Err(self.error_at(
                at,
                "the subject of a triple term must be a variable or an IRI",
            ));
        }
        let predicate: Expression = if let Some(v) = self.try_variable()? {
            v.into()
        } else if self.peek() == Some(b'a')
            && !self.byte_at(self.pos + 1).is_some_and(super::is_word_byte)
        {
            self.pos += 1;
            nrese_rdf::vocab::rdf::TYPE.into_owned().into()
        } else {
            self.iri()?.into()
        };
        let object = self.expression_triple_term_part()?;
        self.expect(")>>")?;
        self.leave();
        Ok(Expression::FunctionCall(
            Function::Triple,
            vec![subject, predicate, object],
        ))
    }

    fn expression_triple_term_part(&mut self) -> ParseResult<Expression> {
        if self.looking_at("<<(") {
            return self.expression_triple_term();
        }
        if let Some(v) = self.try_variable()? {
            return Ok(v.into());
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
        Err(self.expected("a term of a triple term"))
    }
}
