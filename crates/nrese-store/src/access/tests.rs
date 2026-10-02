use std::collections::BTreeSet;

use nrese_rdf::{GraphName, NamedNode};

use super::*;
use crate::{StoreConfig, StoreService};

fn named(iri: &str) -> GraphName {
    GraphName::NamedNode(NamedNode::new_unchecked(iri))
}

fn roles(names: &[&str]) -> Principal {
    Principal {
        user: None,
        roles: names.iter().map(|r| (*r).to_owned()).collect(),
        admin: false,
    }
}

fn user(name: &str) -> Principal {
    Principal {
        user: Some(name.to_owned()),
        ..Principal::default()
    }
}

fn admin() -> Principal {
    Principal {
        admin: true,
        ..Principal::default()
    }
}

fn rule(name: &str, read: &[&str], write: &[&str], default_graph: DefaultGraphRight) -> RoleRule {
    RoleRule {
        name: name.to_owned(),
        read: read.iter().map(|s| (*s).to_owned()).collect(),
        write: write.iter().map(|s| (*s).to_owned()).collect(),
        deny: Vec::new(),
        default_graph,
    }
}

/// The policy of the server's access tests: an analyst and an editor.
fn policy() -> AccessPolicy {
    AccessPolicy {
        default: Fallback::Deny,
        inferred: Inferred::Hidden,
        roles: vec![
            rule(
                "analyst",
                &["https://kg.example/public/*", "https://kg.example/sales"],
                &["https://kg.example/analyst/*"],
                DefaultGraphRight::Read,
            ),
            rule(
                "editor",
                &[],
                &["https://kg.example/public/*"],
                DefaultGraphRight::None,
            ),
        ],
    }
}

fn enforced() -> AccessState {
    let mut state = AccessState::new("urn:nrese:");
    state.apply(&admin(), Change::Import(policy())).unwrap();
    state
}

#[test]
fn without_enforcement_everyone_is_unrestricted() {
    let state = AccessState::new("urn:nrese:");
    assert!(state.view(&roles(&["guest"]), "nrese").reads_everything());
    assert!(!state.grants_read(&user("alice")));
    assert!(!state.grants_write(&user("alice")));
}

#[test]
fn roles_union_their_rules_and_a_deny_wins() {
    let mut state = enforced();
    let analyst = state.view(&roles(&["analyst"]), "nrese");
    assert!(analyst.can_read(&named("https://kg.example/public/x")));
    assert!(analyst.can_read(&named("https://kg.example/sales")));
    assert!(!analyst.can_read(&named("https://kg.example/sales/2")));
    assert!(analyst.can_read(&named("https://kg.example/analyst/notes")));
    assert!(analyst.can_read(&GraphName::DefaultGraph));
    assert!(!analyst.can_write(&GraphName::DefaultGraph));
    assert!(!analyst.can_write(&named("https://kg.example/public/x")));
    assert!(!analyst.sees_inferred());
    let both = state.view(&roles(&["analyst", "editor"]), "nrese");
    assert!(both.can_write(&named("https://kg.example/public/x")));
    let stranger = state.view(&roles(&["guest"]), "nrese");
    assert!(!stranger.can_read(&GraphName::DefaultGraph));
    assert!(state.view(&admin(), "nrese").reads_everything());

    let mut contractor = rule("contractor", &[], &[], DefaultGraphRight::Deny);
    contractor.deny = vec!["https://kg.example/public/hr/*".to_owned()];
    state.apply(&admin(), Change::PutRole(contractor)).unwrap();
    let view = state.view(&roles(&["analyst", "editor", "contractor"]), "nrese");
    assert!(view.can_read(&named("https://kg.example/public/x")));
    assert!(!view.can_read(&named("https://kg.example/public/hr/salaries")));
    assert!(!view.can_write(&named("https://kg.example/public/hr/salaries")));
    assert!(!view.can_read(&GraphName::DefaultGraph));

    let mut settings = state.settings.clone();
    settings.fallback = Fallback::Allow;
    settings.inferred = Inferred::Visible;
    state.apply(&admin(), Change::Settings(settings)).unwrap();
    assert!(state.view(&roles(&["guest"]), "nrese").reads_everything());
    assert!(state.view(&roles(&["analyst"]), "nrese").sees_inferred());
}

