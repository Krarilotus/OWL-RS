//! Blank node labels: as written, or fresh for each document.

use std::collections::hash_map::RandomState;
use std::fmt::Write;
use std::hash::BuildHasher;

/// How a parser names the blank nodes of a document.
#[derive(Debug, Clone, Default)]
pub(crate) enum BlankNodes {
    /// The labels as written.
    #[default]
    AsWritten,
    /// A fresh name per label, the same for the label throughout the document: a 128-bit
    /// keyed hash of the label, under a key random per document. Deterministic for every
    /// part of the document (the chunks of a parallel parse share the key), so it needs
    /// no table of the labels seen.
    Fresh(RandomState),
}

impl BlankNodes {
    pub(crate) fn fresh() -> Self {
        Self::Fresh(RandomState::new())
    }

    /// The name for blank node `label`: the label, or its fresh name written into `out`.
    pub(crate) fn name<'a>(&self, label: &'a str, out: &'a mut String) -> &'a str {
        match self {
            Self::AsWritten => label,
            Self::Fresh(key) => {
                out.clear();
                let high = key.hash_one((0_u8, label));
                let low = key.hash_one((1_u8, label));
                // Starts with a letter: also an RDF/XML nodeID.
                let _ = write!(out, "b{high:016x}{low:016x}");
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_names_are_stable_within_a_document_and_differ_between_documents() {
        let (one, other) = (BlankNodes::fresh(), BlankNodes::fresh());
        let (mut a, mut b, mut c) = (String::new(), String::new(), String::new());
        let first = one.name("x", &mut a).to_owned();
        assert_eq!(one.clone().name("x", &mut b), first);
        assert_ne!(other.name("x", &mut c), first);
        assert_ne!(one.name("y", &mut a), first);
        assert_eq!(BlankNodes::AsWritten.name("x", &mut a), "x");
    }
}
