//! Data values for the checker: a literal's value, a value's membership in a data range,
//! and the SWRL built-ins rules use, all by the datatype map's value spaces (`nrese-xsd`),
//! one value at a time (*OWL 2 Direct Semantics* §2.2's data ranges; *OWL 2 Structural
//! Specification* §4). Nothing here knows the datatype theory or the engines.

use std::collections::HashMap;

use nrese_owl::{DataRange, Ontology, RangeId, Term};
use nrese_xsd::owl::{Datatype, Facet, LiteralError, Rational, Value, ValueSet, facet};

/// SWRL's built-ins namespace.
pub const SWRLB: &str = "http://www.w3.org/2003/11/swrlb#";

/// The ontology's literals and data ranges, read as values and sets.
pub struct Values<'a> {
    o: &'a Ontology,
    /// Datatype definitions: the defined datatype's range.
    definitions: HashMap<Term, RangeId>,
}

impl<'a> Values<'a> {
    pub fn new(o: &'a Ontology) -> Self {
        let definitions = o
            .axioms
            .iter()
            .filter_map(|a| match a {
                nrese_owl::Axiom::DatatypeDefinition(t, r) => Some((*t, *r)),
                _ => None,
            })
            .collect();
        Self { o, definitions }
    }

    /// The value of the literal `term`.
    pub fn literal(&self, term: Term) -> Result<Value, String> {
        let l = self
            .o
            .data
            .literals
            .get(&term)
            .ok_or_else(|| format!("term {term} isn't a literal the source gives"))?;
        let iri = l
            .datatype
            .as_deref()
            .ok_or_else(|| format!("the source gives no datatype of {:?}", l.lexical))?;
        let d = Datatype::from_iri(iri)
            .ok_or_else(|| format!("{iri} isn't a datatype of the OWL 2 datatype map"))?;
        Value::parse(&l.lexical, d, l.language.as_deref()).map_err(|e| match e {
            LiteralError::IllTyped(why) => format!(
                "an ill-typed literal (OWL 2 Structural Specification §5.7: its lexical form must \
                 be one of its datatype's): {why}"
            ),
            LiteralError::Unsupported(why) => why,
        })
    }

    fn datatype(&self, term: Term) -> Result<Datatype, String> {
        let iri = self
            .o
            .data
            .iris
            .get(&term)
            .ok_or_else(|| format!("datatype term {term} has no IRI the source gives"))?;
        Datatype::from_iri(iri).ok_or_else(|| format!("{iri} isn't in the OWL 2 datatype map"))
    }

    /// Whether `v` is in the data range `r`.
    pub fn member(&self, r: RangeId, v: &Value) -> Result<bool, String> {
        self.member_within(r, v, &mut Vec::new())
    }

    fn member_within(&self, r: RangeId, v: &Value, stack: &mut Vec<Term>) -> Result<bool, String> {
        Ok(match self.o.ranges.get(r.0) {
            DataRange::Literal => true,
            DataRange::Datatype(t) => match self.definitions.get(t) {
                Some(_) if stack.contains(t) => {
                    return Err("a cyclic datatype definition".into());
                }
                Some(&def) => {
                    stack.push(*t);
                    let m = self.member_within(def, v, stack)?;
                    stack.pop();
                    m
                }
                None => ValueSet::of(self.datatype(*t)?).contains(v),
            },
            DataRange::And(xs) => {
                for &x in xs {
                    if !self.member_within(x, v, stack)? {
                        return Ok(false);
                    }
                }
                true
            }
            DataRange::Or(xs) => {
                for &x in xs {
                    if self.member_within(x, v, stack)? {
                        return Ok(true);
                    }
                }
                false
            }
            DataRange::Not(x) => !self.member_within(*x, v, stack)?,
            DataRange::OneOf(ls) => {
                for &l in ls {
                    if self.literal(l)? == *v {
                        return Ok(true);
                    }
                }
                false
            }
            DataRange::Restriction(t, facets) => {
                let d = self.datatype(*t)?;
                if !ValueSet::of(d).contains(v) {
                    return Ok(false);
                }
                for &(f, l) in facets {
                    let iri = self
                        .o
                        .data
                        .iris
                        .get(&f)
                        .ok_or_else(|| format!("facet term {f} has no IRI the source gives"))?;
                    let kind =
                        Facet::from_iri(iri).ok_or_else(|| format!("{iri} isn't a facet"))?;
                    let set = facet(d, kind, &self.literal(l)?)
                        .ok_or_else(|| format!("the facet {iri} on {d:?} isn't decided"))?;
                    if !set.contains(v) {
                        return Ok(false);
                    }
                }
                true
            }
        })
    }
}

