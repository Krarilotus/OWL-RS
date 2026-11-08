//! Content is bound to the incarnation of a space that made it (SYN-64; owner decision
//! C-204 (b), 26 Oct: "Content is bound to an internal id, so a reused name starts empty";
//! design note `2026-11-08-p2-b5-syn64-design.md`, attacks C1–C7): a workspace or a local
//! user removed and created again under its name inherits none of the old one's graphs or
//! saved queries, and spaces from before ids keep what they have.

use super::*;

fn workspace(name: &str) -> Change {
    Change::PutWorkspace {
        name: name.to_owned(),
        title: None,
        repository: None,
        graphs: None,
    }
}

fn local_user(name: &str, password: &str) -> Change {
    Change::PutUser {
        name: name.to_owned(),
        admin: None,
        roles: None,
        password_hash: Some(Some(hash_password(password).unwrap())),
    }
}

/// An enforcing access state in `store`.
fn enforcing(store: &StoreService) -> AccessControl {
    let control = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    control
        .apply(&admin(), Change::Import(policy()), "enforced")
        .unwrap();
    control
}

/// `who`, a member of `space`, keeps a graph and a saved query (`kept`) there: the graph's
/// IRI.
fn keep(control: &AccessControl, who: &Principal, space: &str) -> String {
    let graph = format!("{}data", control.state().prefix(space));
    assert!(
        control.view(who, "nrese").can_write(&named(&graph)),
        "{space}: its member writes {graph}"
    );
    let draft = QueryDraft {
        query: "ASK {}".to_owned(),
        ..QueryDraft::default()
    };
    let namespaces = crate::NamespaceMap::new();
    control
        .save_query(who, space, "kept", draft, &namespaces)
        .unwrap();
    graph
}

/// What `who`, of a new incarnation of `space`, inherits of the old one's `graph` and its
/// saved query `kept`.
fn inherited(control: &AccessControl, who: &Principal, space: &str, graph: &str) -> Vec<String> {
    let mut found = Vec::new();
    if control.view(who, "nrese").can_read(&named(graph)) {
        found.push(format!("reads {graph}"));
    }
    if control.saved_query(who, space, "kept").is_ok() {
        found.push(format!("sees the saved query {space}/kept"));
    }
    if control
        .saved_queries(who)
        .iter()
        .any(|query| query.space == space)
    {
        found.push(format!("lists a saved query of {space}"));
    }
    found
}

/// C1, C2, C4, C6 (each red without its fix) and C3, C7 (guards). Specified (C-204 (b)):
/// the new incarnation inherits nothing; spaces from before ids keep their prefixes.
#[test]
fn a_reused_name_starts_empty() {
    let mut wrong = Vec::new();
    let (alice, bob) = (user("alice"), user("bob"));

    // C1: Alice's workspace, removed; Bob creates one of that name.
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let control = enforcing(&store);
    control
        .apply(&alice, workspace("project"), "start")
        .unwrap();
    let graph = keep(&control, &alice, "project");
    let removed = Change::RemoveWorkspace("project".to_owned());
    control.apply(&alice, removed, "done").unwrap();
    control.apply(&bob, workspace("project"), "mine").unwrap();
    for found in inherited(&control, &bob, "project", &graph) {
        wrong.push(format!("C1 recreated workspace: {found}"));
    }

    // C2 and C3: a workspace from before ids (its record without one, as in an upgraded
    // store) keeps its prefix, also reopened; removed and created again, it starts empty.
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut legacy = AccessState::new("urn:nrese:");
    legacy.apply(&admin(), Change::Import(policy())).unwrap();
    legacy.apply(&alice, workspace("team")).unwrap();
    let state_graph = nrese_rdf::NamedNode::new_unchecked(STATE_GRAPH);
    let quads = crate::access::rdf::encode(&legacy, &state_graph);
    let insert = format!(
        "INSERT DATA {{\n{}\n}}",
        crate::access::control::data(&quads)
    );
    store
        .update_ungated(&crate::SparqlUpdateRequest::new(insert))
        .unwrap();
    let control = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    let before_ids = "urn:nrese:workspace/team/";
    if control.state().prefix("team") != before_ids {
        wrong.push(format!(
            "C3 guard: the workspace from before ids moved to {}",
            control.state().prefix("team")
        ));
    }
    let graph = keep(&control, &alice, "team");
    let control = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    if control.state().prefix("team") != before_ids {
        wrong.push("C3 guard: reopened, the workspace from before ids moved".to_owned());
    }
    let removed = Change::RemoveWorkspace("team".to_owned());
    control.apply(&alice, removed, "done").unwrap();
    control.apply(&bob, workspace("team"), "mine").unwrap();
    for found in inherited(&control, &bob, "team", &graph) {
        wrong.push(format!("C2 recreated workspace from before ids: {found}"));
    }

    // C4: removed, the service restarted, then created again.
    let dir = tempfile::tempdir().unwrap();
    let open = || StoreService::new(StoreConfig::on_disk(dir.path())).unwrap();
    let graph = {
        let control = enforcing(&open());
        control.apply(&alice, workspace("lab"), "start").unwrap();
        let graph = keep(&control, &alice, "lab");
        let removed = Change::RemoveWorkspace("lab".to_owned());
        control.apply(&alice, removed, "done").unwrap();
        graph
    };
    let control = AccessControl::open(open(), "urn:nrese:").unwrap();
    control.apply(&bob, workspace("lab"), "mine").unwrap();
    for found in inherited(&control, &bob, "lab", &graph) {
        wrong.push(format!("C4 recreated after a restart: {found}"));
    }

    // C6: a local user removed and created again; C7 (guard): a user without a local
    // login keeps its personal space when a record is made for it.
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let control = enforcing(&store);
    control
        .apply(
            &admin(),
            local_user("carol", "the first carol's secret"),
            "hired",
        )
        .unwrap();
    let graph = keep(&control, &user("carol"), "~carol");
    let removed = Change::RemoveUser("carol".to_owned());
    control.apply(&admin(), removed, "left").unwrap();
    let again = local_user("carol", "the second carol's secret");
    control.apply(&admin(), again, "hired").unwrap();
    for found in inherited(&control, &user("carol"), "~carol", &graph) {
        wrong.push(format!("C6 recreated local user: {found}"));
    }
    let token_user = control.state().prefix("~dave");
    let record = Change::PutUser {
        name: "dave".to_owned(),
        admin: None,
        roles: Some(BTreeSet::from(["analyst".to_owned()])),
        password_hash: None,
    };
    control.apply(&admin(), record, "an analyst").unwrap();
    if token_user != "urn:nrese:space/dave/" || control.state().prefix("~dave") != token_user {
        wrong.push(format!(
            "C7 guard: a token user's space moved: {token_user} -> {}",
            control.state().prefix("~dave")
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
