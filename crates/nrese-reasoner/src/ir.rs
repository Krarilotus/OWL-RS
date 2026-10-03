//! The rule IR (reasoner-v2 design §3.1): rules are data.
//!
//! A [`Rule`] joins triple-pattern atoms, checks guards and derives facts (or reports an
//! inconsistency). Terms are variables or constant ids; all reasoning happens on ids.
//!
//! Rulesets are written in a small text syntax ([`parse_rules`]) and resolved against a
//! [`Vocabulary`], which interns the IRIs and literals the rules mention:
//!
//! ```text
//! # comment
//! cax-sco: (?c1 rdfs:subClassOf ?c2), (?x rdf:type ?c1) -> (?x rdf:type ?c2)
//! prp-fp:  (?p rdf:type owl:FunctionalProperty), (?x ?p ?y1), (?x ?p ?y2), ?y1 != ?y2
//!          -> (?y1 owl:sameAs ?y2)
//! cax-dw:  (?c1 owl:disjointWith ?c2), (?x rdf:type ?c1), (?x rdf:type ?c2) -> false
//! ```
//!
//! Prefixes `rdf:`, `rdfs:`, `owl:` and `xsd:` are built in; literals are written
//! `"lexical"^^xsd:type`. A rule ends at the next line starting with a name and `:`.

use std::fmt;

pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const OWL: &str = "http://www.w3.org/2002/07/owl#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Interns the constants rules mention. The store implementation interns into the
/// engine's dictionary; tests use a local one.
pub trait Vocabulary {
    fn iri(&mut self, iri: &str) -> u64;
    fn literal(&mut self, lexical: &str, datatype: &str) -> u64;
    /// A language-tagged string (user rules may mention one).
    fn language_literal(&mut self, lexical: &str, language: &str) -> u64;
    /// The ids blank nodes take, `low..=high`, where they are one range: GraphDB's
    /// `[Constraint x != blank_node]` ([`Guard::NotIn`]). `None`: rules can't test it.
    fn blank_node_ids(&self) -> Option<(u64, u64)> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Term {
    Var(u8),
    Const(u64),
}

/// A triple pattern `(subject predicate object)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(pub [Term; 3]);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Guard {
    /// The two terms must be bound to different ids.
    NotEqual(Term, Term),
    /// The term's id must lie outside `low..=high` (a kind of term: blank nodes).
    NotIn(Term, u64, u64),
    /// The two terms must be members of one list of the index, the first's id below the
    /// second's (each pair once): a long n-ary axiom (`owl:AllDifferent` over thousands of
    /// individuals) as one rule whose pairs are checked here, instead of a rule per pair
    /// ([`super::lists`]; its rules are symmetric in the two terms).
    SameList(Term, Term, SharedListIndex),
}

impl Guard {
    /// Whether the guard holds for the terms' values (`None`: unbound, decided later).
    pub fn holds(&self, value: impl Fn(Term) -> Option<u64>) -> bool {
        match self {
            Guard::NotEqual(a, b) => match (value(*a), value(*b)) {
                (Some(a), Some(b)) => a != b,
                _ => true,
            },
            Guard::NotIn(term, low, high) => value(*term).is_none_or(|v| v < *low || v > *high),
            Guard::SameList(a, b, index) => match (value(*a), value(*b)) {
                (Some(a), Some(b)) => a < b && index.together(a, b),
                _ => true,
            },
        }
    }
}

/// Which lists each term is a member of, for [`Guard::SameList`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListIndex {
    /// Each member's list numbers, sorted.
    lists: std::collections::HashMap<u64, Vec<u32>>,
    count: u32,
}

impl ListIndex {
    /// Adds a list of `members`.
    pub fn add(&mut self, members: &[u64]) {
        let list = self.count;
        self.count += 1;
        for &member in members {
            let lists = self.lists.entry(member).or_default();
            if lists.last() != Some(&list) {
                lists.push(list);
            }
        }
    }