#[test]
fn every_user_has_a_personal_space_only_it_writes() {
    let mut state = enforced();
    let alice_space = state.prefix("~alice");
    assert_eq!(alice_space, "urn:nrese:space/alice/");
    let draft = named(&format!("{alice_space}drafts"));
    let alice = state.view(&user("alice"), "nrese");
    assert!(alice.can_write(&draft));
    assert!(alice.can_read(&draft));
    assert!(!alice.can_read(&GraphName::DefaultGraph));
    let bob = state.view(&user("bob"), "nrese");
    assert!(!bob.can_read(&draft));
    // A named user may read and write (its personal space at least).
    assert!(state.grants_read(&user("bob")) && state.grants_write(&user("bob")));
    assert!(!state.grants_write(&roles(&["guest"])));

    // Shared with viewers only, by its user alone.
    let share = |level| Change::SetMember {
        workspace: "~alice".to_owned(),
        user: "bob".to_owned(),
        level: Some(level),
    };
    assert!(matches!(
        state.apply(&user("bob"), share(Level::Viewer)),
        Err(AccessError::Forbidden(_))
    ));
    assert!(matches!(
        state.apply(&user("alice"), share(Level::Editor)),
        Err(AccessError::Invalid(_))
    ));
    state.apply(&user("alice"), share(Level::Viewer)).unwrap();
    let bob = state.view(&user("bob"), "nrese");
    assert!(bob.can_read(&draft));
    assert!(!bob.can_write(&draft));
    // Names are encoded in prefixes.
    assert_eq!(state.prefix("~a|b"), "urn:nrese:space/a%7Cb/");
}

#[test]
fn workspaces_have_owners_editors_and_viewers() {
    let mut state = enforced();
    let create = Change::PutWorkspace {
        name: "project".to_owned(),
        title: Some(Some("Project".to_owned())),
        repository: None,
        graphs: None,
    };
    state.apply(&user("alice"), create.clone()).unwrap();
    assert_eq!(
        state.workspaces["project"].members.get("alice"),
        Some(&Level::Owner)
    );
    // Someone else can't change it, nor take graphs in without being an administrator.
    assert!(state.apply(&user("bob"), create).is_err());
    let take_in = Change::PutWorkspace {
        name: "project".to_owned(),
        title: None,
        repository: None,
        graphs: Some(vec!["https://kg.example/secret/*".to_owned()]),
    };
    assert!(matches!(
        state.apply(&user("alice"), take_in.clone()),
        Err(AccessError::Forbidden(_))
    ));
    state.apply(&admin(), take_in).unwrap();
    let member = |name: &str, level| Change::SetMember {
        workspace: "project".to_owned(),
        user: name.to_owned(),
        level,
    };
    state
        .apply(&user("alice"), member("bob", Some(Level::Editor)))
        .unwrap();
    state
        .apply(&user("alice"), member("carol", Some(Level::Viewer)))
        .unwrap();
    assert!(
        state
            .apply(&user("bob"), member("dave", Some(Level::Viewer)))
            .is_err()
    );
    let graph = named(&format!("{}data", state.prefix("project")));
    let secret = named("https://kg.example/secret/x");
    let bob = state.view(&user("bob"), "nrese");
    assert!(bob.can_write(&graph) && bob.can_write(&secret));
    let carol = state.view(&user("carol"), "nrese");
    assert!(carol.can_read(&graph) && !carol.can_write(&graph));
    assert!(!state.view(&user("dave"), "nrese").can_read(&graph));
    // The last owner can't leave.
    assert!(matches!(
        state.apply(&user("alice"), member("alice", None)),
        Err(AccessError::Conflict(_))
    ));
    assert_eq!(
        state.workspaces["project"].members.get("alice"),
        Some(&Level::Owner)
    );
    // A workspace in one repository only.
    state
        .apply(
            &admin(),
            Change::PutWorkspace {
                name: "project".to_owned(),
                title: None,
                repository: Some(Some("other".to_owned())),
                graphs: None,
            },
        )
        .unwrap();
    assert!(!state.view(&user("bob"), "nrese").can_read(&graph));
    assert!(state.view(&user("bob"), "other").can_read(&graph));
    // A deny in a role still wins over a membership.
    let mut deny = rule("no-secrets", &[], &[], DefaultGraphRight::None);
    deny.deny = vec!["https://kg.example/secret/*".to_owned()];
    state.apply(&admin(), Change::PutRole(deny)).unwrap();
    state
        .apply(
            &admin(),
            Change::PutUser {
                name: "bob".to_owned(),
                admin: None,
                roles: Some(BTreeSet::from(["no-secrets".to_owned()])),
                password_hash: None,
            },
        )
        .unwrap();
    assert!(!state.view(&user("bob"), "other").can_read(&secret));
}

