//! What the DL names for itself, apart from every term the data can hold (D16-051).
//!
//! The DL engines add terms of their own to what they reason over: the individuals an
//! entailment test asserts its counterexample of (`¬(C ⊑ D)` as `(C ⊓ ¬D)(x)`), and U1's
//! machinery (fresh classes, Skolem constants, projections, the clash). They are interned
//! like the data's terms, so each must be one no data term can be: **a made name begins
//! with `dl ` and holds spaces**, and no IRI (RFC 3987) holds a space. Every stored or
//! written IRI is an IRI, so a store's data, a query's constants and an update's terms are
//! never one of these, whatever their spelling.
//!
//! They don't begin with a space: that is how the SPARQL executor marks the constants it
//! puts into a pattern (`nrese_sparql_syntax::internal`), and U1's terms are terms the
//! executor evaluates over.

use std::fmt::Display;

/// The first word of every made name.
const PREFIX: &str = "dl ";

/// The `i`th individual of an entailment test.
pub fn individual(i: usize) -> String {
    format!("{PREFIX}individual {i}")
}

/// U1's own term `name` (one word: `clash`, `fresh3`, `sk2-0-1`, …).
pub fn u1(name: impl Display) -> String {
    let name = name.to_string();
    debug_assert!(
        !name.is_empty() && !name.contains(' '),
        "a U1 name is one word"
    );
    format!("{PREFIX}u1 {name}")
}

/// Whether `text` (an IRI's text) is a made name.
pub fn is_made(text: &str) -> bool {
    text.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No made name is an IRI, so no stored or written term is one; none begins with the
    /// space the SPARQL executor's own constants begin with; and the two kinds differ.
    #[test]
    fn made_names_are_no_iris_and_apart_from_each_other() {
        let names = [
            individual(0),
            individual(7),
            u1("clash"),
            u1("fresh0"),
            u1("sk3-1-0"),
        ];
        for name in &names {
            assert!(nrese_rdf::NamedNode::new(name.as_str()).is_err(), "{name}");
            assert!(is_made(name), "{name}");
            assert!(!name.starts_with(' '), "{name}");
        }
        let distinct: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(distinct.len(), names.len());
        assert_ne!(individual(0), u1("individual"));
        // The IRIs these were once spelled with are IRIs, and not made names.
        for iri in ["urn:nrese:dl:fresh:0", "urn:nrese:u1:clash", "dl:u1"] {
            assert!(nrese_rdf::NamedNode::new(iri).is_ok(), "{iri}");
            assert!(!is_made(iri), "{iri}");
        }
    }
}
