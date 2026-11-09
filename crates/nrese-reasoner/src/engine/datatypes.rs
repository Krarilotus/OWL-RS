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
//! Values, value spaces and their identity are the owner's, `nrese_xsd::owl` (OWL 2's
//! datatype map, as the DL route reads it): `+0.0` and `-0.0` are two values, NaN is one,
//! one instant at two offsets is two. An ill-typed literal (a datatype of the map, a lexical
//! form outside it) has no value, so it is in no value space: under a range it breaks
//! dt-not-type (RDF 1.2 Concepts §3.4: "a semantic inconsistency").
//!
//! Not done: dt-type1/2 and dt-eq (typing and equating literals by value), which would
//! fold lexical forms into each other. Not judged: a datatype outside the map, and a value
//! beyond what the owner represents (`LiteralError::Unsupported`).

use crate::ir::{Triple, Violations};
use nrese_engine::TermId;
use nrese_exec::Budget;
use nrese_rdf::{Literal, Term};
use nrese_xsd::owl::{Datatype, LiteralError, Value, ValueSet};
use std::sync::Arc;

/// What a literal is under OWL 2's datatype map.
#[derive(Debug, PartialEq)]
enum Judged {
    Value(Value),
    /// A datatype of the map, a lexical form outside it: no value.
    IllTyped,
    /// A datatype outside the map, or a value the owner doesn't represent.
    Unjudged,
}

fn judge(literal: &Literal) -> Judged {
    let Some(datatype) = Datatype::from_iri(literal.datatype().as_str()) else {
        return Judged::Unjudged;
    };
    match Value::parse(literal.value(), datatype, literal.language()) {
        Ok(value) => Judged::Value(value),
        Err(LiteralError::IllTyped(_)) => Judged::IllTyped,
        Err(LiteralError::Unsupported(_)) => Judged::Unjudged,
    }
}

/// Whether `literal` is in the value space of the datatype `datatype`; `None` where it isn't
/// judged: `datatype` outside the map (a class, say) is asked first, so such a literal isn't
/// read at all.
fn in_value_space(literal: &Literal, datatype: &str) -> Option<bool> {
    let datatype = Datatype::from_iri(datatype)?;
    match judge(literal) {
        Judged::Value(value) => Some(ValueSet::of(datatype).contains(&value)),
        Judged::IllTyped => Some(false),
        Judged::Unjudged => None,
    }
}

