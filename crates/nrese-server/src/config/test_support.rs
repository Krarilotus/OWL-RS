use std::sync::{Mutex, OnceLock};

pub(super) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    pub(super) fn set(values: &[(&'static str, Option<&str>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, value)| {
                let old = std::env::var(key).ok();
                // Only the config tests change the environment, one at a time under the
                // guard's lock; the other tests of this binary don't read these variables.
                // (B3 of the audit work replaces this with an injected source.)
                match value {
                    // SAFETY: see above.
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    // SAFETY: see above.
                    None => unsafe { std::env::remove_var(key) },
                }
                (*key, old)
            })
            .collect();

        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                // SAFETY: as in `set`: the guard's lock is still held.
                Some(value) => unsafe { std::env::set_var(key, value) },
                // SAFETY: as in `set`.
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}
