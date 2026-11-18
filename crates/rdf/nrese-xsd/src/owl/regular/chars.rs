//! Sets of characters as sorted, disjoint, non-adjacent ranges of code points, within
//! XML 1.0's `Char` (what a string value may hold): the classes of a pattern, and the
//! symbols of an automaton.

use super::blocks::BLOCKS;

/// XML 1.0 `Char`: `#x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] | [#x10000-#x10FFFF]`.
const CHAR: &[(u32, u32)] = &[
    (0x9, 0xA),
    (0xD, 0xD),
    (0x20, 0xD7FF),
    (0xE000, 0xFFFD),
    (0x10000, 0x10FFFF),
];

/// `NameStartChar` of XML 1.0 fifth edition: `\i`.
pub(crate) const NAME_START: &[(u32, u32)] = &[
    (0x3A, 0x3A),
    (0x41, 0x5A),
    (0x5F, 0x5F),
    (0x61, 0x7A),
    (0xC0, 0xD6),
    (0xD8, 0xF6),
    (0xF8, 0x2FF),
    (0x370, 0x37D),
    (0x37F, 0x1FFF),
    (0x200C, 0x200D),
    (0x2070, 0x218F),
    (0x2C00, 0x2FEF),
    (0x3001, 0xD7FF),
    (0xF900, 0xFDCF),
    (0xFDF0, 0xFFFD),
    (0x10000, 0xEFFFF),
];

/// What `NameChar` adds to `NameStartChar`: with it, `\c`.
pub(crate) const NAME_MORE: &[(u32, u32)] = &[
    (0x2D, 0x2E),
    (0x30, 0x39),
    (0xB7, 0xB7),
    (0x300, 0x36F),
    (0x203F, 0x2040),
];

