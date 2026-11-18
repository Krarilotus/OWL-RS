//! Regular sets of strings, for the facets that are patterns: `xsd:pattern` (XML Schema
//! regular expressions, on the text of strings and IRIs) and `rdf:langRange` (RFC 4647
//! extended filtering, on language tags).
//!
//! - [`syntax`]: the expressions, parsed.
//! - [`chars`]: sets of characters, with XML's and Unicode's classes.
//! - [`dfa`]: minimal automata, combined, counted and enumerated.
//!
//! A [`Pattern`] is compiled once per distinct source and shared. The string regions of
//! `text` are regular too: a cell of a set of strings (a region and which patterns hold)
//! is the product of their automata, built when a count needs it and kept.

mod blocks;
mod chars;
mod dfa;
mod syntax;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) use chars::{NAME_MORE, NAME_START, in_table};
pub(crate) use dfa::Dfa;
use syntax::Ast;
pub use syntax::PatternError;

use super::text::REGIONS;

/// What a pattern constrains: the text, or the language tag of a tagged string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Target {
    Text,
    Tag,
}

/// A compiled pattern.
pub(crate) struct Pattern {
    /// Distinct per (target, source) in the process: the order sets align patterns by.
    pub(crate) id: u32,
    pub(crate) source: String,
    pub(crate) target: Target,
    pub(crate) dfa: Dfa,
}

impl PartialEq for Pattern {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
    }
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} {:?}", self.target, self.source)
    }
}

impl Pattern {
    pub(crate) fn matches(&self, text: &str, tag: Option<&str>) -> bool {
        match self.target {
            Target::Text => self.dfa.matches(text),
            Target::Tag => tag.is_some_and(|t| self.dfa.matches(t)),
        }
    }
}

#[derive(Default)]
struct Registry {
    patterns: HashMap<(Target, String), Arc<Pattern>>,
    cells: HashMap<CellKey, Arc<Dfa>>,
}

/// A cell's automaton: the region (of a family) and, per pattern, whether it holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CellKey {
    family: Family,
    region: usize,
    target: Target,
    patterns: Vec<(u32, bool)>,
}

/// Cells kept at most; past it the cache starts over.
const MAX_CELLS: usize = 4096;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

fn intern(
    target: Target,
    source: &str,
    build: impl FnOnce() -> Result<Dfa, PatternError>,
) -> Result<Arc<Pattern>, PatternError> {
    let key = (target, source.to_owned());
    if let Some(p) = lock().patterns.get(&key) {
        return Ok(p.clone());
    }
    let dfa = build()?;
    let mut registry = lock();
    let id = registry.patterns.len() as u32;
    Ok(registry
        .patterns
        .entry(key)
        .or_insert_with(|| {
            Arc::new(Pattern {
                id,
                source: source.to_owned(),
                target,
                dfa,
            })
        })
        .clone())
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// An `xsd:pattern`.
pub(crate) fn pattern(source: &str) -> Result<Arc<Pattern>, PatternError> {
    intern(Target::Text, source, || {
        Dfa::from_ast(&syntax::parse(source)?)
    })
}

/// An `rdf:langRange`: the (lower-case) language tags it matches under RFC 4647's extended
/// filtering (§3.3.2).
pub(crate) fn lang_range(range: &str) -> Arc<Pattern> {
    let range = range.to_ascii_lowercase();
    let built = intern(Target::Tag, &range, || {
        Dfa::from_ast(&lang_range_ast(&range))
    });
    match built {
        Ok(p) => p,
        // The expression is small and well-formed by construction.
        Err(e) => unreachable!("a language range's automaton: {e}"),
    }
}

/// Extended filtering as an expression: the first subtags equal (or the range's is `*`);
/// each further subtag of the range but `*` is found in order, skipping only subtags that
/// aren't singletons; whatever follows the last one is free.
fn lang_range_ast(range: &str) -> Ast {
    let alnum = || chars::CharSet::from_ranges([(0x30, 0x39), (0x61, 0x7A)]);
    let subtag = |min| Ast::Repeat(Box::new(Ast::Class(alnum())), min, Some(8));
    let hyphen = || Ast::Class(chars::CharSet::single('-'));
    let literal = |s: &str| {
        Ast::Concat(
            s.chars()
                .map(|c| Ast::Class(chars::CharSet::single(c)))
                .collect(),
        )
    };
    let mut parts = range.split('-');
    let first = parts.next().unwrap_or("");
    let mut seq = vec![if first == "*" {
        Ast::Repeat(
            Box::new(Ast::Class(chars::CharSet::from_ranges([(0x61, 0x7A)]))),
            1,
            Some(8),
        )
    } else {
        literal(first)
    }];
    for part in parts.filter(|p| *p != "*") {
        seq.push(Ast::Repeat(
            Box::new(Ast::Concat(vec![hyphen(), subtag(2)])),
            0,
            None,
        ));
        seq.push(hyphen());
        seq.push(literal(part));
    }
    seq.push(Ast::Repeat(
        Box::new(Ast::Concat(vec![hyphen(), subtag(1)])),
        0,
        None,
    ));
    Ast::Concat(seq)
}

/// The kinds of text a set holds, by their regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Family {
    /// Strings, in `text`'s seven regions.
    Strings,
    /// IRIs: one region, any text.
    Iris,
    /// Language tags (lower case): one region.
    Tags,
}