    /// Whether `a` and `b` are members of one list.
    pub fn together(&self, a: u64, b: u64) -> bool {
        let (Some(x), Some(y)) = (self.lists.get(&a), self.lists.get(&b)) else {
            return false;
        };
        let (mut i, mut j) = (0, 0);
        while i < x.len() && j < y.len() {
            match x[i].cmp(&y[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// A [`ListIndex`] shared by the rules that ask it; compared by its contents, so that
/// the rule instantiated again from the same lists (a commit that touches other list
/// facts re-instantiates every list rule) is known as the same rule, not added again.
#[derive(Debug, Clone)]
pub struct SharedListIndex {
    index: std::sync::Arc<ListIndex>,
    /// A hash of the contents, computed once.
    fingerprint: u64,
}

impl SharedListIndex {
    pub fn new(index: ListIndex) -> Self {
        use std::hash::{Hash, Hasher};
        let mut members: Vec<(&u64, &Vec<u32>)> = index.lists.iter().collect();
        members.sort_unstable();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (index.count, members).hash(&mut hasher);
        Self {
            fingerprint: hasher.finish(),
            index: std::sync::Arc::new(index),
        }
    }

    pub fn together(&self, a: u64, b: u64) -> bool {
        self.index.together(a, b)
    }
}

impl PartialEq for SharedListIndex {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.index, &other.index)
            || (self.fingerprint == other.fingerprint && self.index == other.index)
    }
}

impl Eq for SharedListIndex {}

impl std::hash::Hash for SharedListIndex {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.fingerprint.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Head {
    /// Facts to derive.
    Facts(Vec<Atom>),
    /// A consistency violation (OWL 2 RL rules with a `false` head).
    Inconsistent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The W3C OWL 2 RL/RDF rule name (`cax-sco`, `prp-fp`, …) or a generated name.
    pub name: String,
    pub body: Vec<Atom>,
    pub guards: Vec<Guard>,
    pub head: Head,
}

impl Rule {
    /// Number of distinct variables (they are numbered densely from 0).
    pub fn variables(&self) -> usize {
        let mut max = None;
        let atoms = self.body.iter().chain(match &self.head {
            Head::Facts(atoms) => atoms.as_slice(),
            Head::Inconsistent => &[],
        });
        for atom in atoms {
            for term in atom.0 {
                if let Term::Var(v) = term {
                    max = max.max(Some(v));
                }
            }
        }
        max.map_or(0, |m| usize::from(m) + 1)
    }
}

/// A fact: subject, predicate and object ids.
pub type Triple = [u64; 3];

/// A consistency rule that fired: its name and the bindings of its variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Violation {
    pub rule: String,
    pub bindings: Vec<u64>,
}

/// A rule text that doesn't compile: which rule, what is wrong, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub rule: String,
    pub message: String,
    /// Where in the text, where known: the mistake's, else the start of its rule.
    pub position: Option<Position>,
}

/// A place in a text: line and column, both from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl ParseError {
    pub fn new(rule: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            message: message.into(),
            position: None,
        }
    }

