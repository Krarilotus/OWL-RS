//! A store's namespace prefixes: what clients show and write IRIs with (RDF4J's
//! `/namespaces`, the console, serialisations), one set per store, kept in its data
//! directory as `namespaces.json`.
//!
//! A new store starts with the four standard prefixes. Stores from before this module kept
//! the set as `rdf4j-namespaces.json` (the RDF4J adapter owned it): that file is read when
//! `namespaces.json` doesn't exist yet, and left in place. A file that exists but can't be
//! read or isn't a namespace set is an error (the store doesn't open), not taken as absent:
//! queries would silently resolve prefixes differently.

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
    /// The store's [`crate::StoreService::unavailable`]: set when a change's file is in
    /// place but not made durable.
    unavailable: std::sync::Arc<std::sync::OnceLock<String>>,
    /// Held by a change from its copy of the set to its publication, so that two changes
    /// don't lose one another's; readers take only `map`, never waiting on the disk.
    writer: Mutex<()>,
    /// Where they are kept (stores on disk).
    file: Option<PathBuf>,
}

impl Namespaces {
    /// The namespaces of a store whose data directory is `dir` (`None`: in memory), and
    /// whose unavailability is `unavailable`.
    pub(crate) fn open(
        dir: Option<PathBuf>,
        unavailable: std::sync::Arc<std::sync::OnceLock<String>>,
    ) -> StoreResult<Self> {
        let read = |path: &PathBuf| -> StoreResult<Option<NamespaceMap>> {
            let refused = |error: &dyn std::fmt::Display| {
                StoreError::Configuration(format!(
                    "{}: {error}; the store's namespace prefixes can't be read. Repair or                      remove the file (without it the store starts with {}), then start again",
                    path.display(),
                    match path.ends_with(FILE) {
                        true => format!("those of {BEFORE} if it is there, else the standard four"),
                        false => "the standard four".to_owned(),
                    }
                ))
            };
            match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|error| refused(&error)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(refused(&error)),
            }
        };
        let map = match &dir {
            Some(dir) => match read(&dir.join(FILE))? {
                Some(map) => Some(map),
                None => read(&dir.join(BEFORE))?,
            },
            None => None,
        };
        Ok(Self {
            map: Mutex::new(map.unwrap_or_else(standard)),
            unavailable,
            writer: Mutex::new(()),
            file: dir.map(|dir| dir.join(FILE)),
        })
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

    /// Changes the set and keeps it: the changed copy is written ([`crate::metadata`]),
    /// then published, so that a failed write changes nothing. A write whose file is in
    /// place but not durable publishes it (the set is what the file says) and makes the
    /// store unavailable until a restart, which reads the file again.
    fn change(&self, change: impl FnOnce(&mut NamespaceMap)) -> StoreResult<()> {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = self.all();
        change(&mut next);
        if let Some(path) = &self.file {
            let json = serde_json::to_vec_pretty(&next)
                .map_err(|error| StoreError::Configuration(error.to_string()))?;
            if let Err(error) = crate::metadata::write(path, &json) {
                if error.renamed {
                    *self.map.lock().unwrap_or_else(PoisonError::into_inner) = next;
                    let why = format!(
                        "{}: the namespace prefixes were written but not made durable ({}). Repair: restart the server; it reads the file again",
                        path.display(),
                        error.error
                    );
                    tracing::error!(%why, "store unavailable until the server restarts");
                    let _ = self.unavailable.set(why);
                }
                return Err(error.into());
            }
        }
        *self.map.lock().unwrap_or_else(PoisonError::into_inner) = next;
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
        let namespaces =
            Namespaces::open(Some(dir.path().to_path_buf()), Default::default()).unwrap();
        assert_eq!(namespaces.get("ex").as_deref(), Some("http://example.com/"));
        namespaces
            .set("foaf", "http://xmlns.com/foaf/0.1/")
            .unwrap();
        assert!(namespaces.remove("ex").unwrap());
        assert!(!namespaces.remove("ex").unwrap());
        let reopened =
            Namespaces::open(Some(dir.path().to_path_buf()), Default::default()).unwrap();
        assert_eq!(reopened.all().len(), 1);
        assert!(reopened.get("foaf").is_some());
        assert!(dir.path().join(BEFORE).exists(), "the old file is left");
        // In memory: the standard four.
        assert_eq!(
            Namespaces::open(None, Default::default())
                .unwrap()
                .all()
                .len(),
            4
        );
        assert!(reopened.set("x", " ").is_err());
    }

    /// A current file that doesn't parse is an error, not absence: neither the old file
    /// nor the standard prefixes take its place.
    #[test]
    fn a_corrupt_namespace_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BEFORE), r#"{"ex": "http://old.example/"}"#).unwrap();
        std::fs::write(dir.path().join(FILE), b"{\"ex\": ").unwrap();
        let error =
            Namespaces::open(Some(dir.path().to_path_buf()), Default::default()).unwrap_err();
        assert!(error.to_string().contains(FILE), "{error}");
    }

    /// A change whose file can't be written leaves the live set as it was.
    #[test]
    fn a_failed_change_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let namespaces =
            Namespaces::open(Some(dir.path().to_path_buf()), Default::default()).unwrap();
        namespaces.set("ex", "http://example.com/").unwrap();
        // The staging name taken by a directory: the write fails.
        std::fs::create_dir(dir.path().join(format!("{FILE}.tmp"))).unwrap();
        assert!(namespaces.set("ex", "http://other.example/").is_err());
        assert_eq!(namespaces.get("ex").as_deref(), Some("http://example.com/"));
    }

    /// A change whose file is in place but not durable (the directory sync after the
    /// rename failed) is what the store serves, and the store is unavailable until a
    /// restart: the error doesn't mean "as before".
    #[test]
    fn a_change_written_but_not_durable_makes_the_store_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let unavailable = std::sync::Arc::<std::sync::OnceLock<String>>::default();
        let namespaces = Namespaces::open(
            Some(dir.path().to_path_buf()),
            std::sync::Arc::clone(&unavailable),
        )
        .unwrap();
        crate::metadata::tests::FAIL_DIR_SYNC.set(true);
        let result = namespaces.set("ex", "http://example.com/");
        crate::metadata::tests::FAIL_DIR_SYNC.set(false);
        assert!(result.is_err());
        assert_eq!(
            namespaces.get("ex").as_deref(),
            Some("http://example.com/"),
            "as on disk"
        );
        let why = unavailable.get().expect("unavailable");
        assert!(why.contains(FILE) && why.contains("restart"), "{why}");
    }
}
