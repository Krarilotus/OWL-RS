//! Term values for the native executor: SPARQL comparison, effective boolean value, and the
//! `ORDER BY` order, following SPARQL 1.1 §17; where the specification leaves room, the
//! reference evaluator shares these functions, and the choices are named where made.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use nrese_engine::TermId;
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{Literal, Term};
use nrese_xsd::owl::Datatype;
use nrese_xsd::{
    Boolean, Date, DateTime, DayTimeDuration, Decimal, Double, Duration, Float, GDay, GMonth,
    GMonthDay, GYear, GYearMonth, Integer, Time, TimezoneOffset, YearMonthDuration,
};

/// A valid XSD integer strictly outside signed 64-bit range (`< i64::MIN` or `> i64::MAX`),
/// stored canonically as a sign and non-empty ASCII decimal digits with no leading zeros.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInteger {
    negative: bool,
    digits: String,
}

impl BigInteger {
    /// Converts to `xsd:decimal` if the value fits in `Decimal`'s 128-bit scaled range.
    pub fn to_decimal(&self) -> Option<Decimal> {
        let value = parse_signed_i128(self.negative, self.digits.as_bytes())?;
        Decimal::try_from(value).ok()
    }

    /// Rounds to `xsd:float` using IEEE 754 round-to-nearest-even (`±INF` past `f32::MAX`).
    pub fn to_float(&self) -> Float {
        // `f32::MAX` is ~3.40e38 (39 digits), so 40+ canonical digits always overflow.
        let magnitude = if self.digits.len() > 39 {
            f32::INFINITY
        } else {
            self.digits.parse::<f32>().unwrap_or(f32::INFINITY)
        };
        Float::from(if self.negative { -magnitude } else { magnitude })
    }

    /// Rounds to `xsd:double` using IEEE 754 round-to-nearest-even (`±INF` past `f64::MAX`).
    pub fn to_double(&self) -> Double {
        // `f64::MAX` is ~1.80e308 (309 digits), so 310+ canonical digits always overflow.
        let magnitude = if self.digits.len() > 309 {
            f64::INFINITY
        } else {
            self.digits.parse::<f64>().unwrap_or(f64::INFINITY)
        };
        Double::from(if self.negative { -magnitude } else { magnitude })
    }
}

impl Ord for BigInteger {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self
                .digits
                .len()
                .cmp(&other.digits.len())
                .then_with(|| self.digits.cmp(&other.digits)),
            (true, true) => other
                .digits
                .len()
                .cmp(&self.digits.len())
                .then_with(|| other.digits.cmp(&self.digits)),
        }
    }
}

impl PartialOrd for BigInteger {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for BigInteger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        f.write_str(&self.digits)
    }
}

/// A term's value, as far as operators distinguish them.
#[derive(Debug, Clone)]
pub enum Value {
    Integer(Integer),
    /// An integer outside signed 64-bit range (any unbounded XSD integer subtype, or
    /// `xsd:unsignedLong` in `i64::MAX + 1..=u64::MAX`).
    BigInteger(BigInteger),
    Decimal(Decimal),
    Float(Float),
    Double(Double),
    Boolean(bool),
    /// A simple literal or `xsd:string`.
    String(String),
    LangString(String, String),
    Date(Date),
    DateTime(DateTime),
    Time(Time),
    GYear(GYear),
    GYearMonth(GYearMonth),
    GMonth(GMonth),
    GMonthDay(GMonthDay),
    GDay(GDay),
    Duration(Duration),
    YearMonthDuration(YearMonthDuration),
    DayTimeDuration(DayTimeDuration),
    Iri(String),
    Blank(String),
    /// A literal of another datatype, or an ill-formed typed literal: compared by identity.
    Other(Literal),
    /// A triple term (SPARQL 1.2): compared by identity.
    Triple(Box<nrese_rdf::Triple>),
}

/// Parses canonical ASCII decimal digits with a sign into `i128`, returning `None` on
/// overflow. Accumulates negatively so `i128::MIN` parses without special casing.
fn parse_signed_i128(negative: bool, digits: &[u8]) -> Option<i128> {
    let mut value: i128 = 0;
    for &b in digits {
        value = value.checked_mul(10)?.checked_sub(i128::from(b - b'0'))?;
    }
    if negative {
        Some(value)
    } else {
        value.checked_neg()
    }
}

/// Parses an XSD integer lexical form (`[+-]?[0-9]+`) subject to inclusive `(min, max)`
/// bounds (`None` = unbounded on that side). Values within `i64` stay on the zero-alloc
/// `Value::Integer` fast path; valid values outside `i64` become `Value::BigInteger`.
pub(crate) fn parse_integer(lexical: &str, min: Option<i128>, max: Option<i128>) -> Option<Value> {
    if let Ok(integer) = Integer::from_str(lexical) {
        let value = i128::from(i64::from(integer));
        if min.is_none_or(|lo| value >= lo) && max.is_none_or(|hi| value <= hi) {
            return Some(Value::Integer(integer));
        }
        return None;
    }
    let bytes = lexical.as_bytes();
    let (negative, rest) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if rest.is_empty() || !rest.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let stripped = rest
        .iter()
        .position(|&b| b != b'0')
        .map_or(&b"0"[..], |i| &rest[i..]);
    debug_assert!(stripped != b"0" && stripped.len() >= 19);
    let in_bounds = match parse_signed_i128(negative, stripped) {
        Some(value) => min.is_none_or(|lo| value >= lo) && max.is_none_or(|hi| value <= hi),
        None => {
            if negative {
                min.is_none()
            } else {
                max.is_none()
            }
        }
    };
    if !in_bounds {
        return None;
    }
    let digits = std::str::from_utf8(stripped)
        .expect("validated ASCII digits")
        .to_owned();
    Some(Value::BigInteger(BigInteger { negative, digits }))
}