#[test]
fn users_change_their_own_password_only() {
    let mut state = enforced();
    let password = |name: &str| Change::PutUser {
        name: name.to_owned(),
        admin: None,
        roles: None,
        password_hash: Some(Some("$argon2id$hash".to_owned())),
    };
    state.apply(&user("alice"), password("alice")).unwrap();
    assert!(state.apply(&user("alice"), password("bob")).is_err());
    let promote = Change::PutUser {
        name: "alice".to_owned(),
        admin: Some(true),
        roles: None,
        password_hash: None,
    };
    assert!(state.apply(&user("alice"), promote.clone()).is_err());
    state.apply(&admin(), promote).unwrap();
    assert!(state.is_admin(&user("alice")));
    assert!(state.view(&user("alice"), "nrese").reads_everything());
}

#[test]
fn the_state_round_trips_through_rdf() {
    let mut state = enforced();
    state
        .apply(
            &user("alice"),
            Change::PutWorkspace {
                name: "project".to_owned(),
                title: Some(Some("A \"project\"\nwith lines".to_owned())),
                repository: Some(Some("nrese".to_owned())),
                graphs: None,
            },
        )
        .unwrap();
    for (name, level) in [("bob", Level::Editor), ("c|d", Level::Viewer)] {
        state
            .apply(
                &user("alice"),
                Change::SetMember {
                    workspace: "project".to_owned(),
                    user: name.to_owned(),
                    level: Some(level),
                },
            )
            .unwrap();
    }
    state
        .apply(
            &admin(),
            Change::PutUser {
                name: "alice".to_owned(),
                admin: Some(false),
                roles: Some(BTreeSet::from(["analyst".to_owned()])),
                password_hash: Some(Some("$argon2id$v=19$x".to_owned())),
            },
        )
        .unwrap();
    let graph = rdf::iri(STATE_GRAPH);
    let decoded = rdf::decode(&rdf::encode(&state, &graph), "urn:nrese:");
    assert_eq!(decoded, state);
}

