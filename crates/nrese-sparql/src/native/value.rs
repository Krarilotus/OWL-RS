//! Term values for the native executor: SPARQL comparison, effective boolean value, and the
//! `ORDER BY` order, following SPARQL 1.1 §17; where the specification leaves room, the
//! reference evaluator shares these functions, and the choices are named where made.

use std::cmp::Ordering;
use std::str::FromStr;

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{Literal, Term};
use nrese_xsd::{
    Boolean, Date, DateTime, DayTimeDuration, Decimal, Double, Duration, Float, GDay, GMonth,
    GMonthDay, GYear, GYearMonth, Integer, Time, YearMonthDuration,
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

/// Datatypes derived from `xsd:integer` that SPARQL treats as integers.
const INTEGER_TYPES: [&str; 13] = [
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
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
        } else if INTEGER_TYPES.contains(&datatype.as_str()) {
            Integer::from_str(value).ok().map(Self::Integer)
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

/// `ORDER BY` order (SPARQL §15.1): unbound < blank nodes < IRIs < literals; literals by
/// value where comparable, then by lexical form and datatype so the order is total.
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
        if let (Some(a), Some(b)) = (&self.value, &other.value)
            && let Some(ordering) = compare(a, b)
        {
            return ordering;
        }
        order(self.term.as_ref(), other.term.as_ref())
    }
}

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
            // By value where comparable (equal values tie, e.g. 1 and 1.0),
            // otherwise by (lexical form, datatype, language).
            match compare(&Value::of_literal(x), &Value::of_literal(y)) {
                Some(order) => order,
                // Otherwise the canonical forms of the values ("03" as "3", an xsd:int as
                // an xsd:integer): §15.1 leaves this order to the implementation.
                None => {
                    let (Term::Literal(x), Term::Literal(y)) =
                        (canonical(x.clone().into()), canonical(y.clone().into()))
                    else {
                        unreachable!("canonical keeps literals literals")
                    };
                    // The base direction last, so strings differing only in it don't tie.
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
                }
            }
        }
        _ => rank(a).cmp(&rank(b)),
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