impl Value {
    pub fn of(term: &Term) -> Self {
        match term {
            Term::NamedNode(node) => Self::Iri(node.as_str().to_owned()),
            Term::BlankNode(node) => Self::Blank(node.as_str().to_owned()),
            Term::Literal(literal) => Self::of_literal(literal),
            Term::Triple(triple) => Self::Triple(triple.clone()),
        }
    }

    fn of_literal(literal: &Literal) -> Self {
        if let Some(language) = literal.language() {
            // A base direction (RDF 1.2): equal only to the same term.
            if literal.direction().is_some() {
                return Self::Other(literal.clone());
            }
            return Self::LangString(literal.value().to_owned(), language.to_owned());
        }
        let datatype = literal.datatype();
        let value = literal.value();
        let parsed = if datatype == xsd::STRING {
            Some(Self::String(value.to_owned()))
        } else if datatype == xsd::BOOLEAN {
            Boolean::from_str(value)
                .ok()
                .map(|b| Self::Boolean(b.into()))
        } else if datatype == xsd::DECIMAL {
            Decimal::from_str(value).ok().map(Self::Decimal)
        } else if datatype == xsd::DOUBLE {
            Double::from_str(value).ok().map(Self::Double)
        } else if datatype == xsd::FLOAT {
            Float::from_str(value).ok().map(Self::Float)
        } else if datatype == xsd::DATE {
            Date::from_str(value).ok().map(Self::Date)
        } else if datatype == xsd::DATE_TIME {
            DateTime::from_str(value).ok().map(Self::DateTime)
        } else if datatype == xsd::TIME {
            Time::from_str(value).ok().map(Self::Time)
        } else if datatype == xsd::DAY_TIME_DURATION {
            DayTimeDuration::from_str(value)
                .ok()
                .map(Self::DayTimeDuration)
        } else if datatype == xsd::YEAR_MONTH_DURATION {
            YearMonthDuration::from_str(value)
                .ok()
                .map(Self::YearMonthDuration)
        } else if datatype == xsd::DURATION {
            Duration::from_str(value).ok().map(Self::Duration)
        } else if datatype == xsd::G_YEAR {
            GYear::from_str(value).ok().map(Self::GYear)
        } else if datatype == xsd::G_YEAR_MONTH {
            GYearMonth::from_str(value).ok().map(Self::GYearMonth)
        } else if datatype == xsd::G_MONTH {
            GMonth::from_str(value).ok().map(Self::GMonth)
        } else if datatype == xsd::G_MONTH_DAY {
            GMonthDay::from_str(value).ok().map(Self::GMonthDay)
        } else if datatype == xsd::G_DAY {
            GDay::from_str(value).ok().map(Self::GDay)
        } else if datatype == xsd::INTEGER {
            parse_integer(value, None, None)
        } else if let Some((min, max)) =
            Datatype::from_iri(datatype.as_str()).and_then(Datatype::integer_bounds)
        {
            parse_integer(value, min, max)
        } else {
            None
        };
        parsed.unwrap_or_else(|| Self::Other(literal.clone()))
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::Integer(_)
                | Self::BigInteger(_)
                | Self::Decimal(_)
                | Self::Float(_)
                | Self::Double(_)
        )
    }

    /// A duration of any of the three types, as an `xsd:duration`.
    pub(crate) fn duration(&self) -> Option<Duration> {
        match self {
            Self::Duration(d) => Some(*d),
            Self::YearMonthDuration(d) => Some((*d).into()),
            Self::DayTimeDuration(d) => Some((*d).into()),
            _ => None,
        }
    }
}

/// An exact numeric value (`xsd:integer` / `xsd:decimal`), ordered by its exact
/// mathematical value on the real line.
#[derive(Debug, Clone, Copy)]
enum Exact<'a> {
    Decimal(Decimal),
    Big(&'a BigInteger),
}

impl<'a> Exact<'a> {
    fn of(value: &'a Value) -> Option<Self> {
        match value {
            Value::Integer(i) => Some(Self::Decimal(Decimal::from(*i))),
            Value::BigInteger(b) => Some(Self::Big(b)),
            Value::Decimal(d) => Some(Self::Decimal(*d)),
            _ => None,
        }
    }
}

impl PartialEq for Exact<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Exact<'_> {}

