use std::collections::BTreeSet;

use serde::Deserialize;

use super::AccessGrant;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StringOrMany {
    One(String),
    Many(Vec<String>),
}

pub fn grants_from_claim_parts(
    read_role: &str,
    admin_role: &str,
    scope: Option<&str>,
    scp: Option<&StringOrMany>,
    role: Option<&str>,
    roles: Option<&StringOrMany>,
) -> BTreeSet<AccessGrant> {
    let mut role_names = BTreeSet::new();
    extend_space_delimited(&mut role_names, scope);
    extend_string_or_many(&mut role_names, scp);
    if let Some(role) = role {
        role_names.insert(role.to_owned());
    }
    extend_string_or_many(&mut role_names, roles);

    let mut grants = BTreeSet::new();
    if role_names.contains(admin_role) {
        grants.insert(AccessGrant::Admin);
    }
    if role_names.contains(read_role) {
        grants.insert(AccessGrant::Read);
    }
    grants
}

fn extend_space_delimited(target: &mut BTreeSet<String>, value: Option<&str>) {
    if let Some(value) = value {
        for role in value.split_whitespace() {
            if !role.is_empty() {
                target.insert(role.to_owned());
            }
        }
    }
}

fn extend_string_or_many(target: &mut BTreeSet<String>, value: Option<&StringOrMany>) {
    match value {
        Some(StringOrMany::One(single)) => extend_space_delimited(target, Some(single)),
        Some(StringOrMany::Many(values)) => {
            for value in values {
                if !value.is_empty() {
                    target.insert(value.to_owned());
                }
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{StringOrMany, grants_from_claim_parts};
    use crate::auth::AccessGrant;

    #[test]
    fn grants_are_collected_from_scope_and_roles() {
        assert_eq!(
            grants_from_claim_parts(
                "nrese.read",
                "nrese.admin",
                Some("nrese.read"),
                None,
                None,
                Some(&StringOrMany::Many(vec!["nrese.admin".to_owned()])),
            ),
            BTreeSet::from([AccessGrant::Read, AccessGrant::Admin])
        );
    }
}
