//! Expression evaluation for the native executor.
//!
//! [`supported`] decides whether the native executor can evaluate an expression; if not,
//! the query is refused as unsupported. Evaluation works on decoded terms: `None` means an
//! error or an unbound variable, which SPARQL treats alike in FILTERs. The supported set
//! grows with coverage (execution-core design, XC5).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use md5::{Digest, Md5};
use nrese_rdf::Iri;
use nrese_rdf::vocab::xsd;
use nrese_rdf::{BlankNode, Literal, NamedNode, Term, Variable};
use nrese_sparql_syntax::algebra::{Expression, Function};
use regex::Regex;
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

use nrese_xsd::{
    Date, DateTime, DayTimeDuration, Decimal, Double, Duration, Float, GDay, GMonth, GMonthDay,
    GYear, GYearMonth, Integer, Time, TimezoneOffset, YearMonthDuration,
};

use super::calendar;
use super::value::{
    Value, boolean_term, canonical, compare, effective_boolean, equals, is_lang_string,
};

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
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => supported(a) && supported(b),
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) => supported(a),
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
                        | Function::Abs
                        | Function::Ceil
                        | Function::Floor
                        | Function::Round
                        | Function::Year
                        | Function::Month
                        | Function::Day
                        | Function::Hours
                        | Function::Minutes
                        | Function::Seconds
                        | Function::Concat
                        | Function::StrBefore
                        | Function::StrAfter
                        | Function::StrDt
                        | Function::StrLang
                        | Function::SubStr
                        | Function::Replace
                        | Function::EncodeForUri
                        | Function::Iri
                        | Function::Timezone
                        | Function::Tz
                        | Function::Now
                        | Function::Rand
                        | Function::Uuid
                        | Function::StrUuid
                        | Function::Md5
                        | Function::Sha1
                        | Function::Sha256
                        | Function::Sha384
                        | Function::Sha512
                        | Function::Adjust
                )
                // Casts, GeoSPARQL; any other IRI is an unknown function: an error.
                || matches!(function, Function::Custom(_))
                || (matches!(function, Function::BNode) && args.len() <= 1)
        }
        _ => false,
    }
}

/// Evaluates expressions; caches compiled regular expressions per query. Thread-safe, so
/// parallel filters and aggregates share one.
#[derive(Default)]
pub struct Evaluator {
    regexes: Mutex<HashMap<(String, String), Option<Regex>>>,
    /// The query's `BASE`, against which `IRI()` resolves a relative IRI.
    base: Option<Iri<String>>,
    /// `NOW()`: one instant for the whole query, taken when first asked for.
    now: OnceLock<DateTime>,
    /// Alias IRIs standing for blank nodes put into a pattern (`substitute`).
    aliases: std::sync::RwLock<HashMap<String, Term>>,
    /// This query run, in the blank nodes `BNODE(label)` makes.
    run: u64,
}