/// Whether a built-in computes its first argument from the others (arithmetic, strings),
/// rather than testing them all (comparisons).
pub fn computes(iri: &str) -> bool {
    iri.strip_prefix(SWRLB).is_some_and(|name| {
        matches!(
            name,
            "add"
                | "multiply"
                | "subtract"
                | "unaryMinus"
                | "abs"
                | "stringConcat"
                | "stringLength"
                | "upperCase"
                | "lowerCase"
        )
    })
}

/// A built-in's answer for arguments of which the first may be unbound: `Ok(Some(v))` the
/// value it computes for the first (or, with every argument bound, `v` itself where the
/// relation holds), `Ok(None)` where the relation doesn't hold, `Err` where it isn't
/// evaluated here (an unknown built-in, a value it isn't defined on here).
pub fn built_in(iri: &str, first: Option<&Value>, rest: &[Value]) -> Result<Option<Value>, String> {
    let name = iri
        .strip_prefix(SWRLB)
        .ok_or_else(|| format!("the built-in {iri} isn't one of SWRL's"))?;
    let unknown = || format!("the built-in swrlb:{name} isn't evaluated here");
    // Comparisons: every argument bound.
    let compare = |test: fn(std::cmp::Ordering) -> bool| -> Result<Option<Value>, String> {
        let a = first.ok_or_else(|| format!("swrlb:{name} with its first argument unbound"))?;
        let [b] = rest else {
            return Err(format!("swrlb:{name} takes two arguments"));
        };
        Ok(order(a, b).is_some_and(test).then(|| a.clone()))
    };
    let computed: Value = match name {
        "equal" => {
            let a = first.ok_or_else(|| format!("swrlb:{name} with its first argument unbound"))?;
            let [b] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            return Ok((a == b).then(|| a.clone()));
        }
        "notEqual" => {
            let a = first.ok_or_else(|| format!("swrlb:{name} with its first argument unbound"))?;
            let [b] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            return Ok((a != b).then(|| a.clone()));
        }
        "lessThan" => return compare(|o| o.is_lt()),
        "lessThanOrEqual" => return compare(|o| o.is_le()),
        "greaterThan" => return compare(|o| o.is_gt()),
        "greaterThanOrEqual" => return compare(|o| o.is_ge()),
        "add" | "multiply" => {
            let mut acc =
                Rational::integer(if name == "add" { 0 } else { 1 }).ok_or_else(unknown)?;
            for v in rest {
                let x = real(v, name)?;
                acc = if name == "add" {
                    plus(acc, x)
                } else {
                    times(acc, x)
                }
                .ok_or_else(|| format!("swrlb:{name}: a number past 128 bits"))?;
            }
            Value::Real(acc)
        }
        "subtract" => {
            let [a, b] = rest else {
                return Err(format!("swrlb:{name} takes three arguments"));
            };
            let negated = Rational::new(-real(b, name)?.numerator(), real(b, name)?.denominator())
                .ok_or_else(|| format!("swrlb:{name}: a number past 128 bits"))?;
            Value::Real(
                plus(real(a, name)?, negated)
                    .ok_or_else(|| format!("swrlb:{name}: a number past 128 bits"))?,
            )
        }
        "unaryMinus" | "abs" => {
            let [a] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            let a = real(a, name)?;
            let negate = name == "unaryMinus" || a.numerator() < 0;
            Value::Real(if negate {
                Rational::new(-a.numerator(), a.denominator())
                    .ok_or_else(|| format!("swrlb:{name}: a number past 128 bits"))?
            } else {
                a
            })
        }
        "stringConcat" => {
            let mut s = String::new();
            for v in rest {
                s.push_str(string(v, name)?);
            }
            Value::String(s)
        }
        "stringLength" => {
            let [a] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            Value::Real(
                Rational::integer(string(a, name)?.chars().count() as i128).ok_or_else(unknown)?,
            )
        }
        "upperCase" | "lowerCase" => {
            let [a] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            let s = string(a, name)?;
            Value::String(if name == "upperCase" {
                s.to_uppercase()
            } else {
                s.to_lowercase()
            })
        }
        "contains" | "startsWith" | "endsWith" | "stringEqualIgnoreCase" => {
            let a = first.ok_or_else(|| format!("swrlb:{name} with its first argument unbound"))?;
            let [b] = rest else {
                return Err(format!("swrlb:{name} takes two arguments"));
            };
            let (x, y) = (string(a, name)?, string(b, name)?);
            let holds = match name {
                "contains" => x.contains(y),
                "startsWith" => x.starts_with(y),
                "endsWith" => x.ends_with(y),
                _ => x.to_lowercase() == y.to_lowercase(),
            };
            return Ok(holds.then(|| a.clone()));
        }
        _ => return Err(unknown()),
    };
    // A computing built-in: its first argument is the result.
    Ok(match first {
        None => Some(computed),
        Some(v) => (*v == computed).then_some(computed),
    })
}