impl PartialOrd for Exact<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Exact<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::Decimal(x), Self::Decimal(y)) => x.cmp(&y),
            (Self::Big(x), Self::Big(y)) => x.cmp(y),
            (Self::Decimal(d), Self::Big(b)) => match b.to_decimal() {
                Some(bd) => d.cmp(&bd),
                // `b` lies strictly outside Decimal's range: negative < every Decimal < positive.
                None => {
                    if b.negative {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
            },
            (Self::Big(b), Self::Decimal(d)) => match b.to_decimal() {
                Some(bd) => bd.cmp(&d),
                None => {
                    if b.negative {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
            },
        }
    }
}

/// Numeric comparison with SPARQL type promotion (integer → decimal → float → double).
fn compare_numeric(a: &Value, b: &Value) -> Option<Ordering> {
    use Value::{BigInteger as B, Decimal as D, Float as F, Integer as I};
    match (a, b) {
        (I(x), I(y)) => x.partial_cmp(y),
        (I(_) | B(_) | D(_), I(_) | B(_) | D(_)) => Some(Exact::of(a)?.cmp(&Exact::of(b)?)),
        (I(_) | B(_) | D(_) | F(_), I(_) | B(_) | D(_) | F(_)) => {
            to_float(a)?.partial_cmp(&to_float(b)?)
        }
        _ => to_double(a)?.partial_cmp(&to_double(b)?),
    }
}

fn to_float(value: &Value) -> Option<Float> {
    match value {
        Value::Integer(i) => Some(Float::from(*i)),
        Value::BigInteger(b) => Some(b.to_float()),
        Value::Decimal(d) => Some(Float::from(*d)),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn to_double(value: &Value) -> Option<Double> {
    match value {
        Value::Integer(i) => Some(Double::from(*i)),
        Value::BigInteger(b) => Some(b.to_double()),
        Value::Decimal(d) => Some(Double::from(*d)),
        Value::Float(f) => Some(Double::from(*f)),
        Value::Double(d) => Some(*d),
        _ => None,
    }
}

/// `<`, `>`, `<=`, `>=` as operators: `None` is a type error, `Some(None)` a number
/// unordered with NaN, for which XPath's op:numeric-less-than and op:numeric-greater-than
/// give false.
pub(crate) fn compare_rel(a: &Value, b: &Value) -> Option<Option<Ordering>> {
    if a.is_numeric() && b.is_numeric() {
        return Some(compare_numeric(a, b));
    }
    compare(a, b).map(Some)
}

/// `<`, `>`, `<=`, `>=`: `None` is a type error (or a number unordered with NaN).
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    if a.is_numeric() && b.is_numeric() {
        return compare_numeric(a, b);
    }
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::LangString(x, lx), Value::LangString(y, ly)) if lx == ly => Some(x.cmp(y)),
        (Value::Boolean(x), Value::Boolean(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Date(y)) => x.partial_cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.partial_cmp(y),
        (Value::Time(x), Value::Time(y)) => x.partial_cmp(y),
        (Value::GYear(x), Value::GYear(y)) => x.partial_cmp(y),
        (Value::GYearMonth(x), Value::GYearMonth(y)) => x.partial_cmp(y),
        (Value::GMonth(x), Value::GMonth(y)) => x.partial_cmp(y),
        (Value::GMonthDay(x), Value::GMonthDay(y)) => x.partial_cmp(y),
        (Value::GDay(x), Value::GDay(y)) => x.partial_cmp(y),
        _ => a.duration()?.partial_cmp(&b.duration()?),
    }
}

/// `=` (RDFterm-equal extended by value equality): `None` is a type error.
pub fn equals(a: &Value, b: &Value) -> Option<bool> {
    if a.is_numeric() && b.is_numeric() {
        // Per XPath op:numeric-equal, NaN compares unequal to every numeric value (false,
        // never a type error), so `!=` evaluates to true.
        return Some(compare_numeric(a, b) == Some(Ordering::Equal));
    }
    match (a, b) {
        (Value::Iri(x), Value::Iri(y)) | (Value::Blank(x), Value::Blank(y)) => Some(x == y),
        (Value::String(x), Value::String(y)) => Some(x == y),
        (Value::LangString(x, lx), Value::LangString(y, ly)) => {
            Some(x == y && lx.eq_ignore_ascii_case(ly))
        }
        (Value::Boolean(x), Value::Boolean(y)) => Some(x == y),
        // With a timezone on one side only the order is undetermined (`<` is an error),
        // but the two are not equal: `=` is false and `!=` true.
        (Value::Date(x), Value::Date(y)) => Some(x == y),
        (Value::DateTime(x), Value::DateTime(y)) => Some(x == y),
        (Value::Time(x), Value::Time(y)) => Some(x == y),
        (Value::GYear(x), Value::GYear(y)) => Some(x == y),
        (Value::GYearMonth(x), Value::GYearMonth(y)) => Some(x == y),
        (Value::GMonth(x), Value::GMonth(y)) => Some(x == y),
        (Value::GMonthDay(x), Value::GMonthDay(y)) => Some(x == y),
        (Value::GDay(x), Value::GDay(y)) => Some(x == y),
        // Durations are equal across their types (`"P1Y"^^xsd:duration` = `"P12M"^^xsd:yearMonthDuration`).
        (x, y) if x.duration().is_some() && y.duration().is_some() => {
            Some(x.duration() == y.duration())
        }
        (Value::Other(x), Value::Other(y)) if x == y => Some(true),
        // Triple terms (SPARQL 1.2): the same subject and predicate, and objects equal as
        // values (`<<( :a :b 123 )>> = <<( :a :b 123.0 )>>`).
        (Value::Triple(x), Value::Triple(y)) => {
            if x.subject != y.subject || x.predicate != y.predicate {
                return Some(false);
            }
            equals(&Value::of(&x.object), &Value::of(&y.object))
        }
        (Value::Triple(_), _) | (_, Value::Triple(_)) => Some(false),
        // Different kinds of term (IRI vs literal, …) are simply unequal; two literals of
        // types we can't compare are a type error unless identical.
        (Value::Iri(_) | Value::Blank(_), _) | (_, Value::Iri(_) | Value::Blank(_)) => Some(false),
        // A language-tagged string equals no typed literal, whatever its datatype means.
        (Value::LangString(..), Value::Other(_)) | (Value::Other(_), Value::LangString(..)) => {
            Some(false)
        }
        (Value::Other(_), _) | (_, Value::Other(_)) => None,
        _ => Some(false),
    }
}

/// SPARQL's `langMatches` (RFC 4647 basic filtering): whether the language tag `tag`
/// matches `range`; `*` matches every non-empty tag.
pub fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    tag.len() >= range.len()
        && tag.is_char_boundary(range.len())
        && tag[..range.len()].eq_ignore_ascii_case(range)
        && (tag.len() == range.len() || tag.as_bytes()[range.len()] == b'-')
}

