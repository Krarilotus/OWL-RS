//! Spec: config-reference.md Budgets; P4-F1 requires sizes and 0..=100% shares,
//! with unread capacity refused for limits. Each row runs independently.

use super::{ServerConfig, source::KeyValueSource, units};

macro_rules! query_memory_row {
    ($name:ident, $text:literal, $expected:expr) => {
        #[test]
        fn $name() {
            let capacity = units::machine_memory_bytes().expect("supported proof platform");
            let config = ServerConfig::load_from(
                None,
                KeyValueSource::default(),
                &[("budgets.query_memory".into(), $text.into())],
            );
            let actual = config.map(|c| c.policy.limits.max_query_memory_bytes as u64);
            let expected: Option<u64> = ($expected)(capacity);
            match expected {
                Some(bytes) => assert_eq!(actual.unwrap(), bytes, "{}", $text),
                None => assert!(actual.is_err(), "{} was accepted", $text),
            }
        }
    };
}

query_memory_row!(guard_plain_bytes, "1048576", |_| Some(1 << 20));
query_memory_row!(guard_binary_size, "1.5 GiB", |_| Some(3 << 29));
query_memory_row!(guard_decimal_size, "2GB", |_| Some(2_000_000_000));
query_memory_row!(share_half, "50%", |capacity| Some(capacity / 2));
query_memory_row!(share_zero_is_unlimited, "0%", |_| Some(0));
query_memory_row!(share_whole_capacity, "100%", Some);
query_memory_row!(guard_share_past_capacity, "100.1%", |_| None);
query_memory_row!(guard_negative_share, "-1%", |_| None);
query_memory_row!(guard_malformed_share, "half%", |_| None);
query_memory_row!(guard_nan_share, "NaN%", |_| None);
query_memory_row!(guard_malformed_size, "1GiBB", |_| None);

#[test]
fn guard_unknown_capacity_refuses_shares_but_accepts_sizes() {
    use nrese_exec::memory::Unread;
    for unread in [Unread::Failed, Unread::Unsupported] {
        let parse = |text| units::parse_memory_limit(text, Err(unread), "query_memory");
        assert!(parse("50%").is_err(), "{unread}");
        assert!(parse("0%").is_err(), "{unread}");
        assert!(parse("100%").is_err(), "{unread}");
        assert_eq!(parse("1MiB").unwrap(), 1 << 20);
        assert_eq!(parse("0").unwrap(), 0);
    }
}
