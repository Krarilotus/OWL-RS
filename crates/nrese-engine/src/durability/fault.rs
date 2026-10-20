//! Fault points in the commit protocol ([`crate::engine::bulk`]) and the WAL: a test makes
//! the next pass through one step on its own thread fail with an I/O error. Outside tests
//! [`check`] does nothing.

/// The steps a fault can be injected at, in protocol order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Syncing the active WAL segment and starting the next.
    Rotate,
    /// Writing the image under its temporary name.
    Write,
    /// Mapping the written image.
    Map,
    /// Renaming the image into place.
    Rename,
    /// Syncing the directory after the rename.
    DirSync,
    /// Releasing the WAL segments the image covers.
    Release,
    /// Removing older checkpoints.
    RemoveOlder,
}

#[cfg(test)]
thread_local! {
    static ARMED: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

/// Makes the next `check(step)` on this thread fail.
#[cfg(test)]
pub(crate) fn fail_at(step: Step) {
    ARMED.set(Some(step));
}

/// The step armed on this thread and not yet reached.
#[cfg(test)]
pub(crate) fn armed() -> Option<Step> {
    ARMED.get()
}

/// Fails once if a test armed `step` on this thread.
#[cfg(test)]
pub(crate) fn check(step: Step) -> std::io::Result<()> {
    match ARMED.get() == Some(step) {
        true => {
            ARMED.set(None);
            Err(std::io::Error::other(format!("injected fault at {step:?}")))
        }
        false => Ok(()),
    }
}

#[cfg(not(test))]
#[inline(always)]
pub(crate) fn check(_step: Step) -> std::io::Result<()> {
    Ok(())
}