/// Whether `c` is in a sorted table of ranges.
pub(crate) fn in_table(table: &[(u32, u32)], c: char) -> bool {
    let c = u32::from(c);
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// A set of characters (of `Char`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(crate) struct CharSet(Vec<(u32, u32)>);

impl CharSet {
    pub(crate) fn empty() -> Self {
        Self(Vec::new())
    }

    /// Every `Char`.
    pub(crate) fn all() -> Self {
        Self(CHAR.to_vec())
    }

    pub(crate) fn single(c: char) -> Self {
        Self::from_ranges([(u32::from(c), u32::from(c))])
    }

    /// The characters of ranges in any order, overlapping or not, cut to `Char`.
    pub(crate) fn from_ranges(ranges: impl IntoIterator<Item = (u32, u32)>) -> Self {
        let mut v: Vec<(u32, u32)> = ranges.into_iter().filter(|(a, b)| a <= b).collect();
        v.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(v.len());
        for (lo, hi) in v {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        Self(merged).intersection(&Self(CHAR.to_vec()))
    }

    pub(crate) fn ranges(&self) -> &[(u32, u32)] {
        &self.0
    }

    pub(crate) fn contains(&self, c: char) -> bool {
        in_table(&self.0, c)
    }

    /// How many characters it has.
    pub(crate) fn len(&self) -> u64 {
        self.0.iter().map(|&(a, b)| u64::from(b - a) + 1).sum()
    }

    pub(crate) fn union(&self, o: &Self) -> Self {
        Self::from_ranges(self.0.iter().chain(&o.0).copied())
    }

    pub(crate) fn intersection(&self, o: &Self) -> Self {
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < self.0.len() && j < o.0.len() {
            let (a, b) = self.0[i];
            let (c, d) = o.0[j];
            let (lo, hi) = (a.max(c), b.min(d));
            if lo <= hi {
                out.push((lo, hi));
            }
            if b < d {
                i += 1;
            } else {
                j += 1;
            }
        }
        Self(out)
    }

    /// The `Char`s not in it.
    pub(crate) fn complement(&self) -> Self {
        let mut out = Vec::new();
        let mut next = 0u32;
        for &(a, b) in &self.0 {
            if a > next {
                out.push((next, a - 1));
            }
            next = b.saturating_add(1);
        }
        if next <= 0x10FFFF {
            out.push((next, 0x10FFFF));
        }
        Self(out).intersection(&Self(CHAR.to_vec()))
    }

    pub(crate) fn minus(&self, o: &Self) -> Self {
        self.intersection(&o.complement())
    }

    /// `\s`: space, tab, newline, carriage return.
    pub(crate) fn space() -> Self {
        Self::from_ranges([(0x9, 0xA), (0xD, 0xD), (0x20, 0x20)])
    }

    /// `\i`.
    pub(crate) fn name_start() -> Self {
        Self::from_ranges(NAME_START.iter().copied())
    }

    /// `\c`.
    pub(crate) fn name_char() -> Self {
        Self::from_ranges(NAME_START.iter().chain(NAME_MORE).copied())
    }

    /// `.`: every character but newline and carriage return.
    pub(crate) fn dot() -> Self {
        Self::from_ranges([(0xA, 0xA), (0xD, 0xD)]).complement()
    }

    /// `\d`: decimal digits (`\p{Nd}`).
    pub(crate) fn digit() -> Self {
        Self::category("Nd").unwrap_or_default()
    }

    /// `\w`: every character but punctuation, separators and "other" (`[^\p{P}\p{Z}\p{C}]`).
    pub(crate) fn word() -> Self {
        ["P", "Z", "C"]
            .iter()
            .filter_map(|c| Self::category(c))
            .fold(Self::empty(), |a, b| a.union(&b))
            .complement()
    }

    /// `\p{name}`: a general category (`L`, `Lu`, …) or a block (`IsBasicLatin`, …).
    pub(crate) fn property(name: &str) -> Option<Self> {
        match name.strip_prefix("Is") {
            Some(block) => Self::block(block),
            None => Self::category(name),
        }
    }

    fn category(name: &str) -> Option<Self> {
        let known = matches!(
            name,
            "L" | "Lu"
                | "Ll"
                | "Lt"
                | "Lm"
                | "Lo"
                | "M"
                | "Mn"
                | "Mc"
                | "Me"
                | "N"
                | "Nd"
                | "Nl"
                | "No"
                | "P"
                | "Pc"
                | "Pd"
                | "Ps"
                | "Pe"
                | "Pi"
                | "Pf"
                | "Po"
                | "Z"
                | "Zs"
                | "Zl"
                | "Zp"
                | "S"
                | "Sm"
                | "Sc"
                | "Sk"
                | "So"
                | "C"
                | "Cc"
                | "Cf"
                | "Co"
                | "Cn"
        );
        if !known {
            return None;
        }
        let hir = regex_syntax::ParserBuilder::new()
            .unicode(true)
            .utf8(false)
            .build()
            .parse(&format!("\\p{{{name}}}"))
            .ok()?;
        match hir.kind() {
            regex_syntax::hir::HirKind::Class(regex_syntax::hir::Class::Unicode(class)) => {
                Some(Self::from_ranges(
                    class
                        .iter()
                        .map(|r| (u32::from(r.start()), u32::from(r.end()))),
                ))
            }
            _ => None,
        }
    }

    fn block(name: &str) -> Option<Self> {
        // XML Schema 1.0's names where Unicode renamed the block since.
        let name = match name {
            "Greek" => "GreekandCoptic",
            "CombiningMarksforSymbols" => "CombiningDiacriticalMarksforSymbols",
            n => n,
        };
        let i = BLOCKS.binary_search_by(|(n, _, _)| (*n).cmp(name)).ok()?;
        let (_, lo, hi) = BLOCKS[i];
        Some(Self::from_ranges([(lo, hi)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_combine_within_char() {
        let a = CharSet::from_ranges([(0x61, 0x7A), (0x41, 0x5A)]);
        assert_eq!(a.len(), 52);
        assert!(a.contains('q') && !a.contains('1'));
        let not_a = a.complement();
        assert!(!not_a.contains('q') && not_a.contains('1'));
        // Complements stay within Char: no surrogates, no U+0, no U+FFFE.
        assert!(!not_a.contains('\u{0}') && !not_a.contains('\u{FFFE}'));
        assert_eq!(not_a.complement(), a);
        assert_eq!(a.union(&not_a), CharSet::all());
        assert_eq!(a.intersection(&not_a).len(), 0);
        assert_eq!(a.minus(&CharSet::single('a')).len(), 51);
    }

    #[test]
    fn properties_and_escapes() {
        let lu = CharSet::property("Lu").unwrap();
        assert!(lu.contains('A') && !lu.contains('a'));
        assert!(CharSet::property("IsBasicLatin").unwrap().contains('~'));
        assert!(
            !CharSet::property("IsBasicLatin")
                .unwrap()
                .contains('\u{E9}')
        );
        assert!(CharSet::property("IsGreek").unwrap().contains('\u{3B1}'));
        assert!(CharSet::property("IsNoSuchBlock").is_none());
        assert!(CharSet::property("Xx").is_none());
        assert!(CharSet::digit().contains('7') && CharSet::digit().contains('\u{663}'));
        assert!(CharSet::word().contains('a') && !CharSet::word().contains(' '));
        assert!(!CharSet::word().contains('.'));
        assert!(CharSet::name_start().contains(':') && !CharSet::name_start().contains('-'));
        assert!(CharSet::name_char().contains('-'));
        assert!(!CharSet::dot().contains('\n') && CharSet::dot().contains('\t'));
    }
}
