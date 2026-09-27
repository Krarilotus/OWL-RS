//! Term values for the native executor: SPARQL comparison, effective boolean value, and the
//! `ORDER BY` order, following SPARQL 1.1 §17 and matching spareval where the specification
//! leaves room (differential tests pin that).

use std::cmp::Ordering;
use std::str::FromStr;

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, Term};
use oxsdatatypes::{Boolean, Date, DateTime, Decimal, Double, Float, Integer};

/// A term's value, as far as operators distinguish them.
#[derive(Debug, Clone)]
pub(crate) enum Value {
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
    Iri(String),
    Blank(String),
    /// A literal of another datatype, or an ill-formed typed literal: compared by identity.
    Other(Literal),
}

/// Datatypes derived from `xsd:integer` that SPARQL treats as integers.
const INTEGER_TYPES: [&str; 12] = [
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
];

impl Value {
    pub(crate) fn of(term: &Term) -> Self {
        match term {
            Term::NamedNode(node) => Self::Iri(node.as_str().to_owned()),
            Term::BlankNode(node) => Self::Blank(node.as_str().to_owned()),
            Term::Literal(literal) => Self::of_literal(literal),
        }
    }

    fn of_literal(literal: &Literal) -> Self {
        if let Some(language) = literal.language() {
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
pub(crate) fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    if a.is_numeric() && b.is_numeric() {
        return compare_numeric(a, b);
    }
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::LangString(x, lx), Value::LangString(y, ly)) if lx == ly => Some(x.cmp(y)),
        (Value::Boolean(x), Value::Boolean(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Date(y)) => x.partial_cmp(y),
        (Value::DateTime(x), Value::DateTime(y)) => x.partial_cmp(y),
        _ => None,
    }
}

/// `=` (RDFterm-equal extended by value equality): `None` is a type error.
pub(crate) fn equals(a: &Value, b: &Value) -> Option<bool> {
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
        (Value::Date(x), Value::Date(y)) => x.partial_cmp(y).map(|o| o == Ordering::Equal),
        (Value::DateTime(x), Value::DateTime(y)) => x.partial_cmp(y).map(|o| o == Ordering::Equal),
        (Value::Other(x), Value::Other(y)) if x == y => Some(true),
        // Different kinds of term (IRI vs literal, …) are simply unequal; two literals of
        // types we can't compare are a type error unless identical.
        (Value::Iri(_) | Value::Blank(_), _) | (_, Value::Iri(_) | Value::Blank(_)) => Some(false),
        (Value::Other(_), _) | (_, Value::Other(_)) => None,
        _ => Some(false),
    }
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
pub(crate) fn order(a: Option<&Term>, b: Option<&Term>) -> Ordering {
    fn rank(term: Option<&Term>) -> u8 {
        match term {
            None => 0,
            Some(Term::BlankNode(_)) => 1,
            Some(Term::NamedNode(_)) => 2,
            Some(Term::Literal(_)) => 3,
        }
    }
    match (a, b) {
        (Some(Term::BlankNode(x)), Some(Term::BlankNode(y))) => x.as_str().cmp(y.as_str()),
        (Some(Term::NamedNode(x)), Some(Term::NamedNode(y))) => x.as_str().cmp(y.as_str()),
        (Some(Term::Literal(x)), Some(Term::Literal(y))) => {
            // As spareval: by value where comparable (equal values tie, e.g. 1 and 1.0),
            // otherwise by (lexical form, datatype, language).
            match compare(&Value::of_literal(x), &Value::of_literal(y)) {
                Some(order) => order,
                None => (x.value(), x.datatype(), x.language()).cmp(&(
                    y.value(),
                    y.datatype(),
                    y.language(),
                )),
            }
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

/// The term an evaluator returns for a computed value (MIN, MAX, SAMPLE): literals of the
/// core XSD types in canonical form, as spareval's value-based aggregates produce them;
/// other terms unchanged. Stored terms that are bound directly keep their lexical form.
pub(crate) fn canonical(term: Term) -> Term {
    let Term::Literal(literal) = &term else {
        return term;
    };
    let datatype = literal.datatype();
    let canonical =
        |lexical: String| Literal::new_typed_literal(lexical, datatype.into_owned()).into();
    match Value::of_literal(literal) {
        Value::Integer(i) if datatype == xsd::INTEGER => canonical(i.to_string()),
        Value::Decimal(d) => canonical(d.to_string()),
        Value::Double(d) => canonical(d.to_string()),
        Value::Float(f) => canonical(f.to_string()),
        Value::Boolean(b) => canonical(b.to_string()),
        Value::Date(d) => canonical(d.to_string()),
        Value::DateTime(d) => canonical(d.to_string()),
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
