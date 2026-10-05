//! The string datatypes of the map as one chain of subsets (XML Schema 1.1 Part 2 §3.4,
//! names per XML 1.0 fifth edition):
//!
//! `string ⊇ normalizedString ⊇ token ⊇ NMTOKEN ⊇ Name ⊇ NCName ⊇ language`
//!
//! (a language tag starts with a letter and has letters, digits and hyphens only, so it is
//! an NCName). The differences of consecutive types are seven disjoint *regions*; every
//! string type is a union of the regions from one on, so sets of strings are sets of
//! lengths per region, which count and complement exactly.

/// The regions, by index: what each holds and is not.
pub const REGIONS: usize = 7;

/// `Char` of XML 1.0: what a string value may hold.
pub fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

pub fn is_normalized(s: &str) -> bool {
    !s.contains(['\t', '\n', '\r'])
}

pub fn is_token(s: &str) -> bool {
    is_normalized(s) && !s.starts_with(' ') && !s.ends_with(' ') && !s.contains("  ")
}

pub fn is_nmtoken(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_name_char)
}

pub fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(is_name_start) && chars.all(is_name_char)
}

pub fn is_ncname(s: &str) -> bool {
    is_name(s) && !s.contains(':')
}

/// `[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`.
pub fn is_language(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or("");
    let ok = |p: &str, alpha: bool| {
        (1..=8).contains(&p.len())
            && p.bytes().all(|b| {
                if alpha {
                    b.is_ascii_alphabetic()
                } else {
                    b.is_ascii_alphanumeric()
                }
            })
    };
    ok(first, true) && parts.all(|p| ok(p, false))
}

/// The region of a string (of `Char`s): 0 not normalized, 1 normalized not a token, 2 a
/// token not an NMTOKEN, 3 an NMTOKEN not a Name, 4 a Name not an NCName, 5 an NCName not
/// a language tag, 6 a language tag.
pub fn region(s: &str) -> usize {
    if !is_normalized(s) {
        0
    } else if !is_token(s) {
        1
    } else if !is_nmtoken(s) {
        2
    } else if !is_name(s) {
        3
    } else if !is_ncname(s) {
        4
    } else if !is_language(s) {
        5
    } else {
        6
    }
}

/// How many strings of `length` characters the region has, as (at least, at most);
/// `u64::MAX` is "more than any search needs" (a thousand or more, and unbounded above).
pub fn count(region: usize, length: u64) -> (u64, u64) {
    const MANY: (u64, u64) = (1000, u64::MAX);
    match (region, length) {
        (2, 0) => (1, 1),
        (_, 0) => (0, 0),
        // `\t`, `\n`, `\r`.
        (0, 1) => (3, 3),
        // ` `.
        (1, 1) => (1, 1),
        // Not a name character nor a space: at least the 32 ASCII punctuation marks.
        (2, 1) => (32, u64::MAX),
        // Name characters that can't start a name: `-`, `.`, digits, U+B7, U+300–36F,
        // U+203F–2040.
        (3, 1) => (127, 127),
        // `:`.
        (4, 1) => (1, 1),
        // `[a-zA-Z]`.
        (6, 1) => (52, 52),
        _ => MANY,
    }
}

/// The strings of `length` characters in the region, where they are few and known.
pub fn strings(region: usize, length: u64) -> Option<Vec<String>> {
    Some(match (region, length) {
        (2, 0) => vec![String::new()],
        (_, 0) => Vec::new(),
        (0, 1) => ["\t", "\n", "\r"].map(str::to_owned).to_vec(),
        (1, 1) => vec![" ".to_owned()],
        (3, 1) => ('-'..='.')
            .chain('0'..='9')
            .chain(['\u{B7}'])
            .chain('\u{300}'..='\u{36F}')
            .chain('\u{203F}'..='\u{2040}')
            .map(String::from)
            .collect(),
        (4, 1) => vec![":".to_owned()],
        (6, 1) => ('a'..='z').chain('A'..='Z').map(String::from).collect(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_and_counts_agree() {
        let samples = [
            ("\tx", 0),
            (" a", 1),
            ("a  b", 1),
            ("", 2),
            ("a b", 2),
            ("!", 2),
            ("1a", 3),
            ("-", 3),
            ("a:b", 4),
            (":", 4),
            ("_x", 5),
            ("abcdefghi", 5),
            ("en-US", 6),
            ("x", 6),
        ];
        for (s, r) in samples {
            assert_eq!(region(s), r, "{s:?}");
        }
        // The chain: each type within the one before.
        for s in ["en", "a1", "_", ":", "1", "a b", " ", "\t"] {
            assert!(!is_language(s) || is_ncname(s));
            assert!(!is_ncname(s) || is_name(s));
            assert!(!is_name(s) || is_nmtoken(s));
            assert!(!is_nmtoken(s) || is_token(s));
            assert!(!is_token(s) || is_normalized(s));
        }
        // The listed strings are exactly the counted ones, each in its region.
        for r in 0..REGIONS {
            for length in 0..2 {
                if let Some(list) = strings(r, length) {
                    let (lo, hi) = count(r, length);
                    assert_eq!(
                        (lo, hi),
                        (list.len() as u64, list.len() as u64),
                        "{r} {length}"
                    );
                    for s in &list {
                        assert_eq!(region(s), r, "{s:?}");
                        assert_eq!(s.chars().count() as u64, length);
                    }
                }
            }
        }
    }
}
