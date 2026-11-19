//! Publication exceptions to the model, from docs/spec/reasoning-semantics.md.

use std::collections::BTreeMap;

use crate::{
    Facts, Triple,
    oracle::{self, Datatypes},
};

const DIAGONAL: &str = "eq-ref diagonal (56)";
const SUBJECT: &str = "literal subject (73–74)";
const PREDICATE: &str = "non-IRI predicate (73–74)";
const HIDDEN_UNION: &str = "hidden unnamed union membership (27)";
const DATATYPE_TYPE: &str = "dt-type2 (57)";
const DATATYPE_EQ: &str = "dt-eq without another derivation (57)";
const REFLEXIVITY: &str = "eq-ref-dependent row (56)";
pub const ITEMS: &[&str] = &[
    DIAGONAL,
    SUBJECT,
    PREDICATE,
    HIDDEN_UNION,
    DATATYPE_TYPE,
    DATATYPE_EQ,
    REFLEXIVITY,
    // Zero by construction. The oracle contains no RDF/RDFS axiomatic triples outside Tables 4–9.
    "axiomatic triples outside Tables 4–9 (58)",
];

pub struct Publication {
    pub required: Facts,
    pub counts: BTreeMap<&'static str, usize>,
}

pub fn project(asserted: &Facts, model: &oracle::Model) -> Publication {
    let mut result = Publication {
        required: Facts::new(),
        counts: BTreeMap::new(),
    };
    let has_literals = asserted.iter().flatten().any(|term| term.starts_with('"'));
    let without_type2 = has_literals.then(|| oracle::evaluate(asserted, Datatypes::WithoutType2));
    let without_dt_eq = has_literals.then(|| oracle::evaluate(asserted, Datatypes::Published));
    let without_eq_ref = oracle::without_eq_ref(asserted);
    for row in &model.facts {
        let reason = if without_type2
            .as_ref()
            .is_some_and(|m| !m.facts.contains(row))
        {
            Some(DATATYPE_TYPE)
        } else if let Some(reason) = excluded(row, asserted) {
            Some(reason)
        } else if without_dt_eq
            .as_ref()
            .is_some_and(|m| !m.facts.contains(row))
        {
            // An equality or consequence also supported by prp-fp stays required.
            Some(DATATYPE_EQ)
        } else if !without_eq_ref.contains(row) {
            Some(REFLEXIVITY)
        } else {
            None
        };
        if let Some(reason) = reason {
            *result.counts.entry(reason).or_default() += 1;
        } else {
            result.required.insert(row.clone());
        }
    }
    // The W3C rules exclude RDF/RDFS axiomatic triples (§4.3 after Table 9);
    // prp-ap and dt-type1 belong to the tables and remain required.
    result
}

fn excluded(row: &Triple, asserted: &Facts) -> Option<&'static str> {
    let [s, p, o] = row;
    if asserted.contains(row) {
        return None;
    }
    if p == "owl:sameAs" && s == o {
        return Some(DIAGONAL);
    }
    if s.starts_with('"') {
        return Some(SUBJECT);
    }
    if p.starts_with('"') || p.starts_with("_:") {
        return Some(PREDICATE);
    }
    if p == "rdf:type" && o.starts_with("_:") && hidden_union(o, asserted) {
        return Some(HIDDEN_UNION);
    }
    None
}

fn hidden_union(class: &str, asserted: &Facts) -> bool {
    let allowed_objects = [
        "rdfs:domain",
        "rdfs:range",
        "rdf:type",
        "rdfs:subClassOf",
        "owl:allValuesFrom",
    ];
    let definition = ["owl:unionOf", "rdf:type"];
    asserted
        .iter()
        .any(|[s, p, _]| s == class && p == "owl:unionOf")
        && asserted.iter().all(|[s, p, o]| {
            p != class
                && (s != class || definition.contains(&p.as_str()))
                && (o != class || allowed_objects.contains(&p.as_str()))
        })
}