    /// At line `line` (from 1), column `column` (from 1).
    pub fn at(mut self, line: usize, column: usize) -> Self {
        self.position = Some(Position { line, column });
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(Position { line, column }) = self.position {
            write!(f, "line {line}, column {column}: ")?;
        }
        match self.rule.is_empty() {
            true => write!(f, "{}", self.message),
            false => write!(f, "rule {}: {}", self.rule, self.message),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses rules in the syntax above, interning constants through `vocabulary`.
pub fn parse_rules(text: &str, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
    parse_sources(rule_sources(text)?, vocabulary)
}

/// One rule's text: its name, its source (continuation lines joined) and the line it
/// starts on, for its errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleSource {
    pub name: String,
    pub source: String,
    pub line: usize,
}

/// The rules of `text`, unparsed.
pub(crate) fn rule_sources(text: &str) -> Result<Vec<RuleSource>, ParseError> {
    // Join continuation lines: a rule starts with `name:` at the beginning of a line.
    let mut sources: Vec<(String, String, usize)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default();
        if line.trim().is_empty() {
            continue;
        }
        let starts_rule = !line.starts_with(char::is_whitespace)
            && line
                .split_once(':')
                .is_some_and(|(name, _)| !name.contains(' ') && !name.contains('('));
        if starts_rule {
            let (name, rest) = line.split_once(':').expect("checked above");
            sources.push((name.trim().to_owned(), rest.to_owned(), number + 1));
        } else if let Some((_, body, _)) = sources.last_mut() {
            body.push(' ');
            body.push_str(line.trim());
        } else {
            let column = line.len() - line.trim_start().len() + 1;
            return Err(ParseError::new(
                "",
                format!("text before the first rule: {}", line.trim()),
            )
            .at(number + 1, column));
        }
    }
    Ok(sources
        .into_iter()
        .map(|(name, source, line)| RuleSource { name, source, line })
        .collect())
}

pub(crate) fn parse_sources(
    sources: Vec<RuleSource>,
    vocabulary: &mut impl Vocabulary,
) -> Result<Vec<Rule>, ParseError> {
    sources
        .into_iter()
        .map(|RuleSource { name, source, line }| {
            parse_rule(&name, &source, vocabulary)
                .map_err(|message| ParseError::new(name, message).at(line, 1))
        })
        .collect()
}

fn parse_rule(name: &str, source: &str, vocabulary: &mut impl Vocabulary) -> Result<Rule, String> {
    let (body_text, head_text) = source.split_once("->").ok_or("missing ->")?;
    let mut variables: Vec<String> = Vec::new();
    let mut body = Vec::new();
    let mut guards = Vec::new();
    for part in split_top_level(body_text) {
        if let Some((a, b)) = part.split_once("!=") {
            guards.push(Guard::NotEqual(
                term(a.trim(), &mut variables, vocabulary)?,
                term(b.trim(), &mut variables, vocabulary)?,
            ));
        } else {
            body.push(atom(&part, &mut variables, vocabulary)?);
        }
    }
    let head = if head_text.trim() == "false" {
        Head::Inconsistent
    } else {
        let atoms = split_top_level(head_text)
            .iter()
            .map(|part| atom(part, &mut variables, vocabulary))
            .collect::<Result<Vec<_>, _>>()?;
        // Safety: every head variable must be bound by the body.
        let bound: Vec<Term> = body.iter().flat_map(|a: &Atom| a.0).collect();
        for a in &atoms {
            for t in a.0 {
                if matches!(t, Term::Var(_)) && !bound.contains(&t) {
                    return Err("a head variable isn't bound by the body".to_owned());
                }
            }
        }
        Head::Facts(atoms)
    };
    Ok(Rule {
        name: name.to_owned(),
        body,
        guards,
        head,
    })
}

/// Splits on commas outside parentheses and quotes.
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut quoted, mut current) = (0, false, String::new());
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(std::mem::take(&mut current).trim().to_owned());
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_owned());
    }
    parts
}

/// A ground triple `s p o` of prefixed names or typed literals, as ids.
pub fn parse_triple(text: &str, vocabulary: &mut impl Vocabulary) -> Result<[u64; 3], String> {
    let parts = tokens(text);
    let [s, p, o] = parts.as_slice() else {
        return Err(format!("expected three terms: {text}"));
    };
    let mut variables = Vec::new();
    let mut constant = |t: &str| match term(t, &mut variables, vocabulary)? {
        Term::Const(id) => Ok(id),
        Term::Var(_) => Err(format!("a ground triple has no variables: {text}")),
    };
    Ok([constant(s)?, constant(p)?, constant(o)?])
}

fn atom(
    text: &str,
    variables: &mut Vec<String>,
    vocabulary: &mut impl Vocabulary,
) -> Result<Atom, String> {
    let inner = text
        .trim()
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .ok_or_else(|| format!("expected (s p o): {text}"))?;
    let tokens = tokens(inner);
    if tokens.len() != 3 {
        return Err(format!("expected three terms: {text}"));
    }
    Ok(Atom([
        term(&tokens[0], variables, vocabulary)?,
        term(&tokens[1], variables, vocabulary)?,
        term(&tokens[2], variables, vocabulary)?,
    ]))
}

