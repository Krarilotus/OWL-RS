//! OWL 2 RL's datatype consistency rules (table 8), completion plan 1.6.
//!
//! The rules derive facts about literals that RDF can't store (a literal subject); the
//! reasoner computes them, and the store drops them after checking two rules on them:
//!
//! - **dt-not-type:** `lt rdf:type dt` where the data value of `lt` isn't in the value
//!   space of the datatype `dt`: a string where a property's range is `xsd:integer`, a
//!   decimal where it is `xsd:double` (their value spaces are disjoint in OWL 2), 300
//!   where it is `xsd:byte`.
//! - **dt-diff** (with eq-diff1): `lt1 owl:sameAs lt2` for literals with different data
//!   values: a functional data property with two values.
//!
//! Not done: dt-type1/2 and dt-eq (typing and equating literals by value), which would
//! fold lexical forms into each other; ill-typed literals (no data value) aren't judged.
//! A datatype outside OWL 2's list is not judged either.

use nrese_engine::{TermId, TermKind};
use nrese_reasoner::v2::naive::{Triple, Violation};
use oxrdf::{Literal, Term};
use oxsdatatypes::{DateTime, Decimal, Double, Float, Integer};
use std::str::FromStr;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// A literal's data value, as far as the checks tell values apart.
#[derive(Debug, Clone, PartialEq)]
enum Data {
    /// owl:real / xsd:decimal and its subtypes.
    Decimal(Decimal),
    Double(Double),
    Float(Float),
    Boolean(bool),
    String(String),
    LangString(String, String),
    DateTime(DateTime),
    AnyUri(String),
}

fn is_literal(id: u64) -> bool {
    !matches!(
        TermId::from_raw(id).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

/// The decimal-derived integer types, with their bounds.
fn integer_bounds(local: &str) -> Option<(Option<i128>, Option<i128>)> {
    Some(match local {
        "integer" => (None, None),
        "nonNegativeInteger" => (Some(0), None),
        "positiveInteger" => (Some(1), None),
        "nonPositiveInteger" => (None, Some(0)),
        "negativeInteger" => (None, Some(-1)),
        "long" => (Some(i64::MIN.into()), Some(i64::MAX.into())),
        "int" => (Some(i32::MIN.into()), Some(i32::MAX.into())),
        "short" => (Some(i16::MIN.into()), Some(i16::MAX.into())),
        "byte" => (Some(i8::MIN.into()), Some(i8::MAX.into())),
        "unsignedLong" => (Some(0), Some(u64::MAX.into())),
        "unsignedInt" => (Some(0), Some(u32::MAX.into())),
        "unsignedShort" => (Some(0), Some(u16::MAX.into())),
        "unsignedByte" => (Some(0), Some(u8::MAX.into())),
        _ => return None,
    })
}

/// The data value of `literal`; `None` if it is ill-typed or of a datatype not judged.
fn data(literal: &Literal) -> Option<Data> {
    if let Some(language) = literal.language() {
        return Some(Data::LangString(
            literal.value().to_owned(),
            language.to_ascii_lowercase(),
        ));
    }
    let datatype = literal.datatype().as_str();
    let value = literal.value();
    let local = datatype.strip_prefix(XSD)?;
    Some(match local {
        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN" => {
            Data::String(value.to_owned())
        }
        "boolean" => Data::Boolean(match value.trim() {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return None,
        }),
        "decimal" => Data::Decimal(Decimal::from_str(value.trim()).ok()?),
        "double" => Data::Double(Double::from_str(value.trim()).ok()?),
        "float" => Data::Float(Float::from_str(value.trim()).ok()?),
        "dateTime" | "dateTimeStamp" => Data::DateTime(DateTime::from_str(value.trim()).ok()?),
        "anyURI" => Data::AnyUri(value.trim().to_owned()),
        other => {
            let integer = Integer::from_str(value.trim()).ok()?;
            let (low, high) = integer_bounds(other)?;
            let n = i128::from(i64::from(integer));
            if low.is_some_and(|l| n < l) || high.is_some_and(|h| n > h) {
                return None;
            }
            Data::Decimal(Decimal::from(integer))
        }
    })
}

/// Whether `value` is in the value space of the datatype `datatype`; `None` if the
/// datatype isn't one of OWL 2's.
fn in_value_space(value: &Data, datatype: &str) -> Option<bool> {
    if datatype == "http://www.w3.org/2000/01/rdf-schema#Literal" {
        return Some(true);
    }
    if let Some(local) = datatype.strip_prefix(RDF) {
        return match local {
            "PlainLiteral" => Some(matches!(value, Data::String(_) | Data::LangString(..))),
            "langString" => Some(matches!(value, Data::LangString(..))),
            _ => None,
        };
    }
    if let Some(local) = datatype.strip_prefix("http://www.w3.org/2002/07/owl#") {
        return match local {
            "real" | "rational" => Some(matches!(value, Data::Decimal(_))),
            _ => None,
        };
    }
    let local = datatype.strip_prefix(XSD)?;
    Some(match local {
        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN" => {
            matches!(value, Data::String(_))
        }
        "boolean" => matches!(value, Data::Boolean(_)),
        "decimal" => matches!(value, Data::Decimal(_)),
        "double" => matches!(value, Data::Double(_)),
        "float" => matches!(value, Data::Float(_)),
        "dateTime" => matches!(value, Data::DateTime(_)),
        "dateTimeStamp" => {
            matches!(value, Data::DateTime(d) if d.timezone_offset().is_some())
        }
        "anyURI" => matches!(value, Data::AnyUri(_)),
        other => {
            let (low, high) = integer_bounds(other)?;
            match value {
                Data::Decimal(d) => {
                    let Ok(integer) = Integer::try_from(*d) else {
                        return Some(false);
                    };
                    if Decimal::from(integer) != *d {
                        return Some(false);
                    }
                    let n = i128::from(i64::from(integer));
                    !(low.is_some_and(|l| n < l) || high.is_some_and(|h| n > h))
                }
                _ => false,
            }
        }
    })
}

