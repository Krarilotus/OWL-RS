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
    drop(store);
    drop(catalog);
    // Nothing left under the root, and a removal a start didn't see finish (a renamed
    // directory whose files were still held) is swept at the next one.
    let root = dir.path().join("repositories");
    let left = |root: &std::path::Path| {
        std::fs::read_dir(root)
            .map(|entries| entries.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    std::fs::create_dir_all(root.join(".trash-old-0")).unwrap();
    std::fs::write(root.join(".trash-old-0").join("file"), b"x").unwrap();
    let catalog = open(dir.path());
    assert!(catalog.list().is_empty());
    assert!(left(&root).is_empty(), "{:?}", left(&root));
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
    // Ids differing only in case are one directory on Windows and macOS.
    assert!(matches!(
        catalog.create(
            &DEFAULT_REPOSITORY.to_uppercase(),
            RepositorySettings::default()
        ),
        Err(CatalogError::Conflict(_))
    ));
    catalog
        .create("bench", RepositorySettings::default())
        .expect("created");
    assert!(matches!(
        catalog.create("Bench", RepositorySettings::default()),
        Err(CatalogError::Conflict(_))
    ));
    assert!(invalid(
        catalog.create("con", RepositorySettings::default())
    ));
    assert!(matches!(
        catalog.change("nowhere", RepositorySettings::default()),
        Err(CatalogError::NotFound(_))
    ));
}

#[test]
fn a_repository_keeps_its_own_query_timeout() {
    let dir = tempfile::tempdir().expect("temp dir");
    let timed = |ms| RepositorySettings {
        query_timeout_ms: Some(ms),
        ..RepositorySettings::default()
    };
    {
        let catalog = open(dir.path());
        for refused in [0, nrese_store::catalog::MAX_QUERY_TIMEOUT_MS + 1] {
            assert!(matches!(
                catalog.create("public", timed(refused)),
                Err(CatalogError::Invalid(_))
            ));
        }
        catalog.create("public", timed(10_000)).expect("created");
        let settings = catalog.settings("public").expect("settings");
        assert_eq!(
            settings.query_timeout(),
            Some(std::time::Duration::from_secs(10))
        );
    }
    // Opened again from its settings file.
    let catalog = open(dir.path());
    assert_eq!(
        catalog
            .settings("public")
            .expect("reopened")
            .query_timeout_ms,
        Some(10_000)
    );
    // A hand-written file with a timeout the API refuses doesn't open.
    let file = dir.path().join("repositories").join("broken");
    std::fs::create_dir_all(&file).expect("dir");
    std::fs::write(file.join("repository.json"), r#"{"query_timeout_ms": 0}"#).expect("file");
    drop(catalog);
    let catalog = open(dir.path());
    assert!(catalog.settings("broken").is_none());
    assert!(catalog.settings("public").is_some());
}

/// A repository chooses when its queries get OWL 2 QL answers through existentials
/// (`ql_rewriting`), at once: `auto` (the server's default here) leaves `owl2-rl` with its
/// standard semantics, `on` adds them.
#[test]
fn a_repository_switches_the_ql_rewriting() {
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = open(dir.path());
    let rl = |ql: Option<&str>| RepositorySettings {
        reasoning: Some("owl2-rl".to_owned()),
        ql_rewriting: ql.map(str::to_owned),
        ..RepositorySettings::default()
    };
    catalog.create("rl", rl(None)).expect("created");
    let store = catalog
        .get("rl")
        .expect("rl")
        .pipeline
        .read()
        .store()
        .clone();
    store
        .execute_update_str(
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA {
                <urn:Employee> rdfs:subClassOf [ a owl:Restriction ;
                    owl:onProperty <urn:worksFor> ; owl:someValuesFrom owl:Thing ] .
                <urn:bob> a <urn:Employee> .
             }",
        )
        .expect("written");
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .expect("materialised");
    let employed = || {
        let result = store
            .execute_query_str("SELECT ?x WHERE { ?x <urn:worksFor> ?y }")
            .expect("answered");
        String::from_utf8(result.payload)
            .expect("utf-8")
            .contains("urn:bob")
    };
    assert!(!employed(), "auto: owl2-rl's own semantics");
    catalog.change("rl", rl(Some("on"))).expect("changed");
    assert!(employed(), "on: through the existential");
    catalog.change("rl", rl(Some("off"))).expect("changed");
    assert!(!employed(), "off");
    assert!(matches!(
        catalog.change("rl", rl(Some("sometimes"))),
        Err(CatalogError::Invalid(_))
    ));
}