/// The datatype violations among `facts` (derived facts, generalised ones included),
/// finished, charged to `memory` as [`Violations`] are: a budget without room ends the
/// list there, still inconsistent.
pub(crate) fn violations(
    facts: &[Triple],
    rdf_type: u64,
    same_as: Option<u64>,
    decode: &dyn Fn(u64) -> Option<Term>,
    memory: &Arc<Budget>,
) -> Violations {
    let literal = |id: u64| match decode(id) {
        Some(Term::Literal(l)) => Some(l),
        _ => None,
    };
    let mut out = Violations::new(Arc::clone(memory));
    let (mut not_type, mut diff) = (None, None);
    for &[s, p, o] in facts {
        if !out.complete() {
            break;
        }
        if !TermId::from_raw(s).kind().is_literal() {
            continue;
        }
        if p == rdf_type {
            let (Some(lt), Some(Term::NamedNode(dt))) = (literal(s), decode(o)) else {
                continue;
            };
            if in_value_space(&lt, dt.as_str()) == Some(false)
                && let Some(group) = *not_type.get_or_insert_with(|| out.group("dt-not-type", 2))
            {
                out.record(group, [s, o]);
            }
        } else if Some(p) == same_as && TermId::from_raw(o).kind().is_literal() && s < o {
            let (Some(a), Some(b)) = (literal(s), literal(o)) else {
                continue;
            };
            if let (Judged::Value(x), Judged::Value(y)) = (judge(&a), judge(&b))
                && x != y
                && let Some(group) = *diff.get_or_insert_with(|| out.group("dt-diff", 2))
            {
                out.record(group, [s, o]);
            }
        }
    }
    out.finish();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_rdf::NamedNode;

    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

    fn typed(value: &str, local: &str) -> Literal {
        Literal::new_typed_literal(value, NamedNode::new_unchecked(format!("{XSD}{local}")))
    }

    /// The rules on a materialisation (moved from the store's mutation pipeline tests,
    /// which keep the commit's rejection): a value outside a data property's range
    /// datatype breaks dt-not-type, a functional data property with two different values
    /// dt-diff; equal values under two lexical forms, and values in range, break neither.
    #[test]
    fn a_materialisation_reports_values_outside_their_range_and_two_functional_values() {
        use nrese_engine::{Engine, EngineConfig};
        use nrese_rdf_io::{RdfFormat, RdfParser};
        let rules = |data: &str| -> Vec<String> {
            let engine = Engine::new(EngineConfig::default()).unwrap();
            let mut tx = engine.transaction();
            let text = format!(
                "@prefix ex: <http://example.com/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
                 @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix xsd: <{XSD}> .
                 ex:age a owl:DatatypeProperty, owl:FunctionalProperty ;
                     rdfs:range xsd:nonNegativeInteger .
                 ex:weight rdfs:range xsd:double .
                 ex:name rdfs:range xsd:string .
                 ex:code rdfs:range xsd:token .
                 ex:big rdfs:range xsd:long .
                 ex:hex rdfs:range xsd:hexBinary .
                 ex:born a owl:DatatypeProperty, owl:FunctionalProperty .
                 ex:when rdfs:range xsd:dateTime .
                 ex:any rdfs:range rdfs:Literal .
                 @prefix rdf: <{RDF}> .
                 {data}"
            );
            for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
                tx.insert(quad.unwrap().as_ref());
            }
            let program =
                super::super::Program::compile(&crate::rulesets::Ruleset::Owl2Rl.into(), &|t| {
                    tx.intern(t)
                });
            tx.commit().unwrap();
            let closure = super::super::materialise(&program, &engine.snapshot());
            let mut rules: Vec<String> = closure
                .violations
                .iter()
                .map(|(rule, _)| rule.to_owned())
                .collect();
            rules.sort();
            rules.dedup();
            rules
        };
        let none: Vec<String> = Vec::new();
        assert_eq!(
            rules("ex:a ex:age 42 ; ex:weight 1.5e0 ; ex:name \"Ann\" ."),
            none
        );
        // Equal values under two lexical forms are one value.
        assert_eq!(rules("ex:a ex:age 42, \"042\"^^xsd:integer ."), none);
        assert_eq!(rules("ex:a ex:age 42, 43 ."), ["dt-diff"], "two ages");
        assert_eq!(
            rules("ex:a ex:age -1 ."),
            ["dt-not-type"],
            "below the range"
        );
        assert_eq!(
            rules("ex:a ex:age \"forty\" ."),
            ["dt-not-type"],
            "a string"
        );
        assert_eq!(
            rules("ex:a ex:weight 70 ."),
            ["dt-not-type"],
            "an integer is no double"
        );
        assert_eq!(
            rules("ex:a ex:name \"Ann\"@en ."),
            ["dt-not-type"],
            "a language string is no xsd:string"
        );
        // A string with two inner spaces is no xsd:token (XML Schema 1.1 Part 2 §3.4.2).
        assert_eq!(
            rules("ex:a ex:code \"a  b\" ."),
            ["dt-not-type"],
            "no token"
        );
        // Guard (holds on the base too).
        assert_eq!(rules("ex:a ex:code \"a b\" ."), none, "a token");
        // Every literal of OWL 2's datatype map is judged by its data value (OWL 2 Structural
        // Specification §4, OWL 2 Profiles table 8). An ill-typed literal has no value, so it is
        // in no value space (RDF 1.2 Concepts §3.4: "a semantic inconsistency"). Every row is
        // checked and the wrong ones reported together.
        let dt_not_type: &[&str] = &["dt-not-type"];
        let dt_diff: &[&str] = &["dt-diff"];
        let red: &[(&str, &[&str], &str)] = &[
            (
                "ex:a ex:age \"abc\"^^xsd:integer .",
                dt_not_type,
                "an ill-typed integer",
            ),
            (
                "ex:a ex:name \"yes\"^^xsd:boolean .",
                dt_not_type,
                "an ill-typed boolean",
            ),
            (
                "ex:a ex:hex \"F\"^^xsd:hexBinary .",
                dt_not_type,
                "an ill-typed hexBinary",
            ),
            // §4.2: integers have no bound; 2^63 is past xsd:long's.
            (
                "ex:a ex:big 9223372036854775808 .",
                dt_not_type,
                "2^63 is no long",
            ),
            (
                "ex:a ex:age 18446744073709551616, 18446744073709551617 .",
                dt_diff,
                "two ages past 2^64",
            ),
            // §4.5: the value spaces of hexBinary and base64Binary are disjoint.
            (
                "ex:a ex:hex \"AQ==\"^^xsd:base64Binary .",
                dt_not_type,
                "base64 is no hex",
            ),
            // rdf:PlainLiteral "abc@en" is the tagged string "abc"@en, no xsd:string.
            (
                "ex:a ex:name \"abc@en\"^^rdf:PlainLiteral .",
                dt_not_type,
                "a tagged plain literal",
            ),
            // §4.7: one instant at two offsets is equal but not identical: two values.
            (
                "ex:a ex:born \"1956-06-25T04:00:00-05:00\"^^xsd:dateTime, \
                 \"1956-06-25T10:00:00+01:00\"^^xsd:dateTime .",
                dt_diff,
                "one instant at two offsets",
            ),
            // A year past what is represented doesn't excuse the rest of the form (XML
            // Schema 1.1 Part 2 §3.3.7.2, §3.4.28.1): still ill-typed (XSDVAL sol-loop S2).
            (
                "ex:a ex:when \"100000000001-13-01T00:00:00Z\"^^xsd:dateTime .",
                dt_not_type,
                "a year past what is represented, month 13",
            ),
            (
                "ex:a ex:any \"100000000001-01-01T00:00:00\"^^xsd:dateTimeStamp .",
                dt_not_type,
                "a year past what is represented, a dateTimeStamp without a timezone",
            ),
        ];
        // Guards (accepted on the base too): values in their range, one value under two
        // lexical forms, and a datatype outside the map, which is not judged.
        let guards: &[(&str, &[&str], &str)] = &[
            ("ex:a ex:hex \"0F\"^^xsd:hexBinary .", &[], "a hexBinary"),
            (
                "ex:a ex:age 18446744073709551616 .",
                &[],
                "2^64 is a nonNegativeInteger",
            ),
            ("ex:a ex:big -9223372036854775808 .", &[], "-2^63 is a long"),
            (
                "ex:a ex:name \"abc@\"^^rdf:PlainLiteral .",
                &[],
                "an untagged plain literal",
            ),
            (
                "ex:a ex:born \"2000-01-01T00:00:00Z\"^^xsd:dateTime, \
                 \"2000-01-01T00:00:00.000Z\"^^xsd:dateTime .",
                &[],
                "one date-time under two lexical forms",
            ),
            (
                "ex:a ex:age \"2001\"^^xsd:gYear .",
                &[],
                "gYear is outside the map",
            ),
            // A year has no bound (XML Schema 1.1 Part 2 §3.3.7.2): one past what is
            // represented is valid, not judged, never an inconsistency (XSDVAL sol-loop S1).
            (
                "ex:a ex:when \"100000000001-01-01T00:00:00Z\"^^xsd:dateTime .",
                &[],
                "a year past what is represented, under xsd:dateTime",
            ),
            (
                "ex:a ex:any \"100000000001-01-01T00:00:00Z\"^^xsd:dateTime .",
                &[],
                "a year past what is represented, under rdfs:Literal",
            ),
        ];
        let wrong: Vec<String> = red
            .iter()
            .chain(guards)
            .filter_map(|&(data, expected, why)| {
                let got = rules(data);
                (got != expected).then(|| format!("{why}: {got:?}, not {expected:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn value_spaces() {
        let space = |l: Literal, dt: &str| in_value_space(&l, &format!("{XSD}{dt}"));
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
        // The string types' value spaces (XML Schema 1.1 Part 2 §3.4.1–3.4.7, each equal to
        // its lexical space): a string outside one isn't in it (RSH-12). Every row is checked
        // and the wrong ones reported together.
        let outside = [
            ("a\tb", "normalizedString"),
            ("a  b", "token"),
            (" a", "token"),
            ("a b", "NMTOKEN"),
            ("1a", "Name"),
            ("a:b", "NCName"),
            ("abcdefghi", "language"),
        ];
        // Guards (in the value space, so they hold on the base too).
        let inside = [
            ("a b", "normalizedString"),
            ("a b", "token"),
            ("1a", "NMTOKEN"),
            ("a:b", "Name"),
            ("_a", "NCName"),
            ("en-US", "language"),
        ];
        let rows = outside
            .map(|(v, l)| (v, l, false))
            .into_iter()
            .chain(inside.map(|(v, l)| (v, l, true)));
        let wrong: Vec<String> = rows
            .filter_map(|(value, local, expected)| {
                let got = space(Literal::new_simple_literal(value), local);
                (got != Some(expected))
                    .then(|| format!("{value:?} in xsd:{local}: {got:?}, not {expected}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert_eq!(space(typed("5", "integer"), "gYear"), None);
        assert_eq!(
            space(typed("2001", "gYear"), "integer"),
            None,
            "outside the map"
        );
        // An ill-typed literal has no value: in no value space (RDF 1.2 Concepts §3.4).
        assert_eq!(judge(&typed("abc", "integer")), Judged::IllTyped);
        assert_eq!(space(typed("abc", "integer"), "integer"), Some(false));
        let data = |l: Literal| judge(&l);
        assert_ne!(data(typed("1", "integer")), data(typed("2", "int")));
        assert_eq!(data(typed("01", "integer")), data(typed("1", "int")));
        assert_ne!(data(typed("1", "integer")), data(typed("1", "double")));
        // Identity, not IEEE equality (the W3C test "Plus and Minus Zero are Distinct").
        assert_ne!(data(typed("+0.0", "float")), data(typed("-0.0", "float")));
        assert_ne!(data(typed("0", "double")), data(typed("-0", "double")));
        assert_eq!(data(typed("NaN", "double")), data(typed("NaN", "double")));
        assert_eq!(data(typed("1.0", "double")), data(typed("1", "double")));
    }
}
