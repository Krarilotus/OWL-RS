//! Language tags: well-formedness by BCP 47's grammar (RFC 5646 §2.1).

/// Whether `tag` is a well-formed BCP 47 language tag (any letter case).
pub fn is_well_formed(tag: &str) -> bool {
    if !tag.is_ascii() || tag.is_empty() {
        return false;
    }
    let lower = tag.to_ascii_lowercase();
    if GRANDFATHERED.contains(&lower.as_str()) {
        return true;
    }
    let subtags: Vec<&str> = lower.split('-').collect();
    if subtags
        .iter()
        .any(|s| s.is_empty() || s.len() > 8 || !is_alphanumeric(s))
    {
        return false;
    }
    if subtags[0] == "x" {
        return private_use(&subtags[1..]);
    }
    langtag(&subtags)
}

/// The tags the grammar lists as grandfathered, in lower case.
const GRANDFATHERED: [&str; 26] = [
    "en-gb-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "sgn-be-fr",
    "sgn-be-nl",
    "sgn-ch-de",
    "art-lojban",
    "cel-gaulish",
    "no-bok",
    "no-nyn",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-xiang",
];

fn is_alphanumeric(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_alpha(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_digit(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// `1*("-" (1*8alphanum))` after the `x`.
fn private_use(rest: &[&str]) -> bool {
    !rest.is_empty()
}

/// `language ["-" script] ["-" region] *("-" variant) *("-" extension) ["-" privateuse]`
fn langtag(subtags: &[&str]) -> bool {
    let mut i = 0;
    let n = subtags.len();
    // language = 2*3ALPHA ["-" extlang] / 4ALPHA / 5*8ALPHA
    let language = subtags[0];
    if !is_alpha(language) || language.len() < 2 {
        return false;
    }
    i += 1;
    if language.len() <= 3 {
        // extlang = 3ALPHA *2("-" 3ALPHA)
        let mut extlangs = 0;
        while i < n && extlangs < 3 && subtags[i].len() == 3 && is_alpha(subtags[i]) {
            i += 1;
            extlangs += 1;
        }
    }
    // script = 4ALPHA
    if i < n && subtags[i].len() == 4 && is_alpha(subtags[i]) {
        i += 1;
    }
    // region = 2ALPHA / 3DIGIT
    if i < n
        && ((subtags[i].len() == 2 && is_alpha(subtags[i]))
            || (subtags[i].len() == 3 && is_digit(subtags[i])))
    {
        i += 1;
    }
    // variant = 5*8alphanum / (DIGIT 3alphanum)
    while i < n
        && (subtags[i].len() >= 5
            || (subtags[i].len() == 4 && subtags[i].as_bytes()[0].is_ascii_digit()))
    {
        i += 1;
    }
    // extension = singleton 1*("-" (2*8alphanum)), singleton any alphanumeric but x
    while i < n && subtags[i].len() == 1 && subtags[i] != "x" {
        i += 1;
        let start = i;
        while i < n && subtags[i].len() >= 2 {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    // ["-" privateuse]
    if i < n && subtags[i] == "x" {
        return private_use(&subtags[i + 1..]);
    }
    i == n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5646 Appendix A, well-formed and ill-formed examples.
    #[test]
    fn the_rfc_examples() {
        for ok in [
            "de",
            "fr",
            "ja",
            "i-enochian",
            "zh-Hant",
            "zh-Hans",
            "sr-Cyrl",
            "sr-Latn",
            "zh-cmn-Hans-CN",
            "cmn-Hans-CN",
            "zh-yue-HK",
            "yue-HK",
            "zh-Hans-CN",
            "sr-Latn-RS",
            "sl-rozaj",
            "sl-rozaj-biske",
            "sl-nedis",
            "de-CH-1901",
            "sl-IT-nedis",
            "hy-Latn-IT-arevela",
            "de-DE",
            "en-US",
            "es-419",
            "de-CH-x-phonebk",
            "az-Arab-x-AZE-derbend",
            "x-whatever",
            "qaa-Qaaa-QM-x-southern",
            "de-Qaaa",
            "sr-Latn-QM",
            "sr-Qaaa-RS",
            "en-US-u-islamcal",
            "zh-CN-a-myext-x-private",
            "en-a-myext-b-another",
            "EN-gb-OED",
            "en-GB-oed",
        ] {
            assert!(is_well_formed(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "de-419-DE",
            "de-419-DE-",
            "a-DE",
            "ar-a-aaa-b-bbb-a",
            "abcdefghi",
            "en--US",
            "en-",
            "en-a",
            "en-x",
            "12",
            "en-US-a-b",
            "Übung",
            "en_US",
            "en-abcdefghi",
        ] {
            assert!(!is_well_formed(bad), "{bad}");
        }
    }
}
