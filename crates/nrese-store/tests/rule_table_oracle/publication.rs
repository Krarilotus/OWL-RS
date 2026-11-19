//! Publication exceptions to the model, from docs/spec/reasoning-semantics.md.

use crate::{
    Facts, Triple,
    oracle::{self, Datatypes},
};

pub const ITEMS: &[&str] = &[
    "eq-ref diagonal (56)",
    "literal subject (73–74)",
    "non-IRI predicate (73–74)",
    "hidden unnamed union membership (27)",
    "dt-type2 (57)",
    "dt-eq without another derivation (57)",
    "axiomatic triples outside Tables 4–9 (58)",
];

pub struct Publication {
    pub required: Facts,
    pub counts: [usize; ITEMS.len()],
}

pub fn project(asserted: &Facts, model: &oracle::Model) -> Publication {
    let mut result = Publication {
        required: Facts::new(),
        counts: [0; ITEMS.len()],
    };
    let has_literals = asserted.iter().flatten().any(|term| term.starts_with('"'));
    let without_type2 = has_literals.then(|| oracle::evaluate(asserted, Datatypes::WithoutType2));
    let without_dt_eq = has_literals.then(|| oracle::evaluate(asserted, Datatypes::Published));
    for row in &model.facts {
        let exclusion = excluded(row, asserted);
        if without_type2
            .as_ref()
            .is_some_and(|m| !m.facts.contains(row))
        {
            result.counts[4] += 1;
        } else if let Some(reason) = exclusion {
            result.counts[reason] += 1;
        } else if without_dt_eq
            .as_ref()
            .is_some_and(|m| !m.facts.contains(row))
        {
            // An equality or consequence also supported by prp-fp stays required.
            result.counts[5] += 1;
        } else {
            result.required.insert(row.clone());
        }
    }
    // The W3C rules exclude RDF/RDFS axiomatic triples (§4.3 after Table 9);
    // prp-ap and dt-type1 belong to the tables and remain required.
    result
}

fn excluded(row: &Triple, asserted: &Facts) -> Option<usize> {
    let [s, p, o] = row;
    if asserted.contains(row) {
        return None;
    }
    if p == "owl:sameAs" && s == o {
        return Some(0);
    }
    if s.starts_with('"') {
        return Some(1);
    }
    if p.starts_with('"') || p.starts_with("_:") {
        return Some(2);
    }
    if p == "rdf:type" && o.starts_with("_:") && hidden_union(o, asserted) {
        return Some(3);
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
