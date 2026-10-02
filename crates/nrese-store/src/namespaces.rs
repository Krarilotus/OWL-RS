//! A store's namespace prefixes: what clients show and write IRIs with (RDF4J's
//! `/namespaces`, the console, serialisations), one set per store, kept in its data
//! directory as `namespaces.json`.
//!
//! A new store starts with the four standard prefixes. Stores from before this module kept
//! the set as `rdf4j-namespaces.json` (the RDF4J adapter owned it): that file is read when
//! `namespaces.json` doesn't exist yet, and left in place.

use std::collections::BTreeMap;
use std::path::PathBuf;

use std::sync::{Mutex, PoisonError};

use crate::error::{StoreError, StoreResult};

const FILE: &str = "namespaces.json";
const BEFORE: &str = "rdf4j-namespaces.json";

/// Prefix to namespace IRI.
pub type NamespaceMap = BTreeMap<String, String>;

#[derive(Debug)]
pub struct Namespaces {
    map: Mutex<NamespaceMap>,
    /// Where they are kept (stores on disk).
    file: Option<PathBuf>,
}

impl Namespaces {
    /// The namespaces of a store whose data directory is `dir` (`None`: in memory).
    pub(crate) fn open(dir: Option<PathBuf>) -> Self {
        let read = |path: &PathBuf| -> Option<NamespaceMap> {
            serde_json::from_slice(&std::fs::read(path).ok()?).ok()
        };
        let map = dir
            .as_ref()
            .and_then(|dir| read(&dir.join(FILE)).or_else(|| read(&dir.join(BEFORE))))
            .unwrap_or_else(standard);
        Self {
            map: Mutex::new(map),
            file: dir.map(|dir| dir.join(FILE)),
        }
    }

    /// All of them.
    pub fn all(&self) -> NamespaceMap {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn get(&self, prefix: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(prefix)
            .cloned()
    }

    /// Binds `prefix` to `iri`, replacing an earlier binding.
    pub fn set(&self, prefix: &str, iri: &str) -> StoreResult<()> {
        if iri.trim().is_empty() {
            return Err(StoreError::Configuration(format!(
                "namespace '{prefix}' needs an IRI"
            )));
        }
        self.change(|map| {
            map.insert(prefix.to_owned(), iri.trim().to_owned());
        })
    }

    /// Removes `prefix`; whether it was bound.
    pub fn remove(&self, prefix: &str) -> StoreResult<bool> {
        let mut removed = false;
        self.change(|map| removed = map.remove(prefix).is_some())?;
        Ok(removed)
    }

    pub fn clear(&self) -> StoreResult<()> {
        self.change(BTreeMap::clear)
    }

    /// Changes the set and keeps it (a temporary file renamed over the old one).
    fn change(&self, change: impl FnOnce(&mut NamespaceMap)) -> StoreResult<()> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut map);
        let Some(path) = &self.file else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&*map)
            .map_err(|error| StoreError::Configuration(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

fn standard() -> NamespaceMap {
    [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ]
    .into_iter()
    .map(|(prefix, iri)| (prefix.to_owned(), iri.to_owned()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_are_kept_and_taken_over_from_the_rdf4j_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BEFORE), r#"{"ex": "http://example.com/"}"#).unwrap();
        let namespaces = Namespaces::open(Some(dir.path().to_path_buf()));
        assert_eq!(namespaces.get("ex").as_deref(), Some("http://example.com/"));
        namespaces
            .set("foaf", "http://xmlns.com/foaf/0.1/")
            .unwrap();
        assert!(namespaces.remove("ex").unwrap());
        assert!(!namespaces.remove("ex").unwrap());
        let reopened = Namespaces::open(Some(dir.path().to_path_buf()));
        assert_eq!(reopened.all().len(), 1);
        assert!(reopened.get("foaf").is_some());
        assert!(dir.path().join(BEFORE).exists(), "the old file is left");
        // In memory: the standard four.
        assert_eq!(Namespaces::open(None).all().len(), 4);
        assert!(reopened.set("x", " ").is_err());
    }
}
