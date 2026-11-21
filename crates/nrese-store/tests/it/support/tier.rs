//! The check tier, for the integration tests here and the test binaries of their own that
//! include this file.

/// The check tier (`NRESE_CHECK_TIER`): `landing` (the default) or `nightly`.
pub(crate) fn nightly() -> bool {
    match std::env::var("NRESE_CHECK_TIER").as_deref() {
        Err(_) | Ok("landing") => false,
        Ok("nightly") => true,
        Ok(other) => panic!("NRESE_CHECK_TIER={other:?}: expected `landing` or `nightly`"),
    }
}
