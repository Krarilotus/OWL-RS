//! What a store materialises: a built-in ruleset, the user's rules (Notation3, compiled by
//! [`super::n3`], or a GraphDB ruleset, compiled by the crate's `pie` module), or both, the user's
//! on top.
//!
//! A program's [`RuleProgram::name`] and [`RuleProgram::fingerprint`] identify what it
//! derives; the store records them with the inferred stack, so changing the rules (a
//! single character of the rules file) rebuilds it rather than trusting a closure that
//! another program computed.

use std::sync::Arc;

use super::ir::{ParseError, Rule, Vocabulary};
use super::n3::{self, N3Program};
use super::rulesets::{Ruleset, SEMANTICS_VERSION, SemanticPart};
use super::vocabulary::LocalVocabulary;

/// User rules: their name (the file's, usually), their text, and its format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRules {
    name: String,
    text: String,
    format: RuleFormat,
    /// The relations a GraphDB ruleset's contexts compile to, taken from the validating
    /// compile: part of the fingerprint.
    context_relations: Vec<String>,
    /// The semantic parts the rules use, from the same compile: part of the fingerprint.
    parts: Vec<SemanticPart>,
}

/// The languages user rules are read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFormat {
    /// Notation3 ([`super::n3`]).
    N3,
    /// A GraphDB ruleset, `.pie` (the crate's `pie` module).
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
        let mut rules = Self {
            name: name.into(),
            text: text.into(),
            format,
            context_relations: Vec::new(),
            parts: Vec::new(),
        };
        let mut vocabulary = LocalVocabulary::default();
        let compiled = rules.compile(&mut vocabulary)?;
        if format == RuleFormat::Pie {
            rules.context_relations = super::pie::context_relations(&compiled, &vocabulary);
        }
        if compiled.rules.iter().any(|rule| !rule.guards.is_empty()) {
            rules.parts.push(SemanticPart::GuardsOverClasses);
        }
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
    /// FNV-1a over [`SEMANTICS_VERSION`], the user rules' text, the relations their
    /// contexts compile to and the versions of the semantic parts they use. Stable across
    /// builds and platforms.
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
        // A GraphDB ruleset with contexts derives relations the PIE compiler names, so a
        // build that names them otherwise derives other relations. Others keep their value.
        for name in &user.context_relations {
            feed(b"pie-context:");
            feed(name.as_bytes());
            feed(&[0]);
        }
        // The versions of the semantic parts the rules use ([`SemanticPart`]).
        for part in &user.parts {
            let (name, version) = part.version();
            feed(name.as_bytes());
            feed(&version.to_le_bytes());
        }
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

    /// What a build stored for a user ruleset before its context names and semantic parts
    /// entered the fingerprint.
    fn stored_before(user: &str) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in SEMANTICS_VERSION
            .to_le_bytes()
            .iter()
            .chain(b"custom:")
            .chain(user.as_bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// A GraphDB ruleset whose conclusion is in a context.
    const IN_A_CONTEXT: &str = "Prefices { e : http://e/ }
Rules
{
Id: r
  x <e:p> y
  ---
  x <e:q> y [Context <e:g#x>]
}
";

    /// [`IN_A_CONTEXT`] with a prefix `urn` declared, which a relation's name (`urn:nrese:…`)
    /// must not be expanded by.
    fn with_urn_prefix() -> String {
        IN_A_CONTEXT.replace(
            "Prefices { e : http://e/ }",
            "Prefices\n{\n  e : http://e/\n  urn : http://alias/\n}",
        )
    }

    /// A GraphDB ruleset with contexts no longer matches the state a build with the old
    /// context names stored, so the store rebuilds its closure; other rulesets keep theirs.
    /// `[ Context` is an accepted spelling (fix check 1 of R1-F1), and a declared `urn` prefix
    /// hides no relation (Sol's review of R1-OL1's fix, S1).
    #[test]
    fn changed_context_names_make_older_closures_stale() {
        let without = IN_A_CONTEXT.replace(" [Context <e:g#x>]", "");
        let spaced = IN_A_CONTEXT.replace("[Context", "[ Context");
        let urn = with_urn_prefix();
        for (text, rebuilds) in [
            (IN_A_CONTEXT, true),
            (&spaced, true),
            (&urn, true),
            (&without, false),
        ] {
            let user = Arc::new(UserRules::pie("rules.pie", text).unwrap());
            let program = RuleProgram::new(None, Some(user)).unwrap();
            let stored = stored_before(text);
            assert_eq!(program.fingerprint() != stored, rebuilds, "{text}");
        }
    }

    /// A user ruleset with a guard no longer matches the state a build stored before guards
    /// read the classes over representatives, so the store rebuilds its closure; others keep
    /// theirs (reasoning-semantics.md, "Freshness": a build that changes what a ruleset
    /// derives rebuilds once; Sol's review of the guard fix, r1 S3).
    #[test]
    fn guarded_user_rules_make_older_closures_stale() {
        let guarded = "@prefix : <http://e/> .
            @prefix log: <http://www.w3.org/2000/10/swap/log#> .
            { ?x :p ?y . ?x log:notEqualTo ?y } => { ?x :q ?y } .";
        for (text, rebuilds) in [(guarded, true), (RULES, false)] {
            let user = Arc::new(UserRules::n3("rules.n3", text).unwrap());
            let program = RuleProgram::new(None, Some(user)).unwrap();
            let stored = stored_before(text);
            assert_eq!(program.fingerprint() != stored, rebuilds, "{text}");
        }
    }

    /// The names the rules' context relations compile to are part of the fingerprint: a
    /// build that names them otherwise, as the plain concatenation before 20 Oct did,
    /// rebuilds what this one stored, with no version to change by hand (review of PR #29,
    /// R1-OL1).
    #[test]
    fn renamed_context_relations_change_the_fingerprint() {
        for text in [IN_A_CONTEXT, &with_urn_prefix()] {
            let user = UserRules::pie("rules.pie", text).unwrap();
            assert_eq!(
                user.context_relations,
                ["<urn:nrese:pie-context:http://e/g%23x#http://e/q>"],
                "{text}"
            );
        }
        let user = UserRules::pie("rules.pie", IN_A_CONTEXT).unwrap();
        let mut renamed = user.clone();
        renamed.context_relations = vec!["<urn:nrese:pie-context:http://e/g#x#http://e/q>".into()];
        let fingerprint = |user: UserRules| {
            RuleProgram::new(None, Some(Arc::new(user)))
                .unwrap()
                .fingerprint()
        };
        assert_ne!(fingerprint(user), fingerprint(renamed));
    }

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
