//! The repository catalogue is the store's (ADR-0007): repositories are created, changed,
//! opened again and removed without a server.

use crate::support::Writes;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::catalog::{Catalog, CatalogError, DEFAULT_REPOSITORY, RepositorySettings};
use nrese_store::{StoreConfig, StoreService};
use std::sync::Arc;

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
    std::fs::create_dir_all(root.join("~trash-old-0")).unwrap();
    std::fs::write(root.join("~trash-old-0").join("file"), b"x").unwrap();
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
    assert!(matches!(
        catalog.delete(DEFAULT_REPOSITORY),
        Err(CatalogError::Invalid(_))
    ));
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
    // It is listed as unavailable, with its file and the repair, and can be removed.
    let why = catalog.unavailable("broken").expect("unavailable");
    assert!(
        why.contains("repository.json") && why.contains("Repair"),
        "{why}"
    );
    catalog.delete("broken").expect("removed");
    assert!(catalog.unavailable_list().is_empty());
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

/// A change of reasoning whose inferences can't be recomputed (here: the process's memory
/// limit) leaves the old reasoning in effect, its write path open, and the old settings.
#[test]
fn a_failed_reconfiguration_keeps_the_old_reasoning() {
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: 1,
        ..StoreConfig::in_memory()
    })
    .expect("store");
    store
        .execute_update_str(
            "INSERT DATA { <http://e/C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://e/D> .
                           <http://e/a> a <http://e/C> }",
        )
        .expect("written");
    let catalog =
        Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue");
    let pipeline = || Arc::clone(&*catalog.default_repository().pipeline.read());
    let before = pipeline().reasoner().mode_name();
    assert_ne!(before, "rdfs");
    let rdfs = RepositorySettings {
        reasoning: Some("rdfs".to_owned()),
        ..RepositorySettings::default()
    };
    assert!(matches!(
        catalog.change(DEFAULT_REPOSITORY, rdfs),
        Err(CatalogError::Store(_))
    ));
    assert_eq!(pipeline().reasoner().mode_name(), before);
    assert!(!pipeline().is_retired(), "writes go on");
    assert_eq!(
        catalog.settings(DEFAULT_REPOSITORY),
        Some(RepositorySettings::default())
    );
}

/// Two creations of one id at once: one is made, the other refused, not both reported
/// made with the second replacing the first.
#[test]
fn concurrent_creations_of_one_id_make_one_repository() {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    let catalog =
        Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue");
    let barrier = std::sync::Barrier::new(2);
    let results: Vec<_> = std::thread::scope(|scope| {
        let create = || {
            barrier.wait();
            catalog.create("twin", RepositorySettings::default())
        };
        let handles = [scope.spawn(create), scope.spawn(create)];
        handles.map(|handle| handle.join().expect("joined")).into()
    });
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Err(CatalogError::Conflict(_)))),
        "{results:?}"
    );
}

/// A removed repository whose files couldn't be deleted (held, on Windows) keeps its
/// tombstone: the next start reclaims it and never opens it again.
#[test]
fn a_removed_repository_stays_removed() {
    let dir = tempfile::tempdir().expect("temp dir");
    open(dir.path())
        .create("gone", RepositorySettings::default())
        .expect("created");
    // What a removal leaves when neither the rename nor the deletion succeeds: its
    // tombstone beside the repositories, the directory as it was.
    let root = dir.path().join("repositories");
    let repository = root.join("gone");
    std::fs::write(root.join("~removed-gone"), b"").expect("tombstone");
    let catalog = open(dir.path());
    assert!(catalog.get("gone").is_none());
    assert!(catalog.unavailable("gone").is_none());
    assert!(!repository.exists(), "reclaimed");
    assert!(
        !root.join("~removed-gone").exists(),
        "its tombstone goes with it"
    );
}

/// Settings that would be silently ignored are refused: unknown fields, an unknown rules
/// format, rules without a reasoning mode.
#[test]
fn settings_that_would_be_ignored_are_refused() {
    assert!(serde_json::from_str::<RepositorySettings>(r#"{"reasonning": "rdfs"}"#).is_err());
    assert!(
        serde_json::from_str::<RepositorySettings>(
            r#"{"reasoning": "custom", "rules": {"name": "r", "format": "n3", "text": "", "mode": "x"}}"#
        )
        .is_err()
    );
    let rules = |format: &str| nrese_store::catalog::RepositoryRules {
        name: "rules".to_owned(),
        format: format.to_owned(),
        text: "@prefix : <urn:> . { ?x :p ?y } => { ?y :q ?x } .".to_owned(),
    };
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    let catalog =
        Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue");
    for (case, settings) in [
        (
            "unknown format",
            RepositorySettings {
                reasoning: Some("custom".to_owned()),
                rules: Some(rules("turtle")),
                ..RepositorySettings::default()
            },
        ),
        (
            "rules without a mode",
            RepositorySettings {
                rules: Some(rules("n3")),
                ..RepositorySettings::default()
            },
        ),
    ] {
        assert!(
            matches!(catalog.create("r", settings), Err(CatalogError::Invalid(_))),
            "{case}"
        );
    }
    // The same rules with a mode are taken.
    catalog
        .create(
            "r",
            RepositorySettings {
                reasoning: Some("custom".to_owned()),
                rules: Some(rules("n3")),
                ..RepositorySettings::default()
            },
        )
        .expect("created");
}

/// A removal whose files are held (Windows: its directory open without delete sharing, so
/// that neither the rename nor the deletion succeeds) stays removed across a restart: the
/// decision is kept outside the directory it reclaims (Sol, r1 of the store lifecycle).
#[cfg(windows)]
#[test]
fn a_removal_whose_files_are_held_stays_removed_after_a_restart() {
    use std::os::windows::fs::OpenOptionsExt;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let dir = tempfile::tempdir().expect("temp dir");
    let catalog = open(dir.path());
    catalog
        .create("held", RepositorySettings::default())
        .expect("created");
    let path = dir.path().join("repositories").join("held");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(BACKUP_SEMANTICS)
        .open(&path)
        .expect("held");
    assert_eq!(
        catalog.delete("held").expect("removed"),
        nrese_store::catalog::Removal::Pending
    );
    assert!(catalog.get("held").is_none());
    drop(held);
    drop(catalog);
    let catalog = open(dir.path());
    assert!(
        catalog.get("held").is_none(),
        "the removed repository came back"
    );
    assert!(catalog.unavailable("held").is_none());
    assert!(!path.exists(), "reclaimed at the start");
}
