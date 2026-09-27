//! Expression evaluation for the native executor.
//!
//! [`supported`] decides whether the native executor can evaluate an expression; if not,
//! the whole query runs on spareval. Evaluation works on decoded terms: `None` means an
//! error or an unbound variable, which SPARQL treats alike in FILTERs. The supported set
//! grows with coverage (execution-core design, XC5).

use std::cell::RefCell;
use std::collections::HashMap;

use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, Term, Variable};
use regex::Regex;
use spargebra::algebra::{Expression, Function};

use super::value::{Value, boolean_term, compare, effective_boolean, equals, is_lang_string};

/// True if the native evaluator implements every node of `expr`.
pub(crate) fn supported(expr: &Expression) -> bool {
    match expr {
        Expression::NamedNode(_) | Expression::Literal(_) | Expression::Variable(_) => true,
        Expression::Bound(_) => true,
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b) => supported(a) && supported(b),
        Expression::In(a, list) => supported(a) && list.iter().all(supported),
        Expression::Not(a) => supported(a),
        Expression::If(a, b, c) => supported(a) && supported(b) && supported(c),
        Expression::Coalesce(list) => list.iter().all(supported),
        Expression::FunctionCall(function, args) => {
            args.iter().all(supported)
                && matches!(
                    function,
                    Function::Str
                        | Function::Lang
                        | Function::LangMatches
                        | Function::Datatype
                        | Function::Contains
                        | Function::StrStarts
                        | Function::StrEnds
                        | Function::StrLen
                        | Function::UCase
                        | Function::LCase
                        | Function::Regex
                        | Function::IsIri
                        | Function::IsBlank
                        | Function::IsLiteral
                        | Function::IsNumeric
                )
        }
        _ => false,
    }
}

/// Evaluates expressions; caches compiled regular expressions per query.
#[derive(Default)]
pub(crate) struct Evaluator {
    regexes: RefCell<HashMap<(String, String), Option<Regex>>>,
}

impl Evaluator {
    /// Effective boolean value of `expr` for FILTER: errors and unbound values are false.
    pub(crate) fn filter(
        &self,
        expr: &Expression,
        binding: &dyn Fn(&Variable) -> Option<Term>,
    ) -> bool {
        self.eval(expr, binding)
            .and_then(|term| effective_boolean(&Value::of(&term)))
            .unwrap_or(false)
    }

    pub(crate) fn eval(
        &self,
        expr: &Expression,
        binding: &dyn Fn(&Variable) -> Option<Term>,
    ) -> Option<Term> {
        let boolean = |b: bool| Some(boolean_term(b));
        let ebv = |e: &Expression| {
            self.eval(e, binding)
                .and_then(|t| effective_boolean(&Value::of(&t)))
        };
        let values = |a: &Expression, b: &Expression| {
            Some((
                Value::of(&self.eval(a, binding)?),
                Value::of(&self.eval(b, binding)?),
            ))
        };
        match expr {
            Expression::NamedNode(node) => Some(node.clone().into()),
            Expression::Literal(literal) => Some(literal.clone().into()),
            Expression::Variable(variable) => binding(variable),
            Expression::Bound(variable) => boolean(binding(variable).is_some()),
            Expression::Or(a, b) => match (ebv(a), ebv(b)) {
                (Some(true), _) | (_, Some(true)) => boolean(true),
                (Some(false), Some(false)) => boolean(false),
                _ => None,
            },
            Expression::And(a, b) => match (ebv(a), ebv(b)) {
                (Some(false), _) | (_, Some(false)) => boolean(false),
                (Some(true), Some(true)) => boolean(true),
                _ => None,
            },
            Expression::Not(a) => ebv(a).map(|b| boolean_term(!b)),
            Expression::Equal(a, b) => {
                let (x, y) = values(a, b)?;
                equals(&x, &y).map(boolean_term)
            }
            Expression::SameTerm(a, b) => boolean(self.eval(a, binding)? == self.eval(b, binding)?),
            Expression::Greater(a, b) => {
                let (x, y) = values(a, b)?;
                compare(&x, &y).map(|o| boolean_term(o.is_gt()))
            }
            Expression::GreaterOrEqual(a, b) => {
                let (x, y) = values(a, b)?;
                compare(&x, &y).map(|o| boolean_term(o.is_ge()))
            }
            Expression::Less(a, b) => {
                let (x, y) = values(a, b)?;
                compare(&x, &y).map(|o| boolean_term(o.is_lt()))
            }
            Expression::LessOrEqual(a, b) => {
                let (x, y) = values(a, b)?;
                compare(&x, &y).map(|o| boolean_term(o.is_le()))
            }
            Expression::In(a, list) => {
                let needle = Value::of(&self.eval(a, binding)?);
                let mut error = false;
                for item in list {
                    match self
                        .eval(item, binding)
                        .and_then(|t| equals(&needle, &Value::of(&t)))
                    {
                        Some(true) => return boolean(true),
                        Some(false) => {}
                        None => error = true,
                    }
                }
                if error { None } else { boolean(false) }
            }
            Expression::If(c, a, b) => {
                if ebv(c)? {
                    self.eval(a, binding)
                } else {
                    self.eval(b, binding)
                }
            }
            Expression::Coalesce(list) => list.iter().find_map(|e| self.eval(e, binding)),
            Expression::FunctionCall(function, args) => self.call(function, args, binding),
            _ => None,
        }
    }

