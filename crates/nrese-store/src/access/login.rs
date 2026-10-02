//! Local logins, for standalone and desktop installations without an identity provider:
//! passwords hashed with Argon2id (the PHC string kept in the user's record), checked on
//! `Basic` credentials or at a login that opens a session.
//!
//! Argon2 is slow by design (tens of milliseconds): a credential verified once is
//! remembered by its SHA-256 for as long as the user's hash stays the same, so a client
//! that sends `Basic` credentials with every request pays it once. Failed attempts are
//! counted per user name; past [`LoginLimits::failures`] within [`LoginLimits::window`]
//! the user's logins are refused until the window has passed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};

use super::{AccessError, AccessState, Principal};

/// The bounds of local logins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginLimits {
    /// Failed attempts per user name within `window` before its logins are refused.
    pub failures: u32,
    pub window: Duration,
    /// Credentials remembered as verified (beyond, the memory starts over).
    pub remembered: usize,
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self {
            failures: 10,
            window: Duration::from_secs(300),
            remembered: 10_000,
        }
    }
}

/// The PHC string of `password` (Argon2id, a random salt).
pub fn hash_password(password: &str) -> Result<String, AccessError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| AccessError::Store(format!("password hashing: {error}")))
}

/// Whether `password` is the one `hash` (a PHC string) was made from.
pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// The open sessions and what logins remember.
#[derive(Default)]
pub(super) struct Logins {
    limits: LoginLimits,
    /// Verified credentials (by SHA-256 of `user`, NUL, `password`): the user and the
    /// hash they were verified against.
    verified: Mutex<HashMap<[u8; 32], (String, String)>>,
    /// Failures per user name: how many, since when.
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// Session tokens: the user and when the session ends.
    sessions: Mutex<HashMap<String, (String, Instant)>>,
}

/// A session token's prefix, which tells it from other bearer tokens.
pub const SESSION_PREFIX: &str = "nrese-session.";

impl Logins {
    pub(super) fn new(limits: LoginLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// The principal `user` logs in as with `password`, if they match a local login.
    pub(super) fn login(
        &self,
        state: &AccessState,
        user: &str,
        password: &str,
    ) -> Result<Principal, AccessError> {
        let refused = || AccessError::Forbidden("wrong user name or password".to_owned());
        {
            let mut failures = self.failures.lock().expect("failures");
            if let Some((count, since)) = failures.get(user).copied() {
                if since.elapsed() > self.limits.window {
                    failures.remove(user);
                } else if count >= self.limits.failures {
                    return Err(AccessError::Throttled(format!(
                        "too many failed logins for '{user}'; try again later"
                    )));
                }
            }
        }
        let Some(hash) = state.users.get(user).and_then(|u| u.password_hash.clone()) else {
            self.failed(user);
            return Err(refused());
        };
        let key: [u8; 32] = Sha256::new()
            .chain_update(user.as_bytes())
            .chain_update([0])
            .chain_update(password.as_bytes())
            .finalize()
            .into();
        let known = self
            .verified
            .lock()
            .expect("verified")
            .get(&key)
            .is_some_and(|(name, verified)| name == user && *verified == hash);
        if !known {
            if !verify_password(password, &hash) {
                self.failed(user);
                return Err(refused());
            }
            let mut verified = self.verified.lock().expect("verified");
            if verified.len() >= self.limits.remembered {
                verified.clear();
            }
            verified.insert(key, (user.to_owned(), hash));
        }
        self.failures.lock().expect("failures").remove(user);
        Ok(Principal {
            user: Some(user.to_owned()),
            ..Principal::default()
        })
    }

    fn failed(&self, user: &str) {
        let mut failures = self.failures.lock().expect("failures");
        let entry = failures
            .entry(user.to_owned())
            .or_insert((0, Instant::now()));
        entry.0 += 1;
    }

    /// Opens a session for `user` for `lifetime`; its token.
    pub(super) fn open(&self, user: &str, lifetime: Duration) -> Result<String, AccessError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| AccessError::Store(format!("no randomness: {error}")))?;
        let token = format!(
            "{SESSION_PREFIX}{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let mut sessions = self.sessions.lock().expect("sessions");
        sessions.retain(|_, (_, until)| *until > Instant::now());
        sessions.insert(token.clone(), (user.to_owned(), Instant::now() + lifetime));
        Ok(token)
    }

    /// The user of session `token`, while it lasts.
    pub(super) fn session(&self, token: &str) -> Option<String> {
        let sessions = self.sessions.lock().expect("sessions");
        sessions
            .get(token)
            .filter(|(_, until)| *until > Instant::now())
            .map(|(user, _)| user.clone())
    }

    /// Closes session `token`; whether it was open.
    pub(super) fn close(&self, token: &str) -> bool {
        self.sessions
            .lock()
            .expect("sessions")
            .remove(token)
            .is_some()
    }

    /// Closes every session of `user` (its password or record changed).
    pub(super) fn close_all(&self, user: &str) {
        self.sessions
            .lock()
            .expect("sessions")
            .retain(|_, (owner, _)| owner != user);
    }
}