/// The order of two values where both are numbers of `owl:real` or both strings (else
/// unordered).
fn order(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Real(x), Value::Real(y)) => Some(x.cmp(y)),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn real(v: &Value, name: &str) -> Result<Rational, String> {
    match v {
        Value::Real(r) => Ok(*r),
        other => Err(format!(
            "swrlb:{name} on {other:?}: only numbers of owl:real are evaluated here"
        )),
    }
}

fn string<'v>(v: &'v Value, name: &str) -> Result<&'v str, String> {
    match v {
        Value::String(s) => Ok(s),
        other => Err(format!(
            "swrlb:{name} on {other:?}: only strings without a language tag are evaluated here"
        )),
    }
}

fn plus(a: Rational, b: Rational) -> Option<Rational> {
    let n = a
        .numerator()
        .checked_mul(b.denominator())?
        .checked_add(b.numerator().checked_mul(a.denominator())?)?;
    Rational::new(n, a.denominator().checked_mul(b.denominator())?)
}

fn times(a: Rational, b: Rational) -> Option<Rational> {
    Rational::new(
        a.numerator().checked_mul(b.numerator())?,
        a.denominator().checked_mul(b.denominator())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(i: i128) -> Value {
        Value::Real(Rational::integer(i).expect("small"))
    }

    /// `swrlb:add` computes its first argument or tests it; comparisons test; an unknown
    /// built-in is no answer.
    #[test]
    fn built_ins() {
        let add = format!("{SWRLB}add");
        assert_eq!(built_in(&add, None, &[int(1), int(2)]), Ok(Some(int(3))));
        assert_eq!(
            built_in(&add, Some(&int(3)), &[int(1), int(2)]),
            Ok(Some(int(3)))
        );
        assert_eq!(built_in(&add, Some(&int(4)), &[int(1), int(2)]), Ok(None));
        let less = format!("{SWRLB}lessThan");
        assert_eq!(built_in(&less, Some(&int(1)), &[int(2)]), Ok(Some(int(1))));
        assert_eq!(built_in(&less, Some(&int(2)), &[int(2)]), Ok(None));
        let subtract = format!("{SWRLB}subtract");
        assert_eq!(
            built_in(&subtract, None, &[int(5), int(7)]),
            Ok(Some(int(-2)))
        );
        assert!(built_in(&format!("{SWRLB}tan"), None, &[int(1)]).is_err());
        assert!(built_in("http://example.org/f", None, &[int(1)]).is_err());
    }
}
