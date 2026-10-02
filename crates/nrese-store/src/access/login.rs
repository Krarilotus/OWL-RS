//! Local logins, for standalone and desktop installations without an identity provider:
//! passwords hashed with Argon2id (the PHC string kept in the user's record), checked on
//! `Basic` credentials or at a login that opens a session.
//!
//! Argon2 is slow by design (tens of milliseconds): a credential verified once is
//! remembered for as long as the user's hash stays the same, by its HMAC-SHA-256 under a
//! key drawn at start and never stored (so a memory dump doesn't let anyone test guesses
//! at SHA-256 speed, past Argon2), so a client
//! that sends `Basic` credentials with every request pays it once. A user name without a
//! login is checked against a fixed hash all the same, so the time a refusal takes doesn't
//! tell which names exist.
//!
//! Failed attempts are counted per user name and client address, never per name alone:
//! someone who can reach the port can slow down their own guesses, not lock a user out.
//! Past [`LoginLimits::failures`], the pair waits before its next attempt, one second
//! doubling with each further failure, up to [`LoginLimits::window`]; and an address with
//! [`LoginLimits::address_failures`] failures over any names waits likewise. Counts end a
//! `window` after their last failure. Names without a login are counted like the others
//! (else the throttling itself would tell them apart); both counts are bounded by
//! [`LoginLimits::tracked`] entries, the oldest dropped first.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};

use super::{AccessError, AccessState, Principal};

/// The bounds of local logins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginLimits {
    /// Failed attempts per user name and client address before each further attempt waits.
    pub failures: u32,
    /// Failed attempts per client address, over all user names, before it waits.
    pub address_failures: u32,
    /// How long a count lasts after its last failure, and the longest wait.
    pub window: Duration,
    /// Credentials remembered as verified (beyond, the memory starts over).
    pub remembered: usize,
    /// Counts kept, per name and address and per address (beyond, the oldest go).
    pub tracked: usize,
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self {
            failures: 10,
            address_failures: 100,
            window: Duration::from_secs(300),
            remembered: 10_000,
            tracked: 10_000,
        }
    }
}

/// Failed attempts: how many, and when the last was.
#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
}

/// Failure counts by key, bounded.
struct Counts<K> {
    counts: HashMap<K, Failures>,
}

impl<K> Default for Counts<K> {
    fn default() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> Counts<K> {
    /// How long `key` must still wait: past `free` failures, one second doubling with
    /// each further one, up to `limits.window`. A count a window old is forgotten.
    fn wait(&mut self, key: &K, free: u32, limits: &LoginLimits) -> Option<Duration> {
        let failures = *self.counts.get(key)?;
        let since = failures.last.elapsed();
        if since > limits.window {
            self.counts.remove(key);
            return None;
        }
        let over = failures.count.checked_sub(free)?;
        let backoff = Duration::from_secs(1)
            .checked_mul(1u32.checked_shl(over.min(31)).unwrap_or(u32::MAX))
            .unwrap_or(limits.window)
            .min(limits.window);
        backoff.checked_sub(since).filter(|left| !left.is_zero())
    }

    fn fail(&mut self, key: &K, limits: &LoginLimits) {
        let now = Instant::now();
        if !self.counts.contains_key(key) && self.counts.len() >= limits.tracked {
            self.counts
                .retain(|_, failures| failures.last.elapsed() <= limits.window);
            if self.counts.len() >= limits.tracked
                && let Some(oldest) = self
                    .counts
                    .iter()
                    .min_by_key(|(_, failures)| failures.last)
                    .map(|(key, _)| key.clone())
            {
                self.counts.remove(&oldest);
            }
        }
        let failures = self.counts.entry(key.clone()).or_insert(Failures {
            count: 0,
            last: now,
        });
        failures.count = failures.count.saturating_add(1);
        failures.last = now;
    }

    fn clear(&mut self, key: &K) {
        self.counts.remove(key);
    }

    fn len(&self) -> usize {
        self.counts.len()
    }
}

/// The failure counts of local logins.
#[derive(Default)]
struct Throttle {
    /// Per user name and client address (`None`: a caller that knows no address).
    pairs: Counts<(String, Option<IpAddr>)>,
    /// Per client address, over all names.
    addresses: Counts<Option<IpAddr>>,
}

/// The hash a user name without a login is checked against: the work of a check, which
/// no password passes in practice (made from random bytes).
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let mut bytes = [0u8; 32];
        // Without randomness, zeros: it only has to cost what a check costs.
        let _ = getrandom::fill(&mut bytes);
        let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        hash_password(&text).unwrap_or_default()
    })
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

