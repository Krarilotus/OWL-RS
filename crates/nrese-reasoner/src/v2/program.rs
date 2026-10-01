//! What a store materialises: a built-in ruleset, the user's rules (Notation3, compiled by
//! [`super::n3`], or a GraphDB ruleset, compiled by [`super::pie`]), or both, the user's
//! on top.
//!
//! A program's [`RuleProgram::name`] and [`RuleProgram::fingerprint`] identify what it
//! derives; the store records them with the inferred stack, so changing the rules (a
//! single character of the rules file) rebuilds it rather than trusting a closure that
//! another program computed.

use std::sync::Arc;

use super::ir::{ParseError, Rule, Vocabulary};
use super::n3::{self, N3Program};
use super::rulesets::{Ruleset, SEMANTICS_VERSION};
use super::testing::LocalVocabulary;

/// User rules: their name (the file's, usually), their text, and its format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRules {
    name: String,
    text: String,
    format: RuleFormat,
}

/// The languages user rules are read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFormat {
    /// Notation3 ([`super::n3`]).
    N3,
    /// A GraphDB ruleset, `.pie` ([`super::pie`]).
    Pie,
}

impl UserRules {
    /// N3 rules, checked now: a mistake is an error at startup, not at the first commit.
    pub fn n3(name: impl Into<String>, text: impl Into<String>) -> Result<Self, ParseError> {
        Self::new(name, text, RuleFormat::N3)
    }

    /// A GraphDB ruleset (`.pie`), checked now.
    pub fn pie(name: impl Into<String>, text: impl Into<String>) -> Result<Self, ParseError> {
        Self::new(name, text, RuleFormat::Pie)
    }

    fn new(
        name: impl Into<String>,
        text: impl Into<String>,
        format: RuleFormat,
    ) -> Result<Self, ParseError> {
        let rules = Self {
            name: name.into(),
            text: text.into(),
            format,
        };
        rules.compile(&mut LocalVocabulary::default())?;
        Ok(rules)
    }

    pub fn format(&self) -> RuleFormat {
        self.format
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn compile(&self, vocabulary: &mut impl Vocabulary) -> Result<N3Program, ParseError> {
        match self.format {
            RuleFormat::N3 => n3::compile(&self.name, &self.text, vocabulary),
            RuleFormat::Pie => super::pie::compile(&self.name, &self.text, vocabulary),
        }
    }
}

/// A built-in ruleset, user rules, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleProgram {
    base: Option<Ruleset>,
    user: Option<Arc<UserRules>>,
}

impl RuleProgram {
    /// The program of `base` and `user`; `None` if there are neither.
    pub fn new(base: Option<Ruleset>, user: Option<Arc<UserRules>>) -> Option<Self> {
        (base.is_some() || user.is_some()).then_some(Self { base, user })
    }

    pub fn builtin(ruleset: Ruleset) -> Self {
        Self {
            base: Some(ruleset),
            user: None,
        }
    }

    pub fn base(&self) -> Option<Ruleset> {
        self.base
    }

    pub fn user(&self) -> Option<&UserRules> {
        self.user.as_deref()
    }

    /// `owl2-rl`, `custom:rules.n3`, or `owl2-rl+custom:rules.n3`.
    pub fn name(&self) -> String {
        match (self.base, &self.user) {
            (Some(base), None) => base.name().to_owned(),
            (None, Some(user)) => format!("custom:{}", user.name),
            (Some(base), Some(user)) => format!("{}+custom:{}", base.name(), user.name),
            (None, None) => unreachable!("a program has rules"),
        }
    }

