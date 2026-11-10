//! A session is valid only for the credential it was verified against (SYN-63, C-201;
//! design note `2026-11-08-p2-b5-syn63-design.md`, attacks A1–A5): a login whose
//! verification a change of the user's credential overtakes opens no session, and no
//! token it may have handed out is accepted afterwards.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::*;

const PASSWORD: &str = "correct horse battery";

thread_local! {
    /// The pause of the thread that logs in: it says it has verified on the sender, then
    /// waits for the receiver. Other threads (other tests) have none.
    static PAUSE: RefCell<Option<(Sender<()>, Receiver<()>)>> = const { RefCell::new(None) };
}

/// [`verify_password`], then, on the thread with a [`PAUSE`], the pause: what the test
/// changes the state in, between a login's verification and its session.
fn pausing(password: &str, hash: &str) -> bool {
    let verified = verify_password(password, hash);
    if let Some((verified_now, go)) = PAUSE.with(|pause| pause.borrow_mut().take()) {
        verified_now
            .send(())
            .expect("the test waits for the verification");
        go.recv().expect("the test lets the login go on");
    }
    verified
}

fn set_password(password: &str) -> Change {
    Change::PutUser {
        name: "alice".to_owned(),
        admin: None,
        roles: None,
        password_hash: Some(Some(hash_password(password).unwrap())),
    }
}

/// alice, with a local login of [`PASSWORD`], logs in; `during` changes the state while
/// her password is being verified. What the login answered, and whom its token (if
/// any) is the session of afterwards.
fn login_while(during: &[Change]) -> (Result<(), AccessError>, Option<String>) {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let control =
        AccessControl::open_verifying_with(store, "urn:nrese:", LoginLimits::default(), pausing)
            .unwrap();
    control
        .apply(&admin(), set_password(PASSWORD), "a local login")
        .unwrap();
    let (verified_now, verified) = channel();
    let (go, gone) = channel();
    let control = &control;
    let opened = std::thread::scope(|scope| {
        let login = scope.spawn(move || {
            PAUSE.with(|pause| *pause.borrow_mut() = Some((verified_now, gone)));
            control.open_session("alice", PASSWORD, None)
        });
        verified.recv().expect("the login verifies, with a pause");
        for change in during {
            control
                .apply(&admin(), change.clone(), "meanwhile")
                .unwrap();
        }
        go.send(()).unwrap();
        login.join().unwrap()
    });
    let user = opened
        .as_ref()
        .ok()
        .and_then(|(token, _)| control.session(token))
        .and_then(|principal| principal.user);
    (opened.map(|_| ()), user)
}

/// A1–A4: rotation, two rotations, removal, removal and a new login of the same name,
/// each during the verification. Specified (SYN-63 row; the invariant): the login is
/// refused as a wrong password, and no session of alice exists afterwards. A5 (guard,
/// passes without the fix): a change that leaves the password alone keeps the login.
#[test]
fn a_credential_change_during_a_login_leaves_no_session_of_the_old_one() {
    let rows: [(&str, Vec<Change>); 4] = [
        ("rotated", vec![set_password("a second long secret")]),
        (
            "rotated twice",
            vec![
                set_password("a second long secret"),
                set_password("a third long secret"),
            ],
        ),
        ("removed", vec![Change::RemoveUser("alice".to_owned())]),
        (
            "removed and created again",
            vec![
                Change::RemoveUser("alice".to_owned()),
                set_password("a new user's secret"),
            ],
        ),
    ];
    let mut wrong = Vec::new();
    for (row, during) in rows {
        let (opened, session) = login_while(&during);
        if !matches!(opened, Err(AccessError::Forbidden(_))) {
            wrong.push(format!(
                "{row}: the login answered {opened:?}, not a refusal"
            ));
        }
        if let Some(user) = session {
            wrong.push(format!("{row}: its token is a session of {user}"));
        }
    }
    let (opened, session) = login_while(&[Change::PutUser {
        name: "alice".to_owned(),
        admin: None,
        roles: Some(BTreeSet::from(["reader".to_owned()])),
        password_hash: None,
    }]);
    if opened.is_err() || session.as_deref() != Some("alice") {
        wrong.push(format!(
            "guard, roles changed: {opened:?}, session of {session:?}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