/// HMAC-SHA-256 (RFC 2104) of the concatenated `parts` under `key` (at most one block).
fn hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    padded[..key.len()].copy_from_slice(key);
    let pad = |byte: u8| padded.map(|k| k ^ byte);
    let mut inner = Sha256::new().chain_update(pad(0x36));
    for part in parts {
        inner.update(part);
    }
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner.finalize())
        .finalize()
        .into()
}

/// The open sessions and what logins remember.
#[derive(Default)]
pub(super) struct Logins {
    limits: LoginLimits,
    /// The key of [`Self::verified`]'s HMACs: random, per process.
    secret: [u8; 32],
    /// Verified credentials (by HMAC of `user`, NUL, `password`): the user and the hash
    /// they were verified against.
    verified: Mutex<HashMap<[u8; 32], (String, String)>>,
    /// Failed attempts ([`Throttle`]).
    failures: Mutex<Throttle>,
    /// Session tokens: the user and when the session ends.
    sessions: Mutex<HashMap<String, (String, Instant)>>,
}

/// A session token's prefix, which tells it from other bearer tokens.
pub const SESSION_PREFIX: &str = "nrese-session.";

impl Logins {
    pub(super) fn new(limits: LoginLimits) -> Self {
        let mut secret = [0u8; 32];
        // Without randomness, the cache key is a plain hash again, which is what it was.
        let _ = getrandom::fill(&mut secret);
        Self {
            limits,
            secret,
            ..Self::default()
        }
    }

    /// The principal `user` logs in as with `password` from `client` (the client's
    /// address, if known), if they match a local login.
    pub(super) fn login(
        &self,
        state: &AccessState,
        user: &str,
        password: &str,
        client: Option<IpAddr>,
    ) -> Result<Principal, AccessError> {
        let refused = || AccessError::Forbidden("wrong user name or password".to_owned());
        let pair = (user.to_owned(), client);
        {
            let mut failures = self.failures.lock().expect("failures");
            let limits = &self.limits;
            let wait = failures
                .addresses
                .wait(&client, limits.address_failures, limits)
                .into_iter()
                .chain(failures.pairs.wait(&pair, limits.failures, limits))
                .max();
            if let Some(wait) = wait {
                return Err(AccessError::Throttled(format!(
                    "too many failed logins; try again in {} s",
                    wait.as_secs().max(1)
                )));
            }
        }
        let Some(hash) = state.users.get(user).and_then(|u| u.password_hash.clone()) else {
            // As long as a wrong password takes.
            let _ = verify_password(password, dummy_hash());
            self.failed(&pair);
            return Err(refused());
        };
        let key = hmac_sha256(&self.secret, &[user.as_bytes(), &[0], password.as_bytes()]);
        let known = self
            .verified
            .lock()
            .expect("verified")
            .get(&key)
            .is_some_and(|(name, verified)| name == user && *verified == hash);
        if !known {
            if !verify_password(password, &hash) {
                self.failed(&pair);
                return Err(refused());
            }
            let mut verified = self.verified.lock().expect("verified");
            if verified.len() >= self.limits.remembered {
                verified.clear();
            }
            verified.insert(key, (user.to_owned(), hash));
        }
        self.failures.lock().expect("failures").pairs.clear(&pair);
        Ok(Principal {
            user: Some(user.to_owned()),
            ..Principal::default()
        })
    }

    fn failed(&self, pair: &(String, Option<IpAddr>)) {
        let mut failures = self.failures.lock().expect("failures");
        failures.pairs.fail(pair, &self.limits);
        failures.addresses.fail(&pair.1, &self.limits);
    }

    /// The failure counts kept: per name and address, per address.
    pub(super) fn tracked(&self) -> (usize, usize) {
        let failures = self.failures.lock().expect("failures");
        (failures.pairs.len(), failures.addresses.len())
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

#[cfg(test)]
mod tests {
    /// RFC 4231, test case 1.
    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 1 uses a 20-byte key 0x0b..; padded to 32 bytes with zeros it
        // is the same HMAC key (keys shorter than a block are zero-padded).
        let mut key = [0u8; 32];
        key[..20].fill(0x0b);
        let mac = super::hmac_sha256(&key, &[b"Hi ", b"There"]);
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}
