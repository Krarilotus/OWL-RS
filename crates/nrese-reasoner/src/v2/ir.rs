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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Term {
    Var(u8),
    Const(u64),
}

/// A triple pattern `(subject predicate object)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(pub [Term; 3]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Guard {
    /// The two terms must be bound to different ids.
    NotEqual(Term, Term),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub rule: String,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rule {}: {}", self.rule, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parses rules in the syntax above, interning constants through `vocabulary`.
pub fn parse_rules(text: &str, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
    // Join continuation lines: a rule starts with `name:` at the beginning of a line.
    let mut sources: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
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
            sources.push((name.trim().to_owned(), rest.to_owned()));
        } else if let Some((_, body)) = sources.last_mut() {
            body.push(' ');
            body.push_str(line.trim());
        } else {
            return Err(ParseError {
                rule: String::new(),
                message: format!("text before the first rule: {line}"),
            });
        }
    }
    sources
        .into_iter()
        .map(|(name, source)| {
            parse_rule(&name, &source, vocabulary).map_err(|message| ParseError {
                rule: name,
                message,
            })
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
    use crate::v2::testing::LocalVocabulary;

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
