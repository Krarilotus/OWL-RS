//! A local [`Vocabulary`] and N-Triples-lite helpers for tests and tools.

use std::collections::HashMap;

use super::ir::Vocabulary;

/// Interns terms into dense ids, keyed by their N-Triples form.
#[derive(Debug, Default, Clone)]
pub struct LocalVocabulary {
    ids: HashMap<String, u64>,
    terms: Vec<String>,
}

impl LocalVocabulary {
    /// The id of a term in N-Triples syntax (`<iri>`, `_:b`, `"lex"^^<dt>`, `"lex"@en`).
    pub fn term(&mut self, text: &str) -> u64 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(text.to_owned());
        self.ids.insert(text.to_owned(), id);
        id
    }

    pub fn text(&self, id: u64) -> &str {
        &self.terms[id as usize]
    }
}

impl Vocabulary for LocalVocabulary {
    fn iri(&mut self, iri: &str) -> u64 {
        self.term(&format!("<{iri}>"))
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.term(&format!("\"{lexical}\"^^<{datatype}>"))
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.term(&format!("\"{lexical}\"@{language}"))
    }
}