#[test]
fn changes_are_kept_with_their_history_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        let store = StoreService::new(StoreConfig::on_disk(dir.path())).unwrap();
        AccessControl::open(store, "urn:nrese:").unwrap()
    };
    {
        let control = open();
        assert!(control.is_new());
        assert!(matches!(
            control.apply(&admin(), Change::Import(policy()), "  "),
            Err(AccessError::Invalid(_))
        ));
        control
            .apply(
                &admin(),
                Change::Import(policy()),
                "the policy file at start",
            )
            .unwrap();
        control
            .apply(
                &user("alice"),
                Change::PutWorkspace {
                    name: "project".to_owned(),
                    title: None,
                    repository: None,
                    graphs: None,
                },
                "project start",
            )
            .unwrap();
        control
            .apply(
                &admin(),
                Change::PutUser {
                    name: "alice".to_owned(),
                    admin: None,
                    roles: None,
                    password_hash: Some(Some("$argon2id$secret".to_owned())),
                },
                "local login",
            )
            .unwrap();
        // A refused change leaves no trace.
        assert!(
            control
                .apply(
                    &user("bob"),
                    Change::RemoveWorkspace("project".to_owned()),
                    "mine now"
                )
                .is_err()
        );
    }
    let control = open();
    assert!(!control.is_new());
    let state = control.state();
    assert!(state.settings.enforced);
    assert_eq!(state.roles.len(), 2);
    assert_eq!(
        state.workspaces["project"].members.get("alice"),
        Some(&Level::Owner)
    );
    let history = control.history(10).unwrap();
    let summary: Vec<(u64, &str, &str)> = history
        .iter()
        .map(|r| (r.number, r.author.as_str(), r.reason.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (3, "administrator", "local login"),
            (2, "alice", "project start"),
            (1, "administrator", "the policy file at start"),
        ]
    );
    assert!(history[1].added.iter().any(|l| l.contains("Workspace")));
    assert!(history[1].removed.is_empty());
    // Password hashes are not kept in the history.
    assert!(history[0].added.iter().all(|l| !l.contains("secret")));
    assert!(history[0].time.ends_with('Z') && history[0].time.len() == 20);
    // The next change continues the numbering.
    let record = control
        .apply(
            &admin(),
            Change::RemoveRole("editor".to_owned()),
            "no editors",
        )
        .unwrap();
    assert_eq!(record.number, 4);
    assert!(record.removed.iter().any(|l| l.contains("editor")));
}

#[test]
fn local_logins_check_passwords_open_sessions_and_throttle_failures() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let limits = LoginLimits {
        failures: 3,
        ..LoginLimits::default()
    };
    let control = AccessControl::open_with(store, "urn:nrese:", limits).unwrap();
    assert!(matches!(
        control.password_hash("short"),
        Err(AccessError::Invalid(_))
    ));
    let hash = control.password_hash("correct horse battery").unwrap();
    assert!(hash.as_deref().unwrap().starts_with("$argon2id$"));
    assert!(verify_password(
        "correct horse battery",
        hash.as_deref().unwrap()
    ));
    let set_password = |hash: Option<String>| Change::PutUser {
        name: "alice".to_owned(),
        admin: None,
        roles: None,
        password_hash: Some(hash),
    };
    control
        .apply(&admin(), set_password(hash), "a local login")
        .unwrap();
    let principal = control.login("alice", "correct horse battery").unwrap();
    assert_eq!(principal.user.as_deref(), Some("alice"));
    // Verified once, remembered: the same answer again.
    assert!(control.login("alice", "correct horse battery").is_ok());
    assert!(matches!(
        control.login("alice", "wrong"),
        Err(AccessError::Forbidden(_))
    ));
    assert!(control.login("nobody", "correct horse battery").is_err());
    let (token, lifetime) = control
        .open_session("alice", "correct horse battery")
        .unwrap();
    assert!(token.starts_with(SESSION_PREFIX));
    assert_eq!(lifetime.as_secs(), 12 * 3600);
    assert_eq!(
        control.session(&token).and_then(|p| p.user).as_deref(),
        Some("alice")
    );
    assert!(control.session("nrese-session.forged").is_none());
    // A new password ends the sessions and the old password.
    let new = control.password_hash("another long secret").unwrap();
    control
        .apply(&user("alice"), set_password(new), "rotated")
        .unwrap();
    assert!(control.session(&token).is_none());
    assert!(control.login("alice", "correct horse battery").is_err());
    // A success clears the count; three failures within the window, and the right
    // password is refused too, until the window has passed.
    for _ in 0..2 {
        assert!(control.login("alice", "guess").is_err());
    }
    assert!(matches!(
        control.login("alice", "another long secret"),
        Err(AccessError::Throttled(_))
    ));
}
