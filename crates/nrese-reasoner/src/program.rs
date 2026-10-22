//! What a store materialises: a built-in ruleset, the user's rules (Notation3, compiled by
//! [`super::n3`], or a GraphDB ruleset, compiled by the crate's `pie` module), or both, the user's
//! on top.
//!
//! A program's [`RuleProgram::name`] and [`RuleProgram::fingerprint`] identify what it
//! derives; the store records them with the inferred stack, so changing the rules (a
//! single character of the rules file) rebuilds it rather than trusting a closure that
//! another program computed.

use std::sync::Arc;

use super::ir::{ParseError, Rule, Term, Vocabulary};
use super::n3::{self, N3Program};
use super::rulesets::{Ruleset, SEMANTICS_VERSION};
use super::vocabulary::LocalVocabulary;

/// User rules: their name (the file's, usually), their text, and its format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRules {
    name: String,
    text: String,
    format: RuleFormat,
    /// Selected once with the validated text; fingerprint checks must not parse it again.
    pie_contexts: bool,
    /// A head can produce generalized rdf:type/owl:sameAs facts. Parsed once with the
    /// rules, not while comparing a persisted state's fingerprint.
    value_checks: bool,
    /// Parsed differing inline conclusions need head-local semantics on upgrade.
    pie_head_guards: bool,
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
            pie_contexts: false,
            value_checks: false,
            pie_head_guards: false,
        };
        let mut vocabulary = LocalVocabulary::default();
        let compiled = if format == RuleFormat::Pie {
            let (compiled, affected) =
                super::pie::compile_with_features(&rules.name, &rules.text, &mut vocabulary)?;
            rules.pie_head_guards = affected;
            compiled
        } else {
            rules.compile(&mut vocabulary)?
        };
        let rdf_type = vocabulary.iri(&format!("{}type", super::ir::RDF));
        let same_as = vocabulary.iri(&format!("{}sameAs", super::ir::OWL));
        let checked = |predicate: Term| {
            matches!(predicate, Term::Var(_))
                || predicate == Term::Const(rdf_type)
                || predicate == Term::Const(same_as)
        };
        rules.value_checks = compiled
            .rules
            .iter()
            .any(|rule| rule.head.atoms().any(|atom| checked(atom.0[1])))
            || compiled
                .facts
                .iter()
                .any(|fact| checked(Term::Const(fact[1])));
        rules.pie_contexts = format == RuleFormat::Pie && super::pie::uses_contexts(&rules.text);
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
        // A GraphDB ruleset with contexts derives relations named by the PIE compiler; a
        // change of those names is a change of what it derives. Others keep their value.
        if user.pie_contexts {
            feed(b"pie-context-names:");
            feed(&super::pie::CONTEXT_NAMES.to_le_bytes());
        }
        // Built-ins already carry this feature. Pure custom rules that cannot produce
        // generalized type/equality facts keep their previous fingerprint.
        if self.base.is_none() && user.value_checks {
            feed(super::rulesets::VALUE_CHECK_SEMANTICS);
        }
        if user.pie_head_guards {
            feed(b"pie-head-guards:");
            feed(&1u32.to_le_bytes());
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

    /// What a build before the PIE context names changed stored for a user ruleset.
    fn fingerprint_before_context_names(user: &str) -> u64 {
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

    /// A GraphDB ruleset with contexts no longer matches the state a build with the old
    /// context names stored, so the store rebuilds its closure; other rulesets keep theirs.
    #[test]
    fn changed_context_names_make_older_closures_stale() {
        let with = "Prefices { e : http://e/ }
Rules
{
Id: r
  x <e:p> y
  ---
  x <e:q> y [Context <e:g#x>]
}
";
        let without = "Prefices { e : http://e/ }
Rules
{
Id: r
  x <e:p> y
  ---
  x <e:q> y
}
";
        let spaced = with.replace("[Context", "[ Context");
        let tabbed = with.replace("[Context", "[\tContext");
        let prefixed_comment = with.replace("[Context", "[/*gap*/Context");
        let inner_comment = with.replace("[Context", "[Con/*gap*/text");
        for (text, rebuilds) in [
            (with, true),
            (&spaced, true),
            (&tabbed, true),
            (&prefixed_comment, true),
            (&inner_comment, true),
            (without, false),
        ] {
            let user = Arc::new(UserRules::pie("rules.pie", text).unwrap());
            let program = RuleProgram::new(None, Some(user)).unwrap();
            let stored = fingerprint_before_context_names(text);
            assert_eq!(program.fingerprint() != stored, rebuilds, "{text}");
        }
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

    /// Marker eligibility is parsed before normalization, and does not replace either
    /// Context-name or value-check invalidation. Unaffected rules keep old hashes.
    #[test]
    fn inline_head_marker_is_affected_only_and_composes() {
        for contexts in [false, true] {
            for values in [false, true] {
                for affected in [false, true] {
                    let context = if contexts { "[Context <urn:g#x>]" } else { "" };
                    let constraint = if affected { "[Constraint x != x]" } else { "" };
                    let value = if values {
                        "x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> y"
                    } else {
                        "x <urn:r> y"
                    };
                    let text = format!(
                        "Prefices {{}}\nRules {{\nId: r\nx <urn:p> y\n---\nx <urn:q> y {constraint} {context}\n{value}\n}}\n"
                    );
                    let user = Arc::new(UserRules::pie("r.pie", &text).unwrap());
                    assert_eq!(
                        (user.pie_contexts, user.value_checks, user.pie_head_guards),
                        (contexts, values, affected)
                    );
                    let program = RuleProgram::new(None, Some(user)).unwrap();
                    let mut expected = fingerprint_before_context_names(&text);
                    let mut feed = |bytes: &[u8]| {
                        for &byte in bytes {
                            expected ^= u64::from(byte);
                            expected = expected.wrapping_mul(0x0000_0100_0000_01b3);
                        }
                    };
                    if contexts {
                        feed(b"pie-context-names:");
                        feed(&super::super::pie::CONTEXT_NAMES.to_le_bytes());
                    }
                    if values {
                        feed(super::super::rulesets::VALUE_CHECK_SEMANTICS);
                    }
                    if affected {
                        feed(b"pie-head-guards:");
                        feed(&1u32.to_le_bytes());
                    }
                    assert_eq!(program.fingerprint(), expected);
                }
            }
        }
        for heads in [
            "x <urn:q> y [Constraint x != y]",
            "x <urn:q> y [Constraint x != y]\nx <urn:r> y [Constraint x != y]",
            "[Constraint x != x]\nx <urn:q> y\nx <urn:r> y",
        ] {
            let text = format!("Prefices {{}}\nRules {{\nId: r\nx <urn:p> y\n---\n{heads}\n}}\n");
            let program = RuleProgram::new(
                None,
                Some(Arc::new(UserRules::pie("r.pie", &text).unwrap())),
            )
            .unwrap();
            assert_eq!(
                program.fingerprint(),
                fingerprint_before_context_names(&text)
            );
        }
    }
}
