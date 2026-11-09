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

/// A store holding `state` as an access state from before ids (records without one), with
/// the one history record that made it, as an upgraded store holds them.
fn seeded(state: &AccessState) -> StoreService {
    let quads = encoded(state);
    let record = seed_record(1, &quads);
    with_statements(&quads, &[record])
}

/// `state`'s statements in the state graph.
fn encoded(state: &AccessState) -> Vec<nrese_rdf::Quad> {
    let state_graph = nrese_rdf::NamedNode::new_unchecked(STATE_GRAPH);
    crate::access::rdf::encode(state, &state_graph)
}

/// History record `number`, adding `quads`.
fn seed_record(number: u64, quads: &[nrese_rdf::Quad]) -> ChangeRecord {
    ChangeRecord {
        number,
        author: "admin".to_owned(),
        time: "2026-10-01T00:00:00Z".to_owned(),
        reason: "before ids".to_owned(),
        summary: "seeded".to_owned(),
        added: quads.iter().map(crate::access::control::line).collect(),
        removed: Vec::new(),
    }
}

/// A store holding `quads` in the state graph and `records` in the history graph.
fn with_statements(quads: &[nrese_rdf::Quad], records: &[ChangeRecord]) -> StoreService {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut all = quads.to_vec();
    for record in records {
        all.extend(crate::access::control::record_quads(record));
    }
    let insert = format!("INSERT DATA {{\n{}\n}}", crate::access::control::data(&all));
    store
        .update_ungated(&crate::SparqlUpdateRequest::new(insert))
        .unwrap();
    store
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
    let mut legacy = AccessState::new("urn:nrese:");
    legacy.apply(&admin(), Change::Import(policy())).unwrap();
    legacy.apply(&alice, workspace("team")).unwrap();
    let store = seeded(&legacy);
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

/// C17 (package review R1-D1; red without its fix) and C18 (guard): a local user from before
/// ids, removed, created again without a password and given one in a second change, is a
/// new person and inherits nothing; an unchanged account from before ids, its password
/// reset, keeps its space. Both also after reopen. Specified (C-204 (b)): a reused name
/// starts empty; a missing password isn't evidence of a continuing identity.
#[test]
fn a_local_user_recreated_in_two_steps_starts_empty() {
    let (carol, erin) = (user("carol"), user("erin"));
    let mut legacy = AccessState::new("urn:nrese:");
    legacy.apply(&admin(), Change::Import(policy())).unwrap();
    for change in [
        local_user("carol", "the first carol's secret"),
        local_user("erin", "erin's long secret"),
    ] {
        legacy.apply(&admin(), change).unwrap();
    }
    let store = seeded(&legacy);
    let control = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    let old = keep(&control, &carol, "~carol");
    let kept = keep(&control, &erin, "~erin");
    let record = Change::PutUser {
        name: "carol".to_owned(),
        admin: None,
        roles: Some(BTreeSet::from(["analyst".to_owned()])),
        password_hash: None,
    };
    for (change, reason) in [
        (Change::RemoveUser("carol".to_owned()), "left"),
        (record, "hired, her login to follow"),
        (
            local_user("carol", "the second carol's secret"),
            "her login",
        ),
        (local_user("erin", "erin's new long secret"), "reset"),
    ] {
        control.apply(&admin(), change, reason).unwrap();
    }
    let reopened = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    let mut wrong = Vec::new();
    for (when, control) in [("now", control), ("reopened", reopened)] {
        for found in inherited(&control, &carol, "~carol", &old) {
            wrong.push(format!("C17 {when}: the second carol {found}"));
        }
        let keeps = control.view(&erin, "nrese").can_write(&named(&kept))
            && control.saved_query(&erin, "~erin", "kept").is_ok();
        if !keeps {
            wrong.push(format!("C18 guard {when}: erin lost her own space"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Whether `who` still writes `graph` and reads its saved query `kept` in `space`.
fn keeps(control: &AccessControl, who: &Principal, space: &str, graph: &str) -> bool {
    control.view(who, "nrese").can_write(&named(graph))
        && control.saved_query(who, space, "kept").is_ok()
}

/// A user's id is fixed when its record is created; nothing later moves its space
/// (fix check r1, R2-L1; design note revision r2, its event table). Specified: the same
/// person keeps its personal space through a password added and removed, and through a
/// role record made for a token user; also after reopen. Red without the fix: R2-L1
/// (dave) and R2-L1b (erin). The collision row is documented behaviour (master, 9 Nov
/// 00:38): an administrator creating a local account under a token user's name gives the
/// name a fresh space.
#[test]
fn a_user_s_own_space_never_moves() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let control = enforcing(&store);
    let (dave, erin, frank) = (user("dave"), user("erin"), user("frank"));
    let role = |name: &str| Change::PutUser {
        name: name.to_owned(),
        admin: None,
        roles: Some(BTreeSet::from(["analyst".to_owned()])),
        password_hash: None,
    };
    let no_password = Change::PutUser {
        name: "dave".to_owned(),
        admin: None,
        roles: None,
        password_hash: Some(None),
    };
    let mut wrong = Vec::new();
    // R2-L1: an external user's role record, then a password, then none.
    control.apply(&admin(), role("dave"), "an analyst").unwrap();
    let dave_graph = keep(&control, &dave, "~dave");
    for (change, step) in [
        (local_user("dave", "dave's long secret"), "password added"),
        (no_password, "password removed"),
    ] {
        control.apply(&admin(), change, step).unwrap();
        if !keeps(&control, &dave, "~dave", &dave_graph) {
            wrong.push(format!("R2-L1 {step}: dave lost his own space"));
        }
    }
    // R2-L1b: a token user's content before any record; then a role record and a password.
    let erin_graph = keep(&control, &erin, "~erin");
    for (change, step) in [
        (role("erin"), "role record created"),
        (local_user("erin", "erin's long secret"), "password added"),
    ] {
        control.apply(&admin(), change, step).unwrap();
        if !keeps(&control, &erin, "~erin", &erin_graph) {
            wrong.push(format!("R2-L1b {step}: erin lost her own space"));
        }
    }
    // Documented collision: a local account created under a token user's name.
    let frank_graph = keep(&control, &frank, "~frank");
    let account = local_user("frank", "a new local frank's secret");
    control.apply(&admin(), account, "a local account").unwrap();
    for found in inherited(&control, &frank, "~frank", &frank_graph) {
        wrong.push(format!("collision guard: the local account frank {found}"));
    }
    let reopened = AccessControl::open(store.clone(), "urn:nrese:").unwrap();
    for (who, graph, space) in [(&dave, &dave_graph, "~dave"), (&erin, &erin_graph, "~erin")] {
        if !keeps(&reopened, who, space, graph) {
            wrong.push(format!("reopened: {space} lost its own space"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The history decides identities (design note revision r2), so an access state whose
/// history can't answer "did this name have a local login" refuses to open rather than
/// guess (master, 9 Nov 00:38): H1 a state without history, H2 a gap, H4 an unreadable
/// record, and a current local login the history never recorded. Control: the complete
/// one opens. Red without the fix (each opens).
#[test]
fn an_incomplete_history_refuses_to_open() {
    let mut state = AccessState::new("urn:nrese:");
    state.apply(&admin(), Change::Import(policy())).unwrap();
    state
        .apply(&admin(), local_user("carol", "carol's long secret"))
        .unwrap();
    let quads = encoded(&state);
    let complete = seed_record(1, &quads);
    let without_password_line = ChangeRecord {
        added: complete
            .added
            .iter()
            .filter(|line| !line.contains("passwordHash"))
            .cloned()
            .collect(),
        ..complete.clone()
    };
    let later = ChangeRecord {
        number: 3,
        added: Vec::new(),
        ..complete.clone()
    };
    let unreadable = with_statements(&quads, std::slice::from_ref(&complete));
    let stray = format!(
        "INSERT DATA {{ GRAPH <{HISTORY_GRAPH}> {{ <urn:nrese:change:x> <{}summary> \"?\" }} }}",
        crate::access::rdf::NS
    );
    unreadable
        .update_ungated(&crate::SparqlUpdateRequest::new(stray))
        .unwrap();
    let mut wrong = Vec::new();
    for (row, store) in [
        ("H1 no history", with_statements(&quads, &[])),
        (
            "H2 a gap",
            with_statements(&quads, &[complete.clone(), later]),
        ),
        ("H4 an unreadable record", unreadable),
        (
            "a local login the history never recorded",
            with_statements(&quads, &[without_password_line]),
        ),
    ] {
        match AccessControl::open(store, "urn:nrese:") {
            Err(AccessError::Store(message)) if message.contains("history") => {}
            Err(other) => wrong.push(format!("{row}: refused as {other:?}")),
            Ok(_) => wrong.push(format!("{row}: opened")),
        }
    }
    if let Err(error) = AccessControl::open(with_statements(&quads, &[complete]), "urn:nrese:") {
        wrong.push(format!("control: the complete history refused: {error:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
