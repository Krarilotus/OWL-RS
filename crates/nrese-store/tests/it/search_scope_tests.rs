//! The search scope as a store and repository setting (`store.search_scope`, a
//! repository's `search_scope`; docs/ops/config-reference.md): absent, `global`; a
//! repository inherits the server's and may choose its own, changed at once; an unknown
//! value is refused. Queries, update `WHERE` clauses and autocompletion read it.
//!
//! The observable: literals no statement uses any more. Global relevance counts them (the
//! dictionary keeps every literal it interned), reader relevance doesn't: a store that never
//! had them is the oracle, so `beta` (in one short literal) ranks above `alpha one two`
//! under `reader`, and below it under `global` once sixty `beta` literals came and went.

use crate::support::Writes;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::catalog::{Catalog, CatalogError, RepositorySettings};
use nrese_store::{ReadScope, SearchScope, StoreConfig, StoreService};

const TOP: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                   SELECT ?o WHERE { ?o bds:search \"alpha beta\" ; bds:maxRank 1 }";

/// `alpha` and `beta` once each in the default graph, then sixty more `beta`s that come and
/// go: none is used at the end.
fn fill(store: &StoreService) {
    let mut data = String::from(
        "INSERT DATA { <urn:a> <urn:label> \"alpha one two\" . <urn:b> <urn:label> \"beta\" . ",
    );
    for i in 0..8 {
        data.push_str(&format!("<urn:f{i}> <urn:label> \"filler {i}\" . "));
    }
    data.push('}');
    store.execute_update_str(&data).expect("written");
    let betas: String = (0..60)
        .map(|i| format!("<urn:x{i}> <urn:label> \"beta extra {i}\" . "))
        .collect();
    store
        .execute_update_str(&format!("INSERT DATA {{ {betas} }}"))
        .expect("written");
    store
        .execute_update_str(&format!("DELETE DATA {{ {betas} }}"))
        .expect("deleted");
}

/// The best match of [`TOP`]: `beta` or `alpha one two`.
fn top(store: &StoreService) -> &'static str {
    let result = store.execute_query_str(TOP).expect("answered");
    let payload = String::from_utf8(result.payload).expect("utf-8");
    match (
        payload.contains("alpha one two"),
        payload.contains("\"beta\""),
    ) {
        (true, false) => "alpha one two",
        (false, true) => "beta",
        _ => panic!("one match expected: {payload}"),
    }
}

fn store_of(catalog: &Catalog, id: &str) -> std::sync::Arc<StoreService> {
    catalog
        .get(id)
        .expect("repository")
        .pipeline
        .read()
        .store()
        .clone()
}

fn scoped(scope: Option<&str>) -> RepositorySettings {
    RepositorySettings {
        search_scope: scope.map(str::to_owned),
        ..RepositorySettings::default()
    }
}

/// Each row: the server's setting, the repository's, and the best match its queries get.
/// Then the repository's setting changed in place, and an unknown value refused.
#[test]
fn a_repository_inherits_or_chooses_its_search_scope() {
    let mut failed = Vec::new();
    let rows = [
        (SearchScope::Global, None, "alpha one two"),
        (SearchScope::Global, Some("reader"), "beta"),
        (SearchScope::Reader, None, "beta"),
        (SearchScope::Reader, Some("global"), "alpha one two"),
    ];
    for (server, repository, want) in rows {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = StoreService::new(StoreConfig {
            search_scope: server,
            ..StoreConfig::on_disk(dir.path())
        })
        .expect("store");
        let catalog = Catalog::open(store, ReasonerService::new(ReasonerConfig::default()))
            .expect("catalogue");
        catalog.create("r", scoped(repository)).expect("created");
        let store = store_of(&catalog, "r");
        fill(&store);
        let got = top(&store);
        if got != want {
            failed.push(format!(
                "server {server:?}, repository {repository:?}: {got}"
            ));
        }
    }
    // Changed at once, the cached answers of the other scope not reused.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = StoreService::new(StoreConfig::on_disk(dir.path())).expect("store");
    let catalog =
        Catalog::open(store, ReasonerService::new(ReasonerConfig::default())).expect("catalogue");
    catalog.create("r", scoped(None)).expect("created");
    let store = store_of(&catalog, "r");
    fill(&store);
    for (setting, want) in [
        (Some("reader"), "beta"),
        (Some("global"), "alpha one two"),
        (Some("reader"), "beta"),
        (None, "alpha one two"),
    ] {
        catalog.change("r", scoped(setting)).expect("changed");
        let got = top(&store);
        if got != want {
            failed.push(format!("changed to {setting:?}: {got}"));
        }
    }
    if !matches!(
        catalog.change("r", scoped(Some("sideways"))),
        Err(CatalogError::Invalid(_))
    ) {
        failed.push("an unknown scope is refused".to_owned());
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// An update's `WHERE` clause searches as the store's queries do: its best match under each
/// scope, written down and read back.
#[test]
fn an_update_searches_in_the_stores_scope() {
    let mut failed = Vec::new();
    for (scope, want) in [
        (SearchScope::Global, "alpha one two"),
        (SearchScope::Reader, "beta"),
    ] {
        let store = StoreService::new(StoreConfig {
            search_scope: scope,
            ..StoreConfig::in_memory()
        })
        .expect("store");
        fill(&store);
        store
            .execute_update_str(
                "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                 INSERT { <urn:top> <urn:is> ?o } \
                 WHERE { ?o bds:search \"alpha beta\" ; bds:maxRank 1 }",
            )
            .expect("written");
        let result = store
            .execute_query_str("SELECT ?o WHERE { <urn:top> <urn:is> ?o }")
            .expect("answered");
        let payload = String::from_utf8(result.payload).expect("utf-8");
        if !payload.contains(want) {
            failed.push(format!("{scope:?}: {payload}"));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}

/// Autocompletion scores among the labels and IRIs in use under `reader`: the best visible
/// match has 1; under `global` a literal no statement uses any more (shorter, so better)
/// still sets the scale (guard).
#[test]
fn autocompletion_scores_in_the_stores_scope() {
    let mut failed = Vec::new();
    for (scope, best_is_one) in [(SearchScope::Global, false), (SearchScope::Reader, true)] {
        let store = StoreService::new(StoreConfig {
            search_scope: scope,
            ..StoreConfig::in_memory()
        })
        .expect("store");
        let label = "<http://www.w3.org/2000/01/rdf-schema#label>";
        store
            .execute_update_str(&format!(
                "INSERT DATA {{ <urn:a> {label} \"alpha beta gamma delta\" . \
                                <urn:x> {label} \"alpha\" }}"
            ))
            .expect("written");
        store
            .execute_update_str(&format!("DELETE DATA {{ <urn:x> {label} \"alpha\" }}"))
            .expect("deleted");
        let found = store
            .autocomplete(&ReadScope::All, "alp", 5, true)
            .expect("answered");
        let best = found
            .iter()
            .find(|s| s.iri == "urn:a")
            .map(|s| s.score)
            .unwrap_or_default();
        if (best == 1.0) != best_is_one {
            failed.push(format!("{scope:?}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}
