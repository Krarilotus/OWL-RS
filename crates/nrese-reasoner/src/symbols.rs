use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolTable {
    ids_by_term: HashMap<String, u32>,
    terms_by_id: Vec<String>,
}

impl SymbolTable {
    pub fn get_or_intern(&mut self, term: &str) -> u32 {
        if let Some(id) = self.ids_by_term.get(term) {
            return *id;
        }

        let id = self.terms_by_id.len() as u32;
        let owned = term.to_owned();
        self.ids_by_term.insert(owned.clone(), id);
        self.terms_by_id.push(owned);
        id
    }

    pub fn resolve(&self, id: u32) -> Option<&str> {
        self.terms_by_id.get(id as usize).map(String::as_str)
    }

    #[cfg(test)]
    pub fn id_of(&self, term: &str) -> Option<u32> {
        self.ids_by_term.get(term).copied()
    }

    pub fn len(&self) -> usize {
        self.terms_by_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolTable;

    #[test]
    fn symbol_table_reuses_existing_ids() {
        let mut table = SymbolTable::default();
        let first = table.get_or_intern("urn:test:a");
        let second = table.get_or_intern("urn:test:a");
        let third = table.get_or_intern("urn:test:b");

        assert_eq!(first, second);
        assert_ne!(first, third);
        assert_eq!(table.id_of("urn:test:a"), Some(first));
        assert_eq!(table.resolve(first), Some("urn:test:a"));
    }
}
