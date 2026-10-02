//! The repository catalogue is the store's (ADR-0007): repositories are created, changed,
//! opened again and removed without a server.

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::catalog::{Catalog, CatalogError, DEFAULT_REPOSITORY, RepositorySettings};
use nrese_store::{StoreConfig, StoreService};

fn open(dir: &std::path::Path) -> Catalog {
    let store = StoreService::new(StoreConfig::on_disk(dir)).expect("store");
    Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue")
}

fn titled(title: &str) -> RepositorySettings {
    RepositorySettings {
        title: Some(title.to_owned()),
        ..RepositorySettings::default()
    }
}

#[test]
fn repositories_outlive_the_catalogue_that_made_them() {
    let dir = tempfile::tempdir().expect("temp dir");
    {
        let catalog = open(dir.path());
        catalog
            .create("bench", titled("Benchmarks"))
            .expect("created");
        catalog
            .change(
                "bench",
                RepositorySettings {
                    reasoning: Some("rdfs".to_owned()),
                    ..titled("Benchmarks, with RDFS")
                },
            )
            .expect("changed");
        catalog
            .change(DEFAULT_REPOSITORY, titled("The default"))
            .expect("default changed");
        let store = catalog
            .get("bench")
            .expect("bench")
            .pipeline
            .read()
            .store()
            .clone();
        store
            .execute_update_str("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
            .expect("written");
    }
    // Opened again: the repositories, their settings and data are back.
    let catalog = open(dir.path());
    assert_eq!(
        catalog.list(),
        [("bench".to_owned(), Some("Benchmarks, with RDFS".to_owned()))]
    );
    assert_eq!(
        catalog.settings("bench").and_then(|s| s.reasoning),
        Some("rdfs".to_owned())
    );
    assert_eq!(
        catalog.settings(DEFAULT_REPOSITORY).and_then(|s| s.title),
        Some("The default".to_owned())
    );
    let store = catalog
        .get("bench")
        .expect("bench")
        .pipeline
        .read()
        .store()
        .clone();
    let answer = store
        .execute_query_str("ASK { <urn:a> <urn:p> <urn:b> }")
        .expect("query");
    assert!(String::from_utf8(answer.payload).unwrap().contains("true"));

    catalog.delete("bench").expect("removed");
    assert!(catalog.get("bench").is_none());
    assert!(catalog.list().is_empty());
}

#[test]
fn the_catalogue_refuses_what_it_cant_do() {
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = open(dir.path());
    let invalid =
        |result: Result<(), CatalogError>| matches!(result, Err(CatalogError::Invalid(_)));
    assert!(invalid(
        catalog.create("a/b", RepositorySettings::default())
    ));
    assert!(invalid(catalog.create(
        "x",
        RepositorySettings {
            reasoning: Some("telepathy".to_owned()),
            ..RepositorySettings::default()
        }
    )));
    assert!(invalid(catalog.delete(DEFAULT_REPOSITORY)));
    assert!(matches!(
        catalog.create(DEFAULT_REPOSITORY, RepositorySettings::default()),
        Err(CatalogError::Conflict(_))
    ));
    assert!(matches!(
        catalog.change("nowhere", RepositorySettings::default()),
        Err(CatalogError::NotFound(_))
    ));
}