thread_local! {
    /// The solution an expression is evaluated for ([`Evaluator::in_solution`]).
    static SOLUTION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl Evaluator {
    /// An evaluator for one query; `base` resolves relative IRIs of `IRI()`.
    pub fn with_base(base: Option<Iri<String>>) -> Self {
        Self {
            base,
            run: rand::random(),
            ..Self::default()
        }
    }

    /// Runs `evaluate` for solution number `solution`: what `BNODE(label)` keeps apart.
    pub fn in_solution<T>(&self, solution: u64, evaluate: impl FnOnce() -> T) -> T {
        let previous = SOLUTION.with(|s| s.replace(solution));
        let result = evaluate();
        SOLUTION.with(|s| s.set(previous));
        result
    }

    /// Makes `alias` evaluate to `term` (a blank node put into a pattern).
    pub(crate) fn register_alias(&self, alias: &str, term: Term) {
        self.aliases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(alias.to_owned(), term);
    }

    /// The compiled `pattern` with `flags`, cached per query; `None` if it doesn't compile.
    /// A clone shares the compiled program, so matching runs outside the lock.
    fn regex(&self, pattern: &str, flags: &str) -> Option<Regex> {
        self.regexes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((pattern.to_owned(), flags.to_owned()))
            .or_insert_with(|| compile_regex(pattern, flags))
            .clone()
    }

    /// Effective boolean value of `expr` for FILTER: errors and unbound values are false.
    pub fn filter(&self, expr: &Expression, binding: &dyn Fn(&Variable) -> Option<Term>) -> bool {
        self.eval(expr, binding)
            .and_then(|term| effective_boolean(&Value::of(&term)))
            .unwrap_or(false)
    }

    /// The value of `expr` with the variables `binding` gives; `None` for an error or an
    /// unbound value.
    pub fn eval(
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
            Expression::NamedNode(node) if node.as_str().starts_with(super::substitute::ALIAS) => {
                self.aliases
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(node.as_str())
                    .cloned()
            }
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
            Expression::Add(a, b) => arithmetic(Operator::Add, values(a, b)?),
            Expression::Subtract(a, b) => arithmetic(Operator::Subtract, values(a, b)?),
            Expression::Multiply(a, b) => arithmetic(Operator::Multiply, values(a, b)?),
            Expression::Divide(a, b) => arithmetic(Operator::Divide, values(a, b)?),
            Expression::UnaryPlus(a) => {
                let term = self.eval(a, binding)?;
                let value = Value::of(&term);
                (Numeric::of(&value).is_some() || value.duration().is_some()).then_some(term)
            }
            Expression::UnaryMinus(a) => {
                let value = Value::of(&self.eval(a, binding)?);
                match Numeric::of(&value) {
                    Some(n) => n.negate(),
                    None => calendar::negate(&value),
                }
            }
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
            Function::SubStr => {
                let (source, language) = string(&arg(0)?)?;
                let position = |i: usize| -> Option<usize> {
                    match Value::of(&arg(i)?) {
                        Value::Integer(v) => usize::try_from(i64::from(v)).ok(),
                        _ => None,
                    }
                };
                let start = position(1)?;
                let length = if args.len() > 2 {
                    Some(position(2)?)
                } else {
                    None
                };
                // Character positions, 1-based (XPath fn:substring).
                let mut chars = source.char_indices().skip(start.checked_sub(1)?).peekable();
                let result = match chars.peek().copied() {
                    Some((from, _)) => match length {
                        Some(length) => match chars.nth(length) {
                            Some((to, _)) => &source[from..to],
                            None => &source[from..],
                        },
                        None => &source[from..],
                    },
                    None => "",
                };
                Some(plain(result.to_owned(), language))
            }
            Function::Replace => {
                let (text, language) = string(&arg(0)?)?;
                let simple = |term: Term| match string(&term)? {
                    (value, None) => Some(value),
                    _ => None,
                };
                let pattern = simple(arg(1)?)?;
                let replacement = simple(arg(2)?)?;
                let flags = if args.len() > 3 {
                    simple(arg(3)?)?
                } else {
                    String::new()
                };
                let regex = self.regex(&pattern, &flags)?;
                let replaced = regex.replace_all(&text, replacement.as_str()).into_owned();
                Some(plain(replaced, language))
            }
            Function::EncodeForUri => {
                let (value, _) = string(&arg(0)?)?;
                let mut encoded = String::with_capacity(value.len());
                for byte in value.bytes() {
                    match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            encoded.push(byte as char)
                        }
                        _ => encoded.push_str(&format!("%{byte:02X}")),
                    }
                }
                Some(Literal::new_simple_literal(encoded).into())
            }
            Function::Iri => match arg(0)? {
                iri @ Term::NamedNode(_) => Some(iri),
                term => match (string(&term)?, &self.base) {
                    ((value, None), Some(base)) => base
                        .resolve(&value)
                        .ok()
                        .map(|iri| NamedNode::new_unchecked(iri.into_inner()).into()),
                    ((value, None), None) => NamedNode::new(value).ok().map(Term::from),
                    _ => None,
                },
            },
            Function::Timezone | Function::Tz => {
                let Term::Literal(literal) = arg(0)? else {
                    return None;
                };
                let (duration, offset) = zone(&literal)?;
                match function {
                    Function::Timezone => duration.map(|duration| {
                        Literal::new_typed_literal(duration.to_string(), xsd::DAY_TIME_DURATION)
                            .into()
                    }),
                    _ => Some(
                        Literal::new_simple_literal(
                            offset.map_or_else(String::new, |o| o.to_string()),
                        )
                        .into(),
                    ),
                }
            }
            Function::Now => Some(
                Literal::new_typed_literal(
                    self.now.get_or_init(DateTime::now).to_string(),
                    xsd::DATE_TIME,
                )
                .into(),
            ),
            Function::Rand => Some(Literal::from(rand::random::<f64>()).into()),
            Function::Uuid => Some(NamedNode::new_unchecked(format!("urn:uuid:{}", uuid())).into()),
            Function::StrUuid => Some(Literal::new_simple_literal(uuid()).into()),
            Function::BNode if args.is_empty() => Some(BlankNode::default().into()),
            // The same blank node for the same label within one solution, another in the
            // next solution and in the next query run (SPARQL 1.1 §17.4.2.9).
            Function::BNode => {
                let label = match arg(0)? {
                    Term::Literal(l)
                        if l.language().is_none()
                            && l.datatype().as_str()
                                == "http://www.w3.org/2001/XMLSchema#string" =>
                    {
                        l.value().to_owned()
                    }
                    _ => return None,
                };
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                label.hash(&mut hasher);
                let solution = SOLUTION.with(std::cell::Cell::get);
                Some(
                    BlankNode::new_unchecked(format!(
                        "b{:x}s{solution:x}l{:x}",
                        self.run,
                        hasher.finish()
                    ))
                    .into(),
                )
            }
            Function::Md5
            | Function::Sha1
            | Function::Sha256
            | Function::Sha384
            | Function::Sha512 => {
                let (value, None) = string(&arg(0)?)? else {
                    return None;
                };
                let bytes = value.as_bytes();
                let hash = match function {
                    Function::Md5 => hex::encode(Md5::digest(bytes)),
                    Function::Sha1 => hex::encode(Sha1::digest(bytes)),
                    Function::Sha256 => hex::encode(Sha256::digest(bytes)),
                    Function::Sha384 => hex::encode(Sha384::digest(bytes)),
                    _ => hex::encode(Sha512::digest(bytes)),
                };
                Some(Literal::new_simple_literal(hash).into())
            }
            Function::Custom(name) if name.as_str().starts_with(super::geo::GEOF) => {
                let values: Option<Vec<Term>> = (0..args.len()).map(arg).collect();
                super::geo::call(name.as_str(), &values?)
            }
            Function::Custom(name) => cast(name.as_str(), arg(0)?),
            Function::Str => match arg(0)? {
                Term::NamedNode(node) => Some(Literal::new_simple_literal(node.as_str()).into()),
                // The lexical form, as written (SPARQL 1.1 §17.4.2.5): STR("03"^^xsd:integer)
                // is "03". (A cast to xsd:string gives the canonical form: XPath §19.)
                Term::Literal(literal) => Some(Literal::new_simple_literal(literal.value()).into()),
                // SPARQL 1.2: STR of a triple term is an error.
                Term::BlankNode(_) | Term::Triple(_) => None,
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
                Some(boolean_term(super::value::lang_matches(&tag, &range)))
            }
            Function::Datatype => match arg(0)? {
                Term::Literal(literal) if is_lang_string(&literal) => Some(
                    NamedNode::new_unchecked(nrese_rdf::vocab::rdf::LANG_STRING.as_str()).into(),
                ),
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
                let regex = self.regex(&pattern, &flags)?;
                Some(boolean_term(regex.is_match(&text)))
            }
            Function::IsIri => arg(0).map(|t| boolean_term(t.is_named_node())),
            Function::IsBlank => arg(0).map(|t| boolean_term(t.is_blank_node())),
            Function::IsLiteral => arg(0).map(|t| boolean_term(t.is_literal())),
            Function::Abs | Function::Ceil | Function::Floor | Function::Round => {
                Numeric::of(&Value::of(&arg(0)?))?.rounding(function)
            }
            // The calendar types that have the part (dates, and the Gregorian g-types).
            Function::Year | Function::Month | Function::Day => {
                let value = Value::of(&arg(0)?);
                let part: i64 = match (function, value) {
                    (Function::Year, Value::Date(d)) => d.year(),
                    (Function::Year, Value::DateTime(d)) => d.year(),
                    (Function::Year, Value::GYear(d)) => d.year(),
                    (Function::Year, Value::GYearMonth(d)) => d.year(),
                    (Function::Month, Value::Date(d)) => d.month().into(),
                    (Function::Month, Value::DateTime(d)) => d.month().into(),
                    (Function::Month, Value::GYearMonth(d)) => d.month().into(),
                    (Function::Month, Value::GMonth(d)) => d.month().into(),
                    (Function::Month, Value::GMonthDay(d)) => d.month().into(),
                    (Function::Day, Value::Date(d)) => d.day().into(),
                    (Function::Day, Value::DateTime(d)) => d.day().into(),
                    (Function::Day, Value::GMonthDay(d)) => d.day().into(),
                    (Function::Day, Value::GDay(d)) => d.day().into(),
                    _ => return None,
                };
                Some(Literal::new_typed_literal(part.to_string(), xsd::INTEGER).into())
            }
            Function::Hours | Function::Minutes | Function::Seconds => {
                let (hour, minute, second) = match Value::of(&arg(0)?) {
                    Value::DateTime(d) => (d.hour(), d.minute(), d.second()),
                    Value::Time(t) => (t.hour(), t.minute(), t.second()),
                    _ => return None,
                };
                Some(match function {
                    Function::Hours => {
                        Literal::new_typed_literal(hour.to_string(), xsd::INTEGER).into()
                    }
                    Function::Minutes => {
                        Literal::new_typed_literal(minute.to_string(), xsd::INTEGER).into()
                    }
                    _ => Literal::new_typed_literal(second.to_string(), xsd::DECIMAL).into(),
                })
            }
            Function::Adjust => calendar::adjust(&Value::of(&arg(0)?), &Value::of(&arg(1)?)),
            Function::Concat => {
                // The common language tag if every argument has it, else a simple literal.
                let mut text = String::new();
                let mut language: Option<Option<String>> = None;
                for i in 0..args.len() {
                    let (value, lang) = string(&arg(i)?)?;
                    text.push_str(&value);
                    language = Some(match language {
                        None => lang,
                        Some(previous) if previous == lang => previous,
                        Some(_) => None,
                    });
                }
                Some(plain(text, language.flatten()))
            }
            Function::StrBefore | Function::StrAfter => {
                let (text, needle, language) = pair()?;
                Some(match text.find(&needle) {
                    // An empty match keeps the argument's language; no match is "".
                    Some(at) => {
                        let part = if matches!(function, Function::StrBefore) {
                            text[..at].to_owned()
                        } else {
                            text[at + needle.len()..].to_owned()
                        };
                        plain(part, language)
                    }
                    None => Literal::new_simple_literal("").into(),
                })
            }
            Function::StrDt => {
                let Term::Literal(value) = arg(0)? else {
                    return None;
                };
                let Term::NamedNode(datatype) = arg(1)? else {
                    return None;
                };
                (value.language().is_none() && value.datatype() == xsd::STRING)
                    .then(|| Literal::new_typed_literal(value.value(), datatype).into())
            }
            Function::StrLang => {
                let Term::Literal(value) = arg(0)? else {
                    return None;
                };
                let (language, _) = string(&arg(1)?)?;
                (value.language().is_none()
                    && value.datatype() == xsd::STRING
                    && !language.is_empty())
                .then(|| {
                    Literal::new_language_tagged_literal(value.value(), language)
                        .ok()
                        .map(Term::from)
                })
                .flatten()
            }
            Function::IsNumeric => arg(0).map(|t| {
                boolean_term(matches!(
                    Value::of(&t),
                    Value::Integer(_) | Value::Decimal(_) | Value::Float(_) | Value::Double(_)
                ))
            }),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

/// The timezone of a date or time literal, as `TIMEZONE` (a duration) and `TZ` (an offset)
/// read it; `None` if the literal is no well-formed date or time.
fn zone(literal: &Literal) -> Option<(Option<DayTimeDuration>, Option<TimezoneOffset>)> {
    let value = literal.value();
    macro_rules! of {
        ($type:ty) => {{
            let parsed: $type = value.parse().ok()?;
            (parsed.timezone(), parsed.timezone_offset())
        }};
    }
    Some(match literal.datatype().as_str() {
        "http://www.w3.org/2001/XMLSchema#dateTime"
        | "http://www.w3.org/2001/XMLSchema#dateTimeStamp" => of!(DateTime),
        "http://www.w3.org/2001/XMLSchema#date" => of!(Date),
        "http://www.w3.org/2001/XMLSchema#time" => of!(Time),
        "http://www.w3.org/2001/XMLSchema#gYearMonth" => of!(GYearMonth),
        "http://www.w3.org/2001/XMLSchema#gYear" => of!(GYear),
        "http://www.w3.org/2001/XMLSchema#gMonthDay" => of!(GMonthDay),
        "http://www.w3.org/2001/XMLSchema#gDay" => of!(GDay),
        "http://www.w3.org/2001/XMLSchema#gMonth" => of!(GMonth),
        _ => return None,
    })
}

/// A random (version 4) UUID, lower-case hex in groups of 8-4-4-4-12.
fn uuid() -> String {
    let mut bytes = rand::random::<u128>().to_le_bytes();
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The regular expression of SPARQL's `REGEX(_, pattern, flags)`; `None` if the pattern
/// or a flag is invalid. Flags (XPath): i, s, m, x, and q (the pattern is a literal string).
pub fn compile_regex(pattern: &str, flags: &str) -> Option<Regex> {
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

#[derive(Clone, Copy)]
enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// A numeric value, for arithmetic with SPARQL's type promotion
/// (integer → decimal → float → double); results print in the XSD value's string form.
#[derive(Clone, Copy)]
enum Numeric {
    Integer(Integer),
    Decimal(Decimal),
    Float(Float),
    Double(Double),
}

impl Numeric {
    fn of(value: &Value) -> Option<Self> {
        Some(match value {
            Value::Integer(i) => Self::Integer(*i),
            Value::Decimal(d) => Self::Decimal(*d),
            Value::Float(f) => Self::Float(*f),
            Value::Double(d) => Self::Double(*d),
            _ => return None,
        })
    }

    fn term(self) -> Term {
        match self {
            Self::Integer(i) => Literal::new_typed_literal(i.to_string(), xsd::INTEGER),
            Self::Decimal(d) => Literal::new_typed_literal(d.to_string(), xsd::DECIMAL),
            Self::Float(f) => Literal::new_typed_literal(f.to_string(), xsd::FLOAT),
            Self::Double(d) => Literal::new_typed_literal(d.to_string(), xsd::DOUBLE),
        }
        .into()
    }

    fn negate(self) -> Option<Term> {
        Some(
            match self {
                Self::Integer(i) => Self::Integer(i.checked_neg()?),
                Self::Decimal(d) => Self::Decimal(d.checked_neg()?),
                Self::Float(f) => Self::Float(-f),
                Self::Double(d) => Self::Double(-d),
            }
            .term(),
        )
    }

    fn rounding(self, function: &Function) -> Option<Term> {
        Some(
            match (self, function) {
                (Self::Integer(i), Function::Abs) => Self::Integer(i.checked_abs()?),
                (Self::Integer(i), _) => Self::Integer(i),
                (Self::Decimal(d), Function::Abs) => Self::Decimal(d.checked_abs()?),
                (Self::Decimal(d), Function::Ceil) => Self::Decimal(d.checked_ceil()?),
                (Self::Decimal(d), Function::Floor) => Self::Decimal(d.checked_floor()?),
                (Self::Decimal(d), _) => Self::Decimal(d.checked_round()?),
                (Self::Float(f), Function::Abs) => Self::Float(f.abs()),
                (Self::Float(f), Function::Ceil) => Self::Float(f.ceil()),
                (Self::Float(f), Function::Floor) => Self::Float(f.floor()),
                (Self::Float(f), _) => Self::Float(f.round()),
                (Self::Double(d), Function::Abs) => Self::Double(d.abs()),
                (Self::Double(d), Function::Ceil) => Self::Double(d.ceil()),
                (Self::Double(d), Function::Floor) => Self::Double(d.floor()),
                (Self::Double(d), _) => Self::Double(d.round()),
            }
            .term(),
        )
    }
}

fn arithmetic(operator: Operator, (x, y): (Value, Value)) -> Option<Term> {
    use Numeric::{Decimal as D, Double as Db, Float as F, Integer as I};
    let (Some(a), Some(b)) = (Numeric::of(&x), Numeric::of(&y)) else {
        // Dates, times and durations (SEP-0002).
        return match operator {
            Operator::Add => calendar::add(&x, &y),
            Operator::Subtract => calendar::subtract(&x, &y),
            Operator::Multiply => calendar::multiply(&x, &y),
            Operator::Divide => calendar::divide(&x, &y),
        };
    };
    let decimal = |n: Numeric| match n {
        I(i) => Some(Decimal::from(i)),
        D(d) => Some(d),
        _ => None,
    };
    let float = |n: Numeric| match n {
        I(i) => Some(Float::from(i)),
        D(d) => Some(Float::from(d)),
        F(f) => Some(f),
        Db(_) => None,
    };
    let double = |n: Numeric| match n {
        I(i) => Double::from(i),
        D(d) => Double::from(d),
        F(f) => Double::from(f),
        Db(d) => d,
    };
    let result = match (a, b) {
        (I(x), I(y)) => match operator {
            Operator::Add => I(x.checked_add(y)?),
            Operator::Subtract => I(x.checked_sub(y)?),
            Operator::Multiply => I(x.checked_mul(y)?),
            // Integer division is decimal division in SPARQL.
            Operator::Divide => D(Decimal::from(x).checked_div(Decimal::from(y))?),
        },
        (I(_) | D(_), I(_) | D(_)) => {
            let (x, y) = (decimal(a)?, decimal(b)?);
            D(match operator {
                Operator::Add => x.checked_add(y)?,
                Operator::Subtract => x.checked_sub(y)?,
                Operator::Multiply => x.checked_mul(y)?,
                Operator::Divide => x.checked_div(y)?,
            })
        }
        (I(_) | D(_) | F(_), I(_) | D(_) | F(_)) => {
            let (x, y) = (float(a)?, float(b)?);
            F(match operator {
                Operator::Add => x + y,
                Operator::Subtract => x - y,
                Operator::Multiply => x * y,
                Operator::Divide => x / y,
            })
        }
        _ => {
            let (x, y) = (double(a), double(b));
            Db(match operator {
                Operator::Add => x + y,
                Operator::Subtract => x - y,
                Operator::Multiply => x * y,
                Operator::Divide => x / y,
            })
        }
    };
    Some(result.term())
}

/// `name(term)` for a cast (XPath §19): the value read from the literal, converted, in
/// canonical form.
fn cast(name: &str, term: Term) -> Option<Term> {
    use nrese_xsd::{Boolean, Date, DateTime};
    let typed = |value: String, datatype: nrese_rdf::NamedNodeRef<'_>| -> Option<Term> {
        Some(Literal::new_typed_literal(value, datatype).into())
    };
    if name == xsd::STRING.as_str() {
        return match term {
            Term::NamedNode(node) => Some(Literal::new_simple_literal(node.into_string()).into()),
            Term::BlankNode(_) | Term::Triple(_) => None,
            literal @ Term::Literal(_) => match canonical(literal) {
                Term::Literal(literal) => Some(Literal::new_simple_literal(literal.value()).into()),
                _ => None,
            },
        };
    }
    let value = Value::of(&term);
    if name == xsd::BOOLEAN.as_str() {
        let b: bool = match value {
            Value::Boolean(b) => b,
            Value::Float(v) => Boolean::from(v).into(),
            Value::Double(v) => Boolean::from(v).into(),
            Value::Integer(v) => Boolean::from(v).into(),
            Value::Decimal(v) => Boolean::from(v).into(),
            Value::String(s) => s.parse::<Boolean>().ok()?.into(),
            _ => return None,
        };
        return Some(boolean_term(b));
    }
    if name == xsd::DOUBLE.as_str() {
        let v: Double = match value {
            Value::Float(v) => v.into(),
            Value::Double(v) => v,
            Value::Integer(v) => v.into(),
            Value::Decimal(v) => v.into(),
            Value::Boolean(b) => Boolean::from(b).into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::DOUBLE);
    }
    if name == xsd::FLOAT.as_str() {
        let v: Float = match value {
            Value::Float(v) => v,
            Value::Double(v) => v.into(),
            Value::Integer(v) => v.into(),
            Value::Decimal(v) => v.into(),
            Value::Boolean(b) => Boolean::from(b).into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::FLOAT);
    }
    if name == xsd::INTEGER.as_str() {
        let v: Integer = match value {
            Value::Float(v) => v.try_into().ok()?,
            Value::Double(v) => v.try_into().ok()?,
            Value::Integer(v) => v,
            Value::Decimal(v) => v.try_into().ok()?,
            Value::Boolean(b) => Boolean::from(b).into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::INTEGER);
    }
    if name == xsd::DECIMAL.as_str() {
        let v: Decimal = match value {
            Value::Float(v) => v.try_into().ok()?,
            Value::Double(v) => v.try_into().ok()?,
            Value::Integer(v) => v.into(),
            Value::Decimal(v) => v,
            Value::Boolean(b) => Boolean::from(b).into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::DECIMAL);
    }
    if name == xsd::DATE.as_str() {
        let v: Date = match value {
            Value::Date(v) => v,
            Value::DateTime(v) => v.into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::DATE);
    }
    if name == xsd::DATE_TIME.as_str() {
        let v: DateTime = match value {
            Value::DateTime(v) => v,
            Value::Date(v) => v.into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::DATE_TIME);
    }
    if name == xsd::TIME.as_str() {
        let v: Time = match value {
            Value::Time(v) => v,
            Value::DateTime(v) => v.into(),
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        return typed(v.to_string(), xsd::TIME);
    }
    // Durations: from a string, or from another duration type (XPath §19.1.5: the part the
    // target lacks is dropped).
    if name == xsd::DURATION.as_str() {
        let v: Duration = match value {
            Value::String(s) => s.parse().ok()?,
            other => other.duration()?,
        };
        return typed(v.to_string(), xsd::DURATION);
    }
    if name == xsd::YEAR_MONTH_DURATION.as_str() {
        let v: YearMonthDuration = match value {
            Value::String(s) => s.parse().ok()?,
            other => other.duration()?.into(),
        };
        return typed(v.to_string(), xsd::YEAR_MONTH_DURATION);
    }
    if name == xsd::DAY_TIME_DURATION.as_str() {
        let v: DayTimeDuration = match value {
            Value::String(s) => s.parse().ok()?,
            other => other.duration()?.into(),
        };
        return typed(v.to_string(), xsd::DAY_TIME_DURATION);
    }
    g_cast(name, value)
}

/// Casts to the Gregorian g-types: from a string, the same type, or a date or dateTime,
/// whose parts and timezone they keep (XPath §19.1.4).
fn g_cast(name: &str, value: Value) -> Option<Term> {
    let parts = |date: Date| {
        (
            date.year(),
            date.month(),
            date.day(),
            date.timezone_offset(),
        )
    };
    let (year, month, day, zone) = match &value {
        Value::Date(d) => parts(*d),
        Value::DateTime(d) => parts(Date::from(*d)),
        Value::String(_) => (0, 0, 0, None),
        _ => (i64::MIN, 0, 0, None),
    };
    let from_date = year != i64::MIN && !matches!(value, Value::String(_));
    let zone = zone.map(|z| z.to_string()).unwrap_or_default();
    let year_text = if year < 0 {
        format!("-{:04}", year.unsigned_abs())
    } else {
        format!("{year:04}")
    };
    macro_rules! g {
        ($type:ty, $variant:ident, $datatype:expr, $lexical:expr) => {{
            let v: $type = match value {
                Value::$variant(v) => v,
                Value::String(s) => s.parse().ok()?,
                _ if from_date => $lexical.parse().ok()?,
                _ => return None,
            };
            return Some(Literal::new_typed_literal(v.to_string(), $datatype).into());
        }};
    }
    match name {
        n if n == xsd::G_YEAR.as_str() => {
            g!(GYear, GYear, xsd::G_YEAR, format!("{year_text}{zone}"))
        }
        n if n == xsd::G_YEAR_MONTH.as_str() => {
            g!(
                GYearMonth,
                GYearMonth,
                xsd::G_YEAR_MONTH,
                format!("{year_text}-{month:02}{zone}")
            )
        }
        n if n == xsd::G_MONTH.as_str() => {
            g!(GMonth, GMonth, xsd::G_MONTH, format!("--{month:02}{zone}"))
        }
        n if n == xsd::G_MONTH_DAY.as_str() => {
            g!(
                GMonthDay,
                GMonthDay,
                xsd::G_MONTH_DAY,
                format!("--{month:02}-{day:02}{zone}")
            )
        }
        n if n == xsd::G_DAY.as_str() => g!(GDay, GDay, xsd::G_DAY, format!("---{day:02}{zone}")),
        _ => None,
    }
}
