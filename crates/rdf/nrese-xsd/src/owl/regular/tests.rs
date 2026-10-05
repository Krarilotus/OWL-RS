use std::ops::Bound;

use super::*;
use crate::owl::line::Line;
use crate::owl::set::Count;
use crate::owl::text;

fn lengths(lo: i128, hi: Option<i128>) -> Line<i128> {
    Line::interval(
        Bound::Included(lo),
        hi.map_or(Bound::Unbounded, Bound::Included),
    )
}

fn any_length() -> Line<i128> {
    lengths(0, None)
}

#[test]
fn patterns_match_whole_strings() {
    let p = pattern("a(b|c)").unwrap();
    for (s, ok) in [
        ("ab", true),
        ("ac", true),
        ("a", false),
        ("abc", false),
        ("xab", false),
    ] {
        assert_eq!(p.dfa.matches(s), ok, "{s}");
    }
    let phone = pattern(r"\d{3}-\d{4}").unwrap();
    assert!(phone.dfa.matches("555-1234") && !phone.dfa.matches("5551234"));
    // `^` and `$` are characters.
    assert!(pattern("^a$").unwrap().dfa.matches("^a$"));
    let vowels_out = pattern("[a-z-[aeiou]]+").unwrap();
    assert!(vowels_out.dfa.matches("xyz") && !vowels_out.dfa.matches("xay"));
    let names = pattern(r"[\i-[:]][\c-[:]]*").unwrap();
    assert!(names.dfa.matches("_a-1") && !names.dfa.matches("a:b") && !names.dfa.matches("1a"));
    assert!(pattern("").unwrap().dfa.matches(""));
    // The same source is the same pattern.
    assert_eq!(pattern("a(b|c)").unwrap().id, p.id);
    assert!(pattern("(").is_err());
    assert!(pattern("(a{1000}){1000}").is_err());
}

#[test]
fn counts_and_strings() {
    let p = pattern("a(b|c)").unwrap();
    assert_eq!(p.dfa.count(&any_length()), Count::exact(2));
    assert_eq!(
        p.dfa.strings(&any_length(), 10).unwrap(),
        ["ab".to_owned(), "ac".to_owned()]
    );
    assert_eq!(p.dfa.count(&lengths(0, Some(1))), Count::ZERO);
    let digits = pattern("[0-9]{3}").unwrap();
    assert_eq!(digits.dfa.count(&any_length()), Count::exact(1000));
    assert!(digits.dfa.strings(&any_length(), 999).is_none());
    let ab = pattern("[ab]*").unwrap();
    assert_eq!(ab.dfa.count(&lengths(0, Some(3))), Count::exact(15));
    assert_eq!(ab.dfa.count(&lengths(2, Some(2))), Count::exact(4));
    assert_eq!(ab.dfa.count(&any_length()), Count::MANY);
    // Far out: at least one string per length the window sees, no exact count.
    let far = ab.dfa.count(&lengths(1 << 40, Some((1 << 40) + 3)));
    assert!(far.lo >= 4 && far.hi == u64::MAX, "{far:?}");
    // Even lengths only, far out too.
    let even = pattern("(aa)*").unwrap();
    let c = even.dfa.count(&lengths(5000, Some(5001)));
    assert_eq!(c.lo, 1, "{c:?}");
    assert_eq!(even.dfa.count(&lengths(3, Some(3))), Count::ZERO);
    assert_eq!(even.dfa.count(&lengths(4, Some(4))), Count::exact(1));
}

#[test]
fn products_and_complements_agree_with_matching() {
    let a = pattern("[ab]*a").unwrap();
    let b = pattern("b[ab]*").unwrap();
    let both = a.dfa.intersection(&b.dfa);
    let neither = a.dfa.complement().intersection(&b.dfa.complement());
    let mut words = vec![String::new()];
    for _ in 0..5 {
        let longer: Vec<String> = words
            .iter()
            .flat_map(|w| ["a", "b", "c"].map(|c| format!("{w}{c}")))
            .collect();
        words.extend(longer);
        words.sort();
        words.dedup();
    }
    for w in &words {
        let (x, y) = (a.dfa.matches(w), b.dfa.matches(w));
        assert_eq!(both.matches(w), x && y, "{w}");
        assert_eq!(neither.matches(w), !x && !y, "{w}");
    }
    // Counted alike: "b…a" over {a, b} of length 3 are bba, baa.
    assert_eq!(both.count(&lengths(3, Some(3))), Count::exact(2));
    // Complements count everything else: 1 + n strings of length 1, n = |Char|.
    let none = Dfa::universal(false).complement();
    let chars = chars::CharSet::all().len();
    assert_eq!(none.count(&lengths(1, Some(1))), Count::exact(chars));
}

#[test]
fn regions_are_the_text_regions() {
    let samples = [
        "",
        "\tx",
        " a",
        "a  b",
        "a b",
        "!",
        "1a",
        "-",
        "a:b",
        ":",
        "_x",
        "abcdefghi",
        "en-US",
        "x",
        "a\u{300}",
        "\u{B7}a",
        "en-",
        "-en",
        "a b ",
        "ab\r",
    ];
    for s in samples {
        let r = text::region(s);
        for q in 0..text::REGIONS {
            assert_eq!(
                Family::Strings.region(q).matches(s),
                q == r,
                "{s:?} in region {r}, automaton {q}"
            );
        }
    }
    // Their counts agree with `text`'s where it knows them exactly.
    for r in 0..text::REGIONS {
        for len in 0..2u64 {
            let (lo, hi) = text::count(r, len);
            if lo == hi {
                let n = len as i128;
                assert_eq!(
                    Family::Strings.region(r).count(&lengths(n, Some(n))),
                    Count::exact(lo),
                    "region {r} length {len}"
                );
            }
        }
    }
}

#[test]
fn language_ranges_filter_as_rfc_4647_says() {
    // RFC 4647 §3.3.2's example.
    let de_de = lang_range("de-*-DE");
    for tag in [
        "de-de",
        "de-latn-de",
        "de-latf-de",
        "de-de-x-goethe",
        "de-latn-de-1996",
        "de-deva-de",
    ] {
        assert!(de_de.dfa.matches(tag), "{tag}");
    }
    for tag in ["de", "de-x-de", "de-deva", "fr-de"] {
        assert!(!de_de.dfa.matches(tag), "{tag}");
    }
    let en = lang_range("en");
    assert!(en.dfa.matches("en") && en.dfa.matches("en-gb") && !en.dfa.matches("eng"));
    assert!(lang_range("*").dfa.matches("zh-hant"));
    assert!(lang_range("EN").matches("x", Some("en-us")));
    assert!(!lang_range("en").matches("x", None));
}

#[test]
fn cells_combine_region_and_patterns() {
    let p = pattern("a(b|c)").unwrap();
    // NCNames that match: ab, ac (not language tags? both are: region 6).
    let tags = cell(Family::Strings, 6, Target::Text, &[(&p, true)]);
    assert_eq!(tags.count(&any_length()), Count::exact(2));
    let ncnames = cell(Family::Strings, 5, Target::Text, &[(&p, true)]);
    assert_eq!(ncnames.count(&any_length()), Count::ZERO);
    let rest = cell(Family::Strings, 6, Target::Text, &[(&p, false)]);
    assert_eq!(rest.count(&lengths(2, Some(2))), Count::exact(52 * 52 - 2));
}
