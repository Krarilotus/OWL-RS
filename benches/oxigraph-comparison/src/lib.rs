//! Input generators shared by the differential tests and the benchmarks. Everything is
//! seeded, so a run is repeatable.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

fn pick<'a>(rng: &mut StdRng, items: &[&'a str]) -> &'a str {
    items[rng.random_range(0..items.len())]
}

/// A random IRI reference: mostly well-formed pieces, sometimes a character or piece
/// that makes it invalid, so both acceptance and resolution are exercised.
pub fn iri_reference(rng: &mut StdRng) -> String {
    let mut out = String::new();
    if rng.random_bool(0.6) {
        out.push_str(pick(rng, &["http:", "https:", "urn:", "a:", "file:", "mailto:", "tag:", "g:", "1a:", "a+b.c-d:"]));
    }
    if rng.random_bool(0.5) {
        out.push_str("//");
        if rng.random_bool(0.2) {
            out.push_str(pick(rng, &["user@", "u:p@", "%41@", "a b@"]));
        }
        out.push_str(pick(rng, &[
            "example.org", "h", "[::1]", "[2001:db8::7]", "[v7.a]", "[1:2:3:4:5:6:7:8]",
            "[::ffff:1.2.3.4]", "[1::2::3]", "[zz]", "", "ex%2Eorg", "Übung.de", "a_b~c",
        ]));
        if rng.random_bool(0.3) {
            out.push_str(pick(rng, &[":80", ":", ":8a", ":65536"]));
        }
    }
    for _ in 0..rng.random_range(0..4) {
        out.push_str(pick(rng, &[
            "/", "a", "b/", "..", ".", "../", "./", "c;p", "%20", "%zz", "%a", " ", "ü", "\u{E000}",
            "<", "{", "\\", "@", ":", "d:e", "=", "!", "'", "(x)", "*", "+", ",",
        ]));
    }
    if rng.random_bool(0.3) {
        out.push('?');
        out.push_str(pick(rng, &["q", "a=b&c", "\u{E000}", "/?:@", "%", "x y"]));
    }
    if rng.random_bool(0.3) {
        out.push('#');
        out.push_str(pick(rng, &["f", "s/../x", "\u{E000}", "#", "a?b"]));
    }
    out
}

/// A random text near the lexical forms of numbers.
pub fn numeric_lexical(rng: &mut StdRng) -> String {
    let mut out = String::new();
    if rng.random_bool(0.3) {
        out.push_str(pick(rng, &["+", "-"]));
    }
    match rng.random_range(0..10) {
        0 => out.push_str(pick(rng, &["INF", "NaN", "inf", "nan", "infinity", "Infinity"])),
        _ => {
            for _ in 0..rng.random_range(0..6) {
                out.push(char::from(b'0' + rng.random_range(0..10)));
            }
            if rng.random_bool(0.5) {
                out.push('.');
                for _ in 0..rng.random_range(0..25) {
                    out.push(char::from(b'0' + rng.random_range(0..10)));
                }
            }
            if rng.random_bool(0.3) {
                out.push_str(pick(rng, &["e", "E"]));
                if rng.random_bool(0.3) {
                    out.push_str(pick(rng, &["+", "-"]));
                }
                for _ in 0..rng.random_range(0..4) {
                    out.push(char::from(b'0' + rng.random_range(0..10)));
                }
            }
        }
    }
    if rng.random_bool(0.03) {
        out.push_str(pick(rng, &[" ", "x", "_", "."]));
    }
    out
}

/// A random decimal lexical form of moderate size.
pub fn decimal_lexical(rng: &mut StdRng) -> String {
    let sign = if rng.random_bool(0.3) { "-" } else { "" };
    let whole: u64 = match rng.random_range(0..3) {
        0 => rng.random_range(0..10),
        1 => rng.random_range(0..100_000),
        _ => rng.random_range(0..10_000_000_000),
    };
    let digits = rng.random_range(0..12);
    let fraction: String = (0..digits).map(|_| char::from(b'0' + rng.random_range(0..10))).collect();
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

/// A random text near the lexical forms of dates, times and durations.
pub fn temporal_lexical(rng: &mut StdRng) -> String {
    let two = |rng: &mut StdRng, max: u32| format!("{:02}", rng.random_range(0..=max));
    let year = pick(rng, &["2002", "0000", "-0045", "12000", "02002", "1999", "2000", "1900", "999"]).to_owned();
    let tz = pick(rng, &["", "", "Z", "+01:00", "-05:00", "+14:00", "-14:01", "+00:00", "z"]).to_owned();
    let seconds = format!("{}{}", two(rng, 61), pick(rng, &["", "", ".5", ".123456789", ".", ".0000000000000000001"]));
    let time = format!("{}:{}:{}", two(rng, 25), two(rng, 61), seconds);
    let date = format!("{year}-{}-{}", two(rng, 13), two(rng, 32));
    match rng.random_range(0..9) {
        0 => format!("{date}T{time}{tz}"),
        1 => format!("{date}{tz}"),
        2 => format!("{time}{tz}"),
        3 => format!("{year}-{}{tz}", two(rng, 13)),
        4 => format!("{year}{tz}"),
        5 => format!("--{}-{}{tz}", two(rng, 13), two(rng, 32)),
        6 => format!("---{}{tz}", two(rng, 32)),
        7 => format!("--{}{tz}", two(rng, 13)),
        _ => {
            let mut d = String::from(pick(rng, &["", "", "-", "+"]));
            d.push('P');
            for (designator, time) in [("Y", false), ("M", false), ("D", false), ("H", true), ("M", true), ("S", true)] {
                if time && !d.contains('T') && rng.random_bool(0.5) {
                    d.push('T');
                }
                if rng.random_bool(0.4) {
                    d.push_str(&rng.random_range(0..100).to_string());
                    if designator == "S" && rng.random_bool(0.3) {
                        d.push_str(".25");
                    }
                    d.push_str(designator);
                }
            }
            d
        }
    }
}
