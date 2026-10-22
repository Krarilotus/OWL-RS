//! OWL 2 RL's datatype consistency rules (table 8), completion plan 1.6.
//!
//! The rules derive facts about literals that RDF can't store (a literal subject); the
//! reasoner computes them, and the store drops them after checking two rules on them:
//!
//! - **dt-not-type:** `lt rdf:type dt` where the literal `lt` has no data value in the
//!   value space of the datatype `dt`: a string where a property's range is `xsd:integer`,
//!   a decimal where it is `xsd:double` (their value spaces are disjoint in OWL 2), 300
//!   where it is `xsd:byte`, `" a"` where it is `xsd:token`, and an ill-typed literal
//!   (`"abc"^^xsd:integer`), which has a value in no space.
//! - **dt-diff** (with eq-diff1): `lt1 owl:sameAs lt2` for literals with different data
//!   values: a functional data property with two values.
//!
//! Values, value spaces and their identity are the OWL 2 datatype map's
//! ([`nrese_xsd::owl`]): `+0.0` and `-0.0` are two values, NaN is one, `"1"^^xsd:integer`
//! and `"1.0"^^xsd:decimal` are one. A datatype outside the map, and a value the map can't
//! represent (a number past 128 bits), aren't judged.
//!
//! Not done: dt-type1/2 and dt-eq (typing and equating literals by value), which would
//! fold lexical forms into each other.

use crate::ir::{Triple, Violations};
use nrese_engine::{TermId, TermKind};
use nrese_rdf::{Literal, Term};
use nrese_xsd::owl::{Datatype, LiteralError, Value, ValueSet};
use std::collections::HashMap;

/// What the datatype map says of a literal.
enum Judged {
    /// Its data value.
    Value(Value),
    /// Ill-typed: its lexical form is none of its datatype's, so it has no value.
    IllTyped,
    /// Not judged: a datatype outside the map, or a value it can't represent.
    Unsupported,
}

fn judge(literal: &Literal) -> Judged {
    let Some(datatype) = Datatype::from_iri(literal.datatype().as_str()) else {
        return Judged::Unsupported;
    };
    match Value::parse(literal.value(), datatype, literal.language()) {
        Ok(value) => Judged::Value(value),
        Err(LiteralError::IllTyped(_)) => Judged::IllTyped,
        Err(LiteralError::Unsupported(_)) => Judged::Unsupported,
    }
}

fn is_literal(id: u64) -> bool {
    !matches!(
        TermId::from_raw(id).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

/// The datatype violations among `facts` (derived facts, generalised ones included).
pub(crate) fn violations(
    facts: &[Triple],
    rdf_type: u64,
    same_as: Option<u64>,
    decode: &dyn Fn(u64) -> Option<Term>,
    out: &mut Violations,
) {
    let literal = |id: u64| match decode(id) {
        Some(Term::Literal(l)) => Some(l),
        _ => None,
    };
    // Each range datatype's value space, made once per call.
    let mut spaces: HashMap<u64, Option<ValueSet>> = HashMap::new();
    for &[s, p, o] in facts {
        if !is_literal(s) {
            continue;
        }
        if p == rdf_type {
            let Some(lt) = literal(s) else {
                continue;
            };
            let space = spaces.entry(o).or_insert_with(|| match decode(o) {
                Some(Term::NamedNode(dt)) => Datatype::from_iri(dt.as_str()).map(ValueSet::of),
                _ => None,
            });
            let Some(space) = space else {
                continue;
            };
            let outside = match judge(&lt) {
                Judged::Value(value) => !space.contains(&value),
                Judged::IllTyped => true,
                Judged::Unsupported => false,
            };
            if outside {
                out.record_check("dt-not-type", [s, o], [s, p, o]);
            }
        } else if Some(p) == same_as && is_literal(o) && s < o {
            let (Some(a), Some(b)) = (literal(s), literal(o)) else {
                continue;
            };
            if let (Judged::Value(x), Judged::Value(y)) = (judge(&a), judge(&b))
                && x != y
            {
                out.record_check("dt-diff", [s, o], [s, p, o]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
                 @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
                 ex:age a owl:DatatypeProperty, owl:FunctionalProperty ;
                     rdfs:range xsd:nonNegativeInteger .
                 ex:weight rdfs:range xsd:double .
                 ex:name rdfs:range xsd:string .
                 ex:code rdfs:range xsd:token .
                 ex:label rdfs:range xsd:Name .
                 ex:big rdfs:range xsd:unsignedLong .
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
            let mut rules: Vec<String> =
                closure.violations.iter().map(|v| v.rule.clone()).collect();
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
        // Outside a string datatype's lexical space, past 64 bits, ill-typed (RSH-12: each
        // was accepted, a missed inconsistency).
        assert_eq!(rules("ex:a ex:code \" a\" ."), ["dt-not-type"], "no token");
        assert_eq!(rules("ex:a ex:label \"1x\" ."), ["dt-not-type"], "no Name");
        assert_eq!(
            rules("ex:a ex:big 18446744073709551616 ."),
            ["dt-not-type"],
            "past unsignedLong"
        );
        assert_eq!(
            rules("ex:a ex:age \"abc\"^^xsd:integer ."),
            ["dt-not-type"],
            "ill-typed"
        );
        // One value under two datatypes and lexical forms (the map's identity).
        assert_eq!(rules("ex:a ex:age 0, \"00\"^^xsd:int ."), none);
    }
}