    fn call(
        &self,
        function: &Function,
        args: &[Expression],
        binding: &dyn Fn(&Variable) -> Option<Term>,
    ) -> Option<Term> {
        let arg = |i: usize| self.eval(args.get(i)?, binding);
        let string = |term: &Term| -> Option<(String, Option<String>)> {
            match term {
                Term::Literal(l) if l.language().is_some() => {
                    Some((l.value().to_owned(), l.language().map(str::to_owned)))
                }
                Term::Literal(l) if l.datatype() == xsd::STRING => {
                    Some((l.value().to_owned(), None))
                }
                _ => None,
            }
        };
        // Two string arguments whose language tags are compatible (SPARQL §17.4.3.1.2).
        let pair = || -> Option<(String, String, Option<String>)> {
            let (a, la) = string(&arg(0)?)?;
            let (b, lb) = string(&arg(1)?)?;
            match (&la, &lb) {
                (_, None) => Some((a, b, la)),
                (Some(x), Some(y)) if x == y => Some((a, b, la)),
                _ => None,
            }
        };
        let plain = |value: String, language: Option<String>| -> Term {
            match language {
                Some(language) => {
                    Literal::new_language_tagged_literal_unchecked(value, language).into()
                }
                None => Literal::new_simple_literal(value).into(),
            }
        };
        match function {
            Function::Str => match arg(0)? {
                Term::NamedNode(node) => Some(Literal::new_simple_literal(node.as_str()).into()),
                Term::Literal(literal) => Some(Literal::new_simple_literal(literal.value()).into()),
                Term::BlankNode(_) => None,
            },
            Function::Lang => match arg(0)? {
                Term::Literal(literal) => {
                    Some(Literal::new_simple_literal(literal.language().unwrap_or_default()).into())
                }
                _ => None,
            },
            Function::LangMatches => {
                let (tag, _) = string(&arg(0)?)?;
                let (range, _) = string(&arg(1)?)?;
                let matches = if range == "*" {
                    !tag.is_empty()
                } else {
                    tag.len() >= range.len()
                        && tag[..range.len()].eq_ignore_ascii_case(&range)
                        && (tag.len() == range.len() || tag.as_bytes()[range.len()] == b'-')
                };
                Some(boolean_term(matches))
            }
            Function::Datatype => match arg(0)? {
                Term::Literal(literal) if is_lang_string(&literal) => {
                    Some(NamedNode::new_unchecked(oxrdf::vocab::rdf::LANG_STRING.as_str()).into())
                }
                Term::Literal(literal) => Some(literal.datatype().into_owned().into()),
                _ => None,
            },
            Function::Contains => pair().map(|(a, b, _)| boolean_term(a.contains(&b))),
            Function::StrStarts => pair().map(|(a, b, _)| boolean_term(a.starts_with(&b))),
            Function::StrEnds => pair().map(|(a, b, _)| boolean_term(a.ends_with(&b))),
            Function::StrLen => {
                let (value, _) = string(&arg(0)?)?;
                Some(
                    Literal::new_typed_literal(value.chars().count().to_string(), xsd::INTEGER)
                        .into(),
                )
            }
            Function::UCase => string(&arg(0)?).map(|(v, l)| plain(v.to_uppercase(), l)),
            Function::LCase => string(&arg(0)?).map(|(v, l)| plain(v.to_lowercase(), l)),
            Function::Regex => {
                let (text, _) = string(&arg(0)?)?;
                let (pattern, _) = string(&arg(1)?)?;
                let flags = match args.get(2) {
                    Some(_) => string(&arg(2)?)?.0,
                    None => String::new(),
                };
                let mut cache = self.regexes.borrow_mut();
                let regex = cache
                    .entry((pattern.clone(), flags.clone()))
                    .or_insert_with(|| compile_regex(&pattern, &flags));
                regex.as_ref().map(|r| boolean_term(r.is_match(&text)))
            }
            Function::IsIri => arg(0).map(|t| boolean_term(t.is_named_node())),
            Function::IsBlank => arg(0).map(|t| boolean_term(t.is_blank_node())),
            Function::IsLiteral => arg(0).map(|t| boolean_term(t.is_literal())),
            Function::IsNumeric => arg(0).map(|t| {
                boolean_term(matches!(
                    Value::of(&t),
                    Value::Integer(_) | Value::Decimal(_) | Value::Float(_) | Value::Double(_)
                ))
            }),
            _ => None,
        }
    }
}

/// SPARQL `REGEX` flags (XPath): i, s, m, x, and q (the pattern is a literal string).
pub(crate) fn compile_regex(pattern: &str, flags: &str) -> Option<Regex> {
    let mut builder_pattern = String::new();
    let mut literal = false;
    for flag in flags.chars() {
        match flag {
            'i' | 's' | 'm' | 'x' => {
                builder_pattern.push_str("(?");
                builder_pattern.push(flag);
                builder_pattern.push(')');
            }
            'q' => literal = true,
            _ => return None,
        }
    }
    if literal {
        builder_pattern.push_str(&regex::escape(pattern));
    } else {
        builder_pattern.push_str(pattern);
    }
    regex::RegexBuilder::new(&builder_pattern)
        .size_limit(1 << 20)
        .build()
        .ok()
}