    /// Identifies what the program derives: the built-in ruleset's fingerprint, and
    /// FNV-1a over the user rules' text. Stable across builds and platforms.
    pub fn fingerprint(&self) -> u64 {
        let Some(user) = &self.user else {
            return self.base.map_or(0, Ruleset::fingerprint);
        };
        let mut hash: u64 = self
            .base
            .map_or(0xcbf2_9ce4_8422_2325, Ruleset::fingerprint);
        let mut feed = |bytes: &[u8]| {
            for &byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        feed(&SEMANTICS_VERSION.to_le_bytes());
        feed(b"custom:");
        feed(user.text.as_bytes());
        hash
    }

    /// The rules: the built-in ruleset's, then the user's.
    pub fn rules(&self, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
        let mut rules = match self.base {
            Some(base) => base.rules(vocabulary)?,
            None => Vec::new(),
        };
        if let Some(user) = &self.user {
            rules.extend(user.compile(vocabulary)?.rules);
        }
        Ok(rules)
    }

    /// The statements that hold whatever the data: the built-in ruleset's axioms and the
    /// facts of the user's rules.
    pub fn axiom_triples(
        &self,
        vocabulary: &mut impl Vocabulary,
    ) -> Result<Vec<[u64; 3]>, ParseError> {
        let mut axioms = match self.base {
            Some(base) => base.axiom_triples(vocabulary)?,
            None => Vec::new(),
        };
        if let Some(user) = &self.user {
            axioms.extend(user.compile(vocabulary)?.facts);
        }
        Ok(axioms)
    }

    /// Whether the list-axiom rules apply (OWL 2 RL only).
    pub fn has_list_rules(&self) -> bool {
        self.base.is_some_and(Ruleset::has_list_rules)
    }

    /// Whether the program named `name` closes the data under `owl:sameAs` (its built-in
    /// part has the equality rules), so queries may rely on it.
    pub fn closes_equality(name: &str) -> bool {
        let base = name.split('+').next().unwrap_or(name);
        Ruleset::from_name(base).is_some_and(|ruleset| {
            ruleset
                .owl_rules()
                .map_or(ruleset == Ruleset::Owl2Rl, |rules| {
                    rules.contains(&"eq-rep-s")
                })
        })
    }
}

impl From<Ruleset> for RuleProgram {
    fn from(ruleset: Ruleset) -> Self {
        Self::builtin(ruleset)
    }
}

impl From<&RuleProgram> for RuleProgram {
    fn from(program: &RuleProgram) -> Self {
        program.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "@prefix : <http://e/> .
        { ?x :parent ?y . ?y :parent ?z } => { ?x :grandparent ?z } .
        :parent :is :relation .";

    #[test]
    fn names_fingerprints_and_parts() {
        let user = Arc::new(UserRules::n3("family.n3", RULES).unwrap());
        let both = RuleProgram::new(Some(Ruleset::Rdfs), Some(user.clone())).unwrap();
        let custom = RuleProgram::new(None, Some(user)).unwrap();
        let builtin = RuleProgram::builtin(Ruleset::Rdfs);
        assert_eq!(both.name(), "rdfs+custom:family.n3");
        assert_eq!(custom.name(), "custom:family.n3");
        assert_eq!(builtin.fingerprint(), Ruleset::Rdfs.fingerprint());
        let fingerprints = [
            both.fingerprint(),
            custom.fingerprint(),
            builtin.fingerprint(),
        ];
        assert!(fingerprints[0] != fingerprints[1] && fingerprints[1] != fingerprints[2]);
        // One character of the rules changes the fingerprint.
        let edited = RuleProgram::new(
            None,
            Some(Arc::new(
                UserRules::n3("family.n3", RULES.replace("?z }", "?z } ")).unwrap(),
            )),
        )
        .unwrap();
        assert_ne!(edited.fingerprint(), custom.fingerprint());
        let mut vocabulary = LocalVocabulary::default();
        let rules = both.rules(&mut vocabulary).unwrap();
        assert_eq!(
            rules.len(),
            Ruleset::Rdfs.rules(&mut vocabulary).unwrap().len() + 1
        );
        assert_eq!(custom.axiom_triples(&mut vocabulary).unwrap().len(), 1);
        assert!(RuleProgram::new(None, None).is_none());
        assert!(
            UserRules::n3(
                "bad.n3",
                "{ ?x <http://e/p> ?y } => { ?x <http://e/q> [] } ."
            )
            .is_err()
        );
    }

    #[test]
    fn equality_closure_by_name() {
        assert!(RuleProgram::closes_equality("owl2-rl"));
        assert!(RuleProgram::closes_equality("owl2-rl+custom:x.n3"));
        assert!(RuleProgram::closes_equality("rdfs-plus"));
        assert!(!RuleProgram::closes_equality("rdfs"));
        assert!(!RuleProgram::closes_equality("custom:x.n3"));
    }
}