/// Whitespace-separated tokens; quoted literals stay whole.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut quoted, mut current) = (false, String::new());
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn expand(prefixed: &str) -> Result<String, String> {
    let (prefix, local) = prefixed
        .split_once(':')
        .ok_or_else(|| format!("not a prefixed name: {prefixed}"))?;
    let namespace = match prefix {
        "rdf" => RDF,
        "rdfs" => RDFS,
        "owl" => OWL,
        "xsd" => XSD,
        other => return Err(format!("unknown prefix {other}:")),
    };
    Ok(format!("{namespace}{local}"))
}

fn term(
    text: &str,
    variables: &mut Vec<String>,
    vocabulary: &mut impl Vocabulary,
) -> Result<Term, String> {
    if let Some(name) = text.strip_prefix('?') {
        let index = match variables.iter().position(|v| v == name) {
            Some(i) => i,
            None => {
                variables.push(name.to_owned());
                variables.len() - 1
            }
        };
        return u8::try_from(index)
            .map(Term::Var)
            .map_err(|_| "too many variables".to_owned());
    }
    if let Some(rest) = text.strip_prefix('"') {
        let (lexical, datatype) = rest
            .split_once("\"^^")
            .ok_or_else(|| format!("expected \"…\"^^type: {text}"))?;
        return Ok(Term::Const(vocabulary.literal(lexical, &expand(datatype)?)));
    }
    Ok(Term::Const(vocabulary.iri(&expand(text)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocabulary::LocalVocabulary;

    #[test]
    fn errors_name_the_line_of_their_rule() {
        let mut vocabulary = LocalVocabulary::default();
        let error = parse_rules(
            "# rules\n\
             ok: (?x rdf:type ?y) -> (?y rdf:type ?x)\n\
             \n\
             broken: (?x rdf:type ?y)\n\
             \x20   (?y rdf:type ?x)\n",
            &mut vocabulary,
        )
        .unwrap_err();
        assert_eq!(error.rule, "broken");
        assert_eq!(error.position, Some(Position { line: 4, column: 1 }));
        assert!(
            error
                .to_string()
                .starts_with("line 4, column 1: rule broken: "),
            "{error}"
        );
        let error = parse_rules("\n   stray text\n", &mut vocabulary).unwrap_err();
        assert_eq!(error.position, Some(Position { line: 2, column: 4 }));
    }

    #[test]
    fn parses_rules_with_guards_continuations_and_false_heads() {
        let mut vocabulary = LocalVocabulary::default();
        let rules = parse_rules(
            "# a comment\n\
             prp-fp: (?p rdf:type owl:FunctionalProperty), (?x ?p ?y1), (?x ?p ?y2), ?y1 != ?y2\n\
             \x20   -> (?y1 owl:sameAs ?y2)\n\
             cax-dw: (?c1 owl:disjointWith ?c2), (?x rdf:type ?c1), (?x rdf:type ?c2) -> false\n\
             cls-maxc2: (?x owl:maxCardinality \"1\"^^xsd:nonNegativeInteger) -> (?x rdf:type owl:Class)\n",
            &mut vocabulary,
        )
        .unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].name, "prp-fp");
        assert_eq!(rules[0].body.len(), 3);
        assert_eq!(
            rules[0].guards,
            vec![Guard::NotEqual(Term::Var(2), Term::Var(3))]
        );
        assert_eq!(rules[0].variables(), 4);
        assert_eq!(rules[1].head, Head::Inconsistent);
        let literal = vocabulary.literal("1", &format!("{XSD}nonNegativeInteger"));
        assert_eq!(rules[2].body[0].0[2], Term::Const(literal));
    }

    #[test]
    fn rejects_unsafe_and_malformed_rules() {
        let mut vocabulary = LocalVocabulary::default();
        for bad in [
            "r: (?x rdf:type ?c) -> (?y rdf:type ?c)",
            "r: (?x rdf:type) -> (?x rdf:type owl:Thing)",
            "r: (?x foo:bar ?y) -> (?x rdf:type ?y)",
            "r: (?x rdf:type ?y)",
        ] {
            assert!(parse_rules(bad, &mut vocabulary).is_err(), "{bad}");
        }
    }
}