impl Family {
    pub(crate) fn regions(self) -> usize {
        match self {
            Family::Strings => REGIONS,
            Family::Iris | Family::Tags => 1,
        }
    }

    fn region(self, r: usize) -> &'static Dfa {
        static STRINGS: OnceLock<Vec<Dfa>> = OnceLock::new();
        static ANY: OnceLock<Dfa> = OnceLock::new();
        static TAGS: OnceLock<Dfa> = OnceLock::new();
        match self {
            Family::Strings => &STRINGS.get_or_init(string_regions)[r],
            Family::Iris => ANY.get_or_init(|| Dfa::universal(true)),
            Family::Tags => TAGS.get_or_init(|| compile(r"[a-z]{1,8}(-[a-z0-9]{1,8})*")),
        }
    }
}

fn compile(source: &str) -> Dfa {
    match syntax::parse(source).and_then(|ast| Dfa::from_ast(&ast)) {
        Ok(dfa) => dfa,
        Err(e) => unreachable!("a built-in expression: {e}"),
    }
}

/// `text`'s regions as automata: each type of the chain less the next.
fn string_regions() -> Vec<Dfa> {
    let chain = [
        Dfa::universal(true),
        compile(r"[^\t\n\r]*"),
        compile(r"([^\s]+( [^\s]+)*)?"),
        compile(r"\c+"),
        compile(r"\i\c*"),
        compile(r"[\i-[:]][\c-[:]]*"),
        compile(r"[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*"),
    ];
    (0..REGIONS)
        .map(|r| match chain.get(r + 1) {
            Some(narrower) => chain[r].intersection(&narrower.complement()),
            None => chain[r].clone(),
        })
        .collect()
}

/// The automaton of one cell: region `region` of `family`, where each of `patterns` (all
/// of one target) holds or not as given.
pub(crate) fn cell(
    family: Family,
    region: usize,
    target: Target,
    patterns: &[(&Arc<Pattern>, bool)],
) -> Arc<Dfa> {
    let key = CellKey {
        family,
        region,
        target,
        patterns: patterns.iter().map(|(p, holds)| (p.id, *holds)).collect(),
    };
    if let Some(dfa) = lock().cells.get(&key) {
        return dfa.clone();
    }
    let mut dfa = family.region(region).clone();
    for (p, holds) in patterns {
        dfa = if *holds {
            dfa.intersection(&p.dfa)
        } else {
            dfa.intersection(&p.dfa.complement())
        };
    }
    let dfa = Arc::new(dfa);
    let mut registry = lock();
    if registry.cells.len() >= MAX_CELLS {
        registry.cells.clear();
    }
    registry.cells.insert(key, dfa.clone());
    dfa
}

#[cfg(test)]
mod tests;