/// The datatype violations among `facts` (derived facts, generalised ones included).
pub(crate) fn violations(
    facts: &[Triple],
    rdf_type: u64,
    same_as: Option<u64>,
    decode: &dyn Fn(u64) -> Option<Term>,
) -> Vec<Violation> {
    let literal = |id: u64| match decode(id) {
        Some(Term::Literal(l)) => Some(l),
        _ => None,
    };
    let mut out = Vec::new();
    for &[s, p, o] in facts {
        if !is_literal(s) {
            continue;
        }
        if p == rdf_type {
            let (Some(lt), Some(Term::NamedNode(dt))) = (literal(s), decode(o)) else {
                continue;
            };
            if let Some(value) = data(&lt)
                && in_value_space(&value, dt.as_str()) == Some(false)
            {
                out.push(Violation {
                    rule: "dt-not-type".to_owned(),
                    bindings: vec![s, o],
                });
            }
        } else if Some(p) == same_as && is_literal(o) && s < o {
            let (Some(a), Some(b)) = (literal(s), literal(o)) else {
                continue;
            };
            if let (Some(x), Some(y)) = (data(&a), data(&b))
                && x != y
            {
                out.push(Violation {
                    rule: "dt-diff".to_owned(),
                    bindings: vec![s, o],
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn typed(value: &str, local: &str) -> Literal {
        Literal::new_typed_literal(value, NamedNode::new_unchecked(format!("{XSD}{local}")))
    }

    #[test]
    fn value_spaces() {
        let space =
            |l: Literal, dt: &str| in_value_space(&data(&l).unwrap(), &format!("{XSD}{dt}"));
        assert_eq!(space(typed("5", "integer"), "decimal"), Some(true));
        assert_eq!(space(typed("5", "integer"), "byte"), Some(true));
        assert_eq!(space(typed("300", "integer"), "byte"), Some(false));
        assert_eq!(
            space(typed("-1", "integer"), "nonNegativeInteger"),
            Some(false)
        );
        assert_eq!(space(typed("1.5", "decimal"), "integer"), Some(false));
        assert_eq!(space(typed("2.0", "decimal"), "integer"), Some(true));
        assert_eq!(space(typed("5", "integer"), "double"), Some(false));
        assert_eq!(space(typed("5", "double"), "decimal"), Some(false));
        assert_eq!(
            space(Literal::new_simple_literal("abc"), "integer"),
            Some(false)
        );
        assert_eq!(
            space(Literal::new_simple_literal("abc"), "string"),
            Some(true)
        );
        assert_eq!(
            space(
                Literal::new_language_tagged_literal_unchecked("a", "en"),
                "string"
            ),
            Some(false)
        );
        assert_eq!(
            space(typed("2001-01-01T00:00:00", "dateTime"), "dateTimeStamp"),
            Some(false)
        );
        assert_eq!(space(typed("true", "boolean"), "boolean"), Some(true));
        assert_eq!(space(typed("5", "integer"), "gYear"), None);
        assert!(
            data(&typed("abc", "integer")).is_none(),
            "ill-typed: not judged"
        );
        assert_ne!(data(&typed("1", "integer")), data(&typed("2", "int")));
        assert_eq!(data(&typed("01", "integer")), data(&typed("1", "int")));
        assert_ne!(data(&typed("1", "integer")), data(&typed("1", "double")));
    }
}
