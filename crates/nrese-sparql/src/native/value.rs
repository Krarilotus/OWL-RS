//! Term values for the native executor: SPARQL comparison, effective boolean value, and the
//! `ORDER BY` order, following SPARQL 1.1 §17; where the specification leaves room, the
//! reference evaluator shares these functions, and the choices are named where made.

use std::cmp::Ordering;
use std::str::FromStr;

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{Literal, Term};
use nrese_xsd::{
    Boolean, Date, DateTime, DayTimeDuration, Decimal, Double, Duration, Float, GDay, GMonth,
    GMonthDay, GYear, GYearMonth, Integer, Time, TimezoneOffset, YearMonthDuration,
};

/// A term's value, as far as operators distinguish them.
#[derive(Debug, Clone)]
pub enum Value {
    Integer(Integer),
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

/// Datatypes derived from `xsd:integer` that SPARQL treats as integers, with their value
/// ranges (XSD 1.1 Datatypes §3.4). A lexical form outside its datatype's range is
/// ill-typed: not a number, but a literal compared by identity, and an error in
/// arithmetic (`"300"^^xsd:byte * 2`), as the standard and Jena have it.
const INTEGER_TYPES: [(&str, i128, i128); 13] = [
    (
        "http://www.w3.org/2001/XMLSchema#integer",
        i128::MIN,
        i128::MAX,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#long",
        i64::MIN as i128,
        i64::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#int",
        i32::MIN as i128,
        i32::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#short",
        i16::MIN as i128,
        i16::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#byte",
        i8::MIN as i128,
        i8::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
        0,
        i128::MAX,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
        i128::MIN,
        0,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#negativeInteger",
        i128::MIN,
        -1,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#positiveInteger",
        1,
        i128::MAX,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#unsignedLong",
        0,
        u64::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#unsignedInt",
        0,
        u32::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#unsignedShort",
        0,
        u16::MAX as i128,
    ),
    (
        "http://www.w3.org/2001/XMLSchema#unsignedByte",
        0,
        u8::MAX as i128,
    ),
];

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
        } else if let Some(&(_, min, max)) = INTEGER_TYPES
            .iter()
            .find(|(iri, ..)| *iri == datatype.as_str())
        {
            Integer::from_str(value)
                .ok()
                .filter(|integer| (min..=max).contains(&i128::from(i64::from(*integer))))
                .map(Self::Integer)
        } else {
            None
        };
        parsed.unwrap_or_else(|| Self::Other(literal.clone()))
    }

    fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::Integer(_) | Self::Decimal(_) | Self::Float(_) | Self::Double(_)
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

/// Numeric comparison with SPARQL type promotion (integer → decimal → float → double).
fn compare_numeric(a: &Value, b: &Value) -> Option<Ordering> {
    use Value::{Decimal as D, Float as F, Integer as I};
    match (a, b) {
        (I(x), I(y)) => x.partial_cmp(y),
        (I(_) | D(_), I(_) | D(_)) => to_decimal(a)?.partial_cmp(&to_decimal(b)?),
        (I(_) | D(_) | F(_), I(_) | D(_) | F(_)) => to_float(a)?.partial_cmp(&to_float(b)?),
        _ => to_double(a)?.partial_cmp(&to_double(b)?),
    }
}

fn to_decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::Integer(i) => Some(Decimal::from(*i)),
        Value::Decimal(d) => Some(*d),
        _ => None,
    }
}

fn to_float(value: &Value) -> Option<Float> {
    match value {
        Value::Integer(i) => Some(Float::from(*i)),
        Value::Decimal(d) => Some(Float::from(*d)),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn to_double(value: &Value) -> Option<Double> {
    match value {
        Value::Integer(i) => Some(Double::from(*i)),
        Value::Decimal(d) => Some(Double::from(*d)),
        Value::Float(f) => Some(Double::from(*f)),
        Value::Double(d) => Some(*d),
        _ => None,
    }
}

/// `<`, `>`, `<=`, `>=`: `None` is a type error (the FILTER is then false).
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
        return compare_numeric(a, b).map(|o| o == Ordering::Equal);
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
        Value::Integer(_) | Value::Decimal(_) | Value::Float(_) | Value::Double(_) => 0,
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
/// double value (-0 as 0), whether it is binary floating point, and the exact value of an
/// integer or decimal.
fn numeric_key(value: &Value) -> (bool, OrderedDouble, bool, Option<Decimal>) {
    let double = to_double(value).map_or(f64::NAN, f64::from);
    let exact = to_decimal(value);
    (
        !double.is_nan(),
        OrderedDouble(if double == 0. { 0. } else { double }),
        exact.is_none(),
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
}