/// Effective boolean value (SPARQL §17.2.2); `None` is a type error.
pub(crate) fn effective_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(b) => Some(*b),
        Value::String(s) => Some(!s.is_empty()),
        Value::Integer(i) => Some(*i != Integer::from(0)),
        // BigInteger is strictly outside i64, so never zero.
        Value::BigInteger(_) => Some(true),
        Value::Decimal(d) => Some(*d != Decimal::from(0)),
        Value::Float(f) => Some(!(f.is_nan() || *f == Float::from(0.))),
        Value::Double(d) => Some(!(d.is_nan() || *d == Double::from(0.))),
        _ => None,
    }
}

/// A term with its literal value parsed once: for sorting many terms, where [`order`]
/// would parse both literals at every comparison.
pub struct Sortable {
    term: Option<Term>,
    value: Option<Value>,
}

impl Sortable {
    pub fn new(term: Option<Term>) -> Self {
        let value = match &term {
            Some(Term::Literal(literal)) => Some(Value::of_literal(literal)),
            _ => None,
        };
        Self { term, value }
    }

    /// [`order`] of the two terms.
    pub fn order(&self, other: &Self) -> Ordering {
        match (&self.term, &self.value, &other.term, &other.value) {
            (Some(Term::Literal(x)), Some(xv), Some(Term::Literal(y)), Some(yv)) => {
                order_literals(xv, x, yv, y)
            }
            _ => order(self.term.as_ref(), other.term.as_ref()),
        }
    }
}

