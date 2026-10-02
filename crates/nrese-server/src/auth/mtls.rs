use std::collections::BTreeSet;

use axum::http::HeaderMap;

use crate::auth::{AccessGrant, Authenticated, Identity};
use crate::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtlsConfig {
    /// The header a TLS-terminating proxy passes the client certificate's subject in. It
    /// counts only from `trusted_proxies` ([`super::peers`]): from any other peer it is
    /// dropped before authentication.
    pub subject_header: String,
    /// The peers that may send `subject_header` (loopback by default).
    pub trusted_proxies: Vec<super::peers::AddressRange>,
    pub read_subjects: BTreeSet<String>,
    pub admin_subjects: BTreeSet<String>,
}

pub fn authenticate(config: &MtlsConfig, headers: &HeaderMap) -> Result<Authenticated, ApiError> {
    let subject = extract_subject(headers, &config.subject_header)?;
    let grants = grants_for_subject(config, subject);
    // The subject is a role name of its own, so a policy can name one certificate.
    let mut roles = BTreeSet::from([subject.to_owned()]);
    if grants.contains(&AccessGrant::Read) {
        roles.insert("reader".to_owned());
    }
    Ok(Authenticated {
        identity: Identity::from_grants(&grants, roles).with_user(Some(subject)),
        // A subject the configuration doesn't list is unknown, whatever a policy names.
        known: !grants.is_empty(),
        grants,
        refusal: "client certificate subject does not grant access to this endpoint",
    })
}

fn extract_subject<'a>(headers: &'a HeaderMap, subject_header: &str) -> Result<&'a str, ApiError> {
    let value = headers
        .get(subject_header)
        .ok_or_else(|| ApiError::unauthorized("missing client certificate subject header"))?;

    value
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
        .ok_or_else(|| ApiError::unauthorized("missing client certificate subject header"))
}

fn grants_for_subject(config: &MtlsConfig, subject: &str) -> BTreeSet<AccessGrant> {
    let mut grants = BTreeSet::new();
    if config.admin_subjects.contains(subject) {
        grants.insert(AccessGrant::Admin);
    }
    if config.read_subjects.contains(subject) {
        grants.insert(AccessGrant::Read);
    }
    grants
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{MtlsConfig, grants_for_subject};
    use crate::auth::AccessGrant;

    #[test]
    fn admin_subject_maps_to_admin_grant() {
        let config = MtlsConfig {
            subject_header: "x-client-cert-subject".to_owned(),
            trusted_proxies: crate::auth::peers::AddressRange::loopback(),
            read_subjects: BTreeSet::new(),
            admin_subjects: BTreeSet::from(["CN=admin".to_owned()]),
        };

        assert_eq!(
            grants_for_subject(&config, "CN=admin"),
            BTreeSet::from([AccessGrant::Admin])
        );
    }
}
