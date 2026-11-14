//! Why the normalisation leaves an axiom out of its clauses ([`crate::Normalised::unsupported`]):
//! one cause per variant, decided here and read by type above. The text each gives
//! ([`Unsupported::as_str`], `Display`) is for people; nothing decides by it.

use std::fmt;

/// Why an axiom is in [`crate::Normalised::unsupported`]. Exhaustive on purpose: a new
/// cause is a compile error at every `match` that decides what it means for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unsupported {
    /// A datatype definition: the clauses don't cover it; a datatype theory reads it from
    /// [`crate::Normalised::definitions`].
    DatatypeDefinitions,
    /// A key: the clauses don't cover it; an engine with DL-safe rules reads it from
    /// [`crate::Normalised::rules`].
    Keys,
    /// An axiom over the universal data property (`owl:topDataProperty`).
    UniversalDataProperty,
    /// The universal object property where OWL 2 DL requires a simple property.
    UniversalObjectProperty,
    /// A cardinality whose negation is past `u32`.
    UncountedNegation,
    /// A clause or key that needs more variables than a `u16` numbers.
    TooWide,
    /// A universal that needs an automaton of an irregular role.
    IrregularRoleHierarchy,
}

impl Unsupported {
    /// The cause in words.
    pub fn as_str(self) -> &'static str {
        match self {
            Unsupported::DatatypeDefinitions => "datatype definitions (the datatype theory)",
            Unsupported::Keys => "keys (a DL-safe rule over named individuals)",
            Unsupported::UniversalDataProperty => {
                "the universal data property (owl:topDataProperty)"
            }
            Unsupported::UniversalObjectProperty => {
                "the universal object property where OWL 2 DL requires a simple property (a \
                 cardinality; functional, inverse-functional, irreflexive, asymmetric or \
                 disjoint properties: OWL 2 Structural Specification §11.1, §11.2)"
            }
            Unsupported::UncountedNegation => "a cardinality whose negation is past 4294967295",
            Unsupported::TooWide => "a clause or key with more than 65535 variables",
            Unsupported::IrregularRoleHierarchy => "an irregular role hierarchy",
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}