/// `ORDER BY` order (SPARQL §15.1): unbound < blank nodes < IRIs < literals < triple
/// terms. It is a total order, as sorting needs (a comparison that isn't one makes a sort
/// panic or return a wrong order): literals first by their kind of value (numbers,
/// booleans, strings, language-tagged strings by language, each date and time type,
/// durations, the rest), then by value within it, then by lexical form, datatype, language
/// and direction. Where SPARQL's `<` orders two values, this order agrees; where `<` leaves
/// them unordered (a number and a string, dates with and without a timezone close to each
/// other, durations of months and of days, NaN), the order is this implementation's
/// choice (§15.1 leaves it to the implementation), made per value, never per pair: a
/// choice per pair (by value where comparable, else by text) is not transitive (2 < 10 by
/// value, "10" < "15" < "2" by text).
pub fn order(a: Option<&Term>, b: Option<&Term>) -> Ordering {
    fn rank(term: Option<&Term>) -> u8 {
        match term {
            None => 0,
            Some(Term::BlankNode(_)) => 1,
            Some(Term::NamedNode(_)) => 2,
            Some(Term::Literal(_)) => 3,
            // SPARQL 1.2: triple terms after literals.
            Some(Term::Triple(_)) => 4,
        }
    }
    match (a, b) {
        // Triple terms by subject, then predicate, then object.
        (Some(Term::Triple(x)), Some(Term::Triple(y))) => {
            let subject = |t: &nrese_rdf::Triple| Term::from(t.subject.clone());
            order(Some(&subject(x)), Some(&subject(y)))
                .then_with(|| x.predicate.as_str().cmp(y.predicate.as_str()))
                .then_with(|| order(Some(&x.object), Some(&y.object)))
        }
        (Some(Term::BlankNode(x)), Some(Term::BlankNode(y))) => x.as_str().cmp(y.as_str()),
        (Some(Term::NamedNode(x)), Some(Term::NamedNode(y))) => x.as_str().cmp(y.as_str()),
        (Some(Term::Literal(x)), Some(Term::Literal(y))) => {
            order_literals(&Value::of_literal(x), x, &Value::of_literal(y), y)
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

/// [`order`] of equal inline integers is by datatype IRI: each datatype's rank among the
/// 13 IRIs, by the engine's derived datatype code; `xsd:integer` is [`INTEGER_RANK`].
const DERIVED_RANK: [u8; 12] = [6, 4, 3, 1, 8, 0, 5, 11, 10, 12, 9, 7];
const INTEGER_RANK: u8 = 2;

/// An order key of `id`: its value's offset (`TermId::inline_integer_fields`) above, its
/// datatype's rank in the low 4 bits. It takes the ids of inline integers (`xsd:integer`
/// and the datatypes derived from it, each a canonical lexical form), and the comparison of
/// two keys is [`order`] of the two terms; [`order_key_id`] gives the id back; no key is 0
/// or `u64::MAX`. `None` for other ids (dictionary literals, other types), whose order is
/// [`order`]'s own.
#[inline]
pub(crate) fn order_key(id: TermId) -> Option<u64> {
    let (offset, code) = id.inline_integer_fields()?;
    let rank = code.map_or(INTEGER_RANK, |code| DERIVED_RANK[code as usize]);
    Some(offset << 4 | rank as u64)
}

/// The id of an [`order_key`].
pub(crate) fn order_key_id(key: u64) -> TermId {
    let rank = (key & 15) as u8;
    let code = DERIVED_RANK
        .iter()
        .position(|&r| r == rank)
        .map(|code| code as u8);
    TermId::from_inline_integer_fields(key >> 4, code)
}

/// [`order`] of two literals, given their values.
fn order_literals(xv: &Value, x: &Literal, yv: &Value, y: &Literal) -> Ordering {
    kind(xv)
        .cmp(&kind(yv))
        .then_with(|| within_kind(xv, yv))
        .then_with(|| {
            // The canonical forms ("03" as "3", an xsd:int as an xsd:integer), then the
            // datatype, the language and the base direction (last, so strings differing
            // only in it don't tie).
            let (Term::Literal(x), Term::Literal(y)) =
                (canonical(x.clone().into()), canonical(y.clone().into()))
            else {
                unreachable!("canonical keeps literals literals")
            };
            (
                x.value(),
                x.datatype(),
                x.language(),
                x.direction().map(|d| d.as_str()),
            )
                .cmp(&(
                    y.value(),
                    y.datatype(),
                    y.language(),
                    y.direction().map(|d| d.as_str()),
                ))
        })
        // Literals of one canonical form ("1" and "01") by their own lexical form and
        // datatype: the order is total over distinct terms, so every evaluation (and
        // every `LIMIT` cutting through equal values) takes the same one first.
        .then_with(|| (x.value(), x.datatype()).cmp(&(y.value(), y.datatype())))
}

/// The kinds of literal value `ORDER BY` ranks apart ([`order`]).
fn kind(value: &Value) -> u8 {
    match value {
        Value::Integer(_)
        | Value::BigInteger(_)
        | Value::Decimal(_)
        | Value::Float(_)
        | Value::Double(_) => 0,
        Value::Boolean(_) => 1,
        Value::String(_) => 2,
        Value::LangString(..) => 3,
        Value::Date(_) => 4,
        Value::DateTime(_) => 5,
        Value::Time(_) => 6,
        Value::GYear(_) => 7,
        Value::GYearMonth(_) => 8,
        Value::GMonth(_) => 9,
        Value::GMonthDay(_) => 10,
        Value::GDay(_) => 11,
        Value::Duration(_) | Value::YearMonthDuration(_) | Value::DayTimeDuration(_) => 12,
        Value::Other(_) | Value::Iri(_) | Value::Blank(_) | Value::Triple(_) => 13,
    }
}

/// Two values of one [`kind`] by a key of each (so the result is transitive), equal where
/// the key is: numbers as doubles (NaN first; an integer or decimal before a float or
/// double of the same double value, integers and decimals among themselves exactly), dates
/// and times without a timezone as if in UTC (where `<` orders them, the same order),
/// durations by their average length (months of 30.436875 days), then months and seconds.
fn within_kind(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Boolean(x), Value::Boolean(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::LangString(x, lx), Value::LangString(y, ly)) => (lx, x).cmp(&(ly, y)),
        (Value::Date(x), Value::Date(y)) => utc(*x, *y, Date::or_timezone),
        (Value::DateTime(x), Value::DateTime(y)) => utc(*x, *y, DateTime::or_timezone),
        (Value::Time(x), Value::Time(y)) => utc(*x, *y, Time::or_timezone),
        (Value::GYear(x), Value::GYear(y)) => utc(*x, *y, GYear::or_timezone),
        (Value::GYearMonth(x), Value::GYearMonth(y)) => utc(*x, *y, GYearMonth::or_timezone),
        (Value::GMonth(x), Value::GMonth(y)) => utc(*x, *y, GMonth::or_timezone),
        (Value::GMonthDay(x), Value::GMonthDay(y)) => utc(*x, *y, GMonthDay::or_timezone),
        (Value::GDay(x), Value::GDay(y)) => utc(*x, *y, GDay::or_timezone),
        _ if a.is_numeric() && b.is_numeric() => numeric_key(a).cmp(&numeric_key(b)),
        _ => match (a.duration(), b.duration()) {
            (Some(x), Some(y)) => duration_key(x).cmp(&duration_key(y)),
            _ => Ordering::Equal,
        },
    }
}

/// Two dates or times of one type, each without a timezone taken as in UTC: then both have
/// one, and their instants are totally ordered.
fn utc<T: PartialOrd + Copy>(x: T, y: T, or_timezone: fn(T, TimezoneOffset) -> T) -> Ordering {
    or_timezone(x, TimezoneOffset::UTC)
        .partial_cmp(&or_timezone(y, TimezoneOffset::UTC))
        .unwrap_or(Ordering::Equal)
}

/// A number's key for [`within_kind`]: whether it is a number at all (NaN isn't), its
/// double value (-0 as 0), its exact-vs-floating bucket (`-INF` floats before negative
/// integers past `-f64::MAX`, exact values in the middle, other floats/doubles including
/// `+INF` after), and the exact value of an integer or decimal.
fn numeric_key(value: &Value) -> (bool, OrderedDouble, u8, Option<Exact<'_>>) {
    let double = to_double(value).map_or(f64::NAN, f64::from);
    let exact = Exact::of(value);
    let bucket = if exact.is_some() {
        1
    } else if double == f64::NEG_INFINITY {
        0
    } else {
        2
    };
    (
        !double.is_nan(),
        OrderedDouble(if double == 0. { 0. } else { double }),
        bucket,
        exact,
    )
}

/// A duration's key for [`within_kind`]: its average length in seconds, then its months
/// and seconds.
fn duration_key(duration: Duration) -> (OrderedDouble, i64, Decimal) {
    const MONTH_SECONDS: f64 = 30.436_875 * 86_400.;
    let seconds = f64::from(Double::from(duration.as_seconds()));
    (
        OrderedDouble(duration.all_months() as f64 * MONTH_SECONDS + seconds),
        duration.all_months(),
        duration.as_seconds(),
    )
}

/// A double ordered by [`f64::total_cmp`] (keys hold no NaN, see [`numeric_key`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedDouble(f64);

impl Eq for OrderedDouble {}

impl PartialOrd for OrderedDouble {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedDouble {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.0.is_nan() && other.0.is_nan() {
            return Ordering::Equal;
        }
        self.0.total_cmp(&other.0)
    }
}

/// A literal of a core XSD type in its value's canonical form (derived integer types as
/// xsd:integer), for casts and for the implementation-defined tail of the ORDER BY order;
/// other terms unchanged. Stored terms that are bound directly keep their lexical form.
pub fn canonical(term: Term) -> Term {
    let Term::Literal(literal) = &term else {
        return term;
    };
    let datatype = literal.datatype();
    let canonical =
        |lexical: String| Literal::new_typed_literal(lexical, datatype.into_owned()).into();
    match Value::of_literal(literal) {
        // Every integer type becomes xsd:integer.
        Value::Integer(i) => Literal::new_typed_literal(i.to_string(), xsd::INTEGER).into(),
        Value::BigInteger(b) => Literal::new_typed_literal(b.to_string(), xsd::INTEGER).into(),
        Value::Decimal(d) => canonical(d.to_string()),
        Value::Double(d) => canonical(d.to_string()),
        Value::Float(f) => canonical(f.to_string()),
        Value::Boolean(b) => canonical(b.to_string()),
        Value::Date(d) => canonical(d.to_string()),
        Value::DateTime(d) => canonical(d.to_string()),
        Value::Time(d) => canonical(d.to_string()),
        Value::GYear(d) => canonical(d.to_string()),
        Value::GYearMonth(d) => canonical(d.to_string()),
        Value::GMonth(d) => canonical(d.to_string()),
        Value::GMonthDay(d) => canonical(d.to_string()),
        Value::GDay(d) => canonical(d.to_string()),
        Value::Duration(d) => canonical(d.to_string()),
        Value::YearMonthDuration(d) => canonical(d.to_string()),
        Value::DayTimeDuration(d) => canonical(d.to_string()),
        _ => term,
    }
}

/// The literal for a boolean result.
pub(crate) fn boolean_term(value: bool) -> Term {
    Literal::new_typed_literal(if value { "true" } else { "false" }, xsd::BOOLEAN).into()
}

/// True for `rdf:langString`-typed literals; used by `DATATYPE` and `LANG`.
pub(crate) fn is_lang_string(literal: &Literal) -> bool {
    literal.datatype() == rdf::LANG_STRING
}

#[cfg(test)]
mod tests {
    use nrese_rdf::NamedNode;

    use super::*;

    fn typed(lexical: &str, datatype: &str) -> Term {
        Literal::new_typed_literal(
            lexical,
            NamedNode::new_unchecked(format!("http://www.w3.org/2001/XMLSchema#{datatype}")),
        )
        .into()
    }

    /// An integer-derived literal outside its datatype's range is ill-typed: not a number
    /// (so arithmetic on it is an error), but a literal of its own (found by the Jena oracle:
    /// `"300"^^xsd:byte * 1.5` was 450).
    #[test]
    fn integers_outside_their_derived_range_are_not_numbers() {
        for (lexical, datatype, number) in [
            ("127", "byte", true),
            ("128", "byte", false),
            ("300", "byte", false),
            ("-129", "byte", false),
            ("255", "unsignedByte", true),
            ("256", "unsignedByte", false),
            ("-1", "unsignedInt", false),
            ("0", "positiveInteger", false),
            ("0", "nonNegativeInteger", true),
            ("1", "negativeInteger", false),
            ("2147483648", "int", false),
            ("2147483647", "int", true),
            ("123456789012", "integer", true),
        ] {
            let value = Value::of(&typed(lexical, datatype));
            assert_eq!(
                matches!(value, Value::Integer(_)),
                number,
                "{lexical}^^xsd:{datatype}"
            );
        }
    }

    #[test]
    fn big_integer_bounds_and_canonical_forms() {
        let huge_pos = format!("1{}", "0".repeat(400));
        let huge_neg = format!("-1{}", "0".repeat(400));
        // Values fitting in i64 stay on the Value::Integer fast path.
        for (lexical, datatype) in [
            ("9223372036854775807", "integer"),
            ("-9223372036854775808", "integer"),
            ("9223372036854775807", "long"),
            ("9223372036854775807", "unsignedLong"),
            ("+0000000000000000000000000001", "positiveInteger"),
            ("-0000000000000000000000000000", "nonNegativeInteger"),
        ] {
            assert!(
                matches!(Value::of(&typed(lexical, datatype)), Value::Integer(_)),
                "expected Value::Integer for {lexical}^^xsd:{datatype}"
            );
        }
        // Valid integers outside i64 become Value::BigInteger with canonical Display.
        for (lexical, datatype, expected) in [
            ("9223372036854775808", "integer", "9223372036854775808"),
            (
                "+0009223372036854775808",
                "nonNegativeInteger",
                "9223372036854775808",
            ),
            (
                "9223372036854775808",
                "positiveInteger",
                "9223372036854775808",
            ),
            ("9223372036854775808", "unsignedLong", "9223372036854775808"),
            (
                "18446744073709551615",
                "unsignedLong",
                "18446744073709551615",
            ),
            ("18446744073709551616", "integer", "18446744073709551616"),
            ("-9223372036854775809", "integer", "-9223372036854775809"),
            (
                "-009223372036854775809",
                "negativeInteger",
                "-9223372036854775809",
            ),
            (
                "-9223372036854775809",
                "nonPositiveInteger",
                "-9223372036854775809",
            ),
            (huge_pos.as_str(), "integer", huge_pos.as_str()),
            (huge_pos.as_str(), "nonNegativeInteger", huge_pos.as_str()),
            (huge_pos.as_str(), "positiveInteger", huge_pos.as_str()),
            (huge_neg.as_str(), "integer", huge_neg.as_str()),
            (huge_neg.as_str(), "nonPositiveInteger", huge_neg.as_str()),
            (huge_neg.as_str(), "negativeInteger", huge_neg.as_str()),
        ] {
            let term = typed(lexical, datatype);
            match Value::of(&term) {
                Value::BigInteger(b) => {
                    assert_eq!(b.to_string(), expected, "{lexical}^^xsd:{datatype}")
                }
                other => panic!(
                    "expected Value::BigInteger for {lexical}^^xsd:{datatype}, got {other:?}"
                ),
            }
            assert_eq!(
                canonical(term),
                typed(expected, "integer"),
                "canonical({lexical}^^xsd:{datatype})"
            );
        }
        // Out-of-range bounded subtypes and malformed big integer forms stay Value::Other.
        for (lexical, datatype) in [
            ("9223372036854775808", "long"),
            ("-9223372036854775809", "long"),
            ("18446744073709551616", "unsignedLong"),
            ("-9223372036854775809", "unsignedLong"),
            (huge_pos.as_str(), "unsignedLong"),
            (huge_neg.as_str(), "unsignedLong"),
            (huge_neg.as_str(), "nonNegativeInteger"),
            (huge_neg.as_str(), "positiveInteger"),
            (huge_pos.as_str(), "nonPositiveInteger"),
            (huge_pos.as_str(), "negativeInteger"),
            ("9223372036854775808a", "integer"),
            (" 9223372036854775808", "integer"),
            ("9223372036854775808.0", "integer"),
            ("1e30", "integer"),
            ("", "integer"),
            ("+", "integer"),
            ("-", "integer"),
        ] {
            assert!(
                matches!(Value::of(&typed(lexical, datatype)), Value::Other(_)),
                "expected Value::Other for {lexical}^^xsd:{datatype}"
            );
        }
    }

    /// Literals whose values `<` orders only partly: numbers of every type (NaN, -0, an
    /// integer and a float of nearly the same value), strings, language strings, dates
    /// with and without timezones close to each other, durations of months and of days,
    /// unknown datatypes.
    fn pool() -> Vec<Term> {
        let mut terms = vec![
            typed("2", "integer"),
            typed("02", "integer"),
            typed("+2", "integer"),
            typed("10", "integer"),
            typed("02", "int"),
            typed("1", "unsignedByte"),
            typed("1.0", "decimal"),
            typed("0.1", "decimal"),
            typed("0.10000000149011612", "decimal"),
            typed("0.1", "float"),
            typed("0.1", "double"),
            typed("NaN", "double"),
            typed("-0", "double"),
            typed("0", "integer"),
            typed("INF", "float"),
            typed("1e300", "double"),
            typed("9007199254740993", "integer"),
            typed("9007199254740992", "double"),
            typed("9223372036854775807", "integer"),
            typed("9223372036854775808", "integer"),
            typed("9223372036854775808", "unsignedLong"),
            typed("+09223372036854775808", "nonNegativeInteger"),
            typed("18446744073709551615", "unsignedLong"),
            typed("18446744073709551616", "unsignedLong"),
            typed("18446744073709551616", "double"),
            typed("100000000000000000000", "integer"),
            typed("100000000000000000000.0", "decimal"),
            typed("100000000000000000002", "positiveInteger"),
            typed("170141183460469231731", "integer"),
            typed("170141183460469231731.687303715884105727", "decimal"),
            typed("170141183460469231732", "integer"),
            typed("-170141183460469231731", "integer"),
            typed("-170141183460469231731.687303715884105728", "decimal"),
            typed("-170141183460469231732", "integer"),
            typed("3.4028235E38", "float"),
            typed("-3.4028235E38", "float"),
            typed("340282356779733661637539395458142568447", "integer"),
            typed("340282356779733661637539395458142568448", "integer"),
            typed("-340282356779733661637539395458142568448", "integer"),
            typed("-9223372036854775808", "integer"),
            typed("-9223372036854775809", "integer"),
            typed("-9223372036854775809", "negativeInteger"),
            typed("-1e300", "double"),
            typed("-INF", "double"),
            typed("-INF", "float"),
            typed("INF", "double"),
            typed(&format!("1{}", "0".repeat(40)), "integer"),
            typed(&format!("-1{}", "0".repeat(40)), "integer"),
            typed(&format!("1{}", "0".repeat(400)), "integer"),
            typed(&format!("1{}", "0".repeat(401)), "integer"),
            typed(&format!("-1{}", "0".repeat(400)), "integer"),
            typed(&format!("-1{}", "0".repeat(401)), "integer"),
            typed("true", "boolean"),
            typed("2000-01-01T00:00:00", "dateTime"),
            typed("2000-01-01T10:00:00Z", "dateTime"),
            typed("2000-01-01T20:00:00", "dateTime"),
            typed("2000-01-02T00:00:00+14:00", "dateTime"),
            typed("2000-01-01", "date"),
            typed("2000-01-01Z", "date"),
            typed("P1M", "duration"),
            typed("P30D", "duration"),
            typed("P31D", "dayTimeDuration"),
            typed("P1Y", "yearMonthDuration"),
            typed("abc", "unknownType"),
            Literal::new_simple_literal("15").into(),
            Literal::new_simple_literal("2").into(),
            Literal::new_simple_literal("10").into(),
            Literal::new_language_tagged_literal_unchecked("b", "en").into(),
            Literal::new_language_tagged_literal_unchecked("a", "de").into(),
        ];
        terms.push(NamedNode::new_unchecked("http://example.com/x").into());
        terms
    }

    #[test]
    fn the_order_by_order_is_total_and_agrees_with_less_than() {
        let terms = pool();
        let all: Vec<Option<&Term>> = terms.iter().map(Some).chain([None]).collect();
        let sortable: Vec<Sortable> = all.iter().map(|t| Sortable::new(t.cloned())).collect();
        for (i, &a) in all.iter().enumerate() {
            for (j, &b) in all.iter().enumerate() {
                let ab = order(a, b);
                assert_eq!(ab, order(b, a).reverse(), "antisymmetric: {a:?} {b:?}");
                // Total over distinct terms: only a term ties with itself, so a `LIMIT`
                // cutting through equal values takes the same one everywhere.
                assert_eq!(ab.is_eq(), a == b, "total: {a:?} {b:?}");
                assert_eq!(ab, sortable[i].order(&sortable[j]), "cached: {a:?} {b:?}");
                // Where `<` orders two literals, `ORDER BY` agrees.
                if let (Some(Term::Literal(x)), Some(Term::Literal(y))) = (a, b)
                    && let Some(less) = compare(&Value::of_literal(x), &Value::of_literal(y))
                    && less.is_ne()
                {
                    assert_eq!(ab, less, "agrees with <: {x} {y}");
                }
                for &c in &all {
                    let (bc, ac) = (order(b, c), order(a, c));
                    if ab.is_le() && bc.is_le() {
                        assert!(ac.is_le(), "transitive: {a:?} <= {b:?} <= {c:?}");
                    }
                }
            }
        }
        // And sorting doesn't panic, whatever the input order.
        let mut shuffled = all.clone();
        for round in 0..50 {
            shuffled.rotate_left(round % 7 + 1);
            let n = shuffled.len();
            shuffled.swap(round % n, (round * 13) % n);
            let mut sorted = shuffled.clone();
            sorted.sort_by(|a, b| order(*a, *b));
        }
    }

    /// [`order_key`] on canonical inline integers: each of the 13 datatypes at the bounds of
    /// its value space and of the inline ranges, and at small values all of them share. Each
    /// key holds the term's value plus 2^59 above its datatype's position among the 13 sorted
    /// IRIs, gives the term's id back, and compares with every other key as [`order`] compares
    /// the two terms. That comparison is exhaustive over these terms only; values between them
    /// are covered by the key's form, the same affine map (value + 2^59) for every value of
    /// one datatype, which keeps order and is undone exactly.
    #[test]
    fn order_key_is_term_order_on_inline_integers() {
        use nrese_engine::{Engine, EngineConfig, TermKind};
        const BY_IRI: [&str; 13] = [
            "byte",
            "int",
            "integer",
            "long",
            "negativeInteger",
            "nonNegativeInteger",
            "nonPositiveInteger",
            "positiveInteger",
            "short",
            "unsignedByte",
            "unsignedInt",
            "unsignedLong",
            "unsignedShort",
        ];
        assert!(BY_IRI.is_sorted());
        let (plain, derived) = (1i128 << 59, 1i128 << 55);
        let types: [(&str, i128, i128); 13] = [
            ("integer", i128::MIN, i128::MAX),
            ("nonPositiveInteger", i128::MIN, 0),
            ("negativeInteger", i128::MIN, -1),
            ("long", i64::MIN.into(), i64::MAX.into()),
            ("int", i32::MIN.into(), i32::MAX.into()),
            ("short", i16::MIN.into(), i16::MAX.into()),
            ("byte", i8::MIN.into(), i8::MAX.into()),
            ("nonNegativeInteger", 0, i128::MAX),
            ("unsignedLong", 0, u64::MAX.into()),
            ("unsignedInt", 0, u32::MAX.into()),
            ("unsignedShort", 0, u16::MAX.into()),
            ("unsignedByte", 0, u8::MAX.into()),
            ("positiveInteger", 1, i128::MAX),
        ];
        let mut values: Vec<i128> = (-4..=4).collect();
        for bound in [
            plain,
            derived,
            1 << 31,
            1 << 32,
            1 << 15,
            1 << 16,
            1 << 7,
            1 << 8,
        ] {
            values.extend([-bound - 1, -bound, -bound + 1, bound - 2, bound - 1, bound]);
        }
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let snapshot = engine.snapshot();
        let mut terms = Vec::new();
        for (datatype, low, high) in types {
            let inline = if datatype == "integer" {
                plain
            } else {
                derived
            };
            for &value in values.iter().filter(|v| (low..=high).contains(*v)) {
                let term = typed(&value.to_string(), datatype);
                let id = snapshot
                    .lookup(term.as_ref())
                    .filter(|id| matches!(id.kind(), TermKind::Integer | TermKind::DerivedInteger));
                assert_eq!(
                    id.is_some(),
                    (-inline..inline).contains(&value),
                    "inline {term}"
                );
                terms.extend(id.map(|id| (term, id, value, datatype)));
            }
        }
        assert!(terms.len() > 150, "{} terms", terms.len());
        for (term, id, value, datatype) in &terms {
            let key = order_key(*id).unwrap();
            let rank = BY_IRI.iter().position(|d| d == datatype).unwrap();
            assert_eq!(key & 15, rank as u64, "rank of {term}");
            assert_eq!(i128::from(key >> 4), value + plain, "offset of {term}");
            assert!(key != 0 && key != u64::MAX, "{term}");
            assert_eq!(order_key_id(key), *id, "round trip of {term}");
            for (other, other_id, ..) in &terms {
                let ours = key.cmp(&order_key(*other_id).unwrap());
                assert_eq!(
                    ours,
                    order(Some(term), Some(other)),
                    "{term} against {other}"
                );
            }
        }
    }
}
