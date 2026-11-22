//! Built-in rulesets (reasoner-v2 design §3.3), as data in the [`parse_rules`] syntax.
//!
//! `OWL2_RL` is the W3C OWL 2 RL/RDF rule tables (OWL 2 Profiles, 2nd ed., §4.3), with
//! the W3C rule names and one extra check for an empty `owl:Thing`.
//! Rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`,
//! `cls-oo`, `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) aren't in the
//! text: they're instantiated per list axiom by [`super::lists`]. Left out, as in owlrl's
//! default mode and the reasoning benchmark's normalisation: `eq-ref` (`x sameAs x` for
//! every term), some datatype rules (table 8) and axiomatic triples beyond the rules'
//! own seed statements. What `eq-ref`
//! contributes to consistency is kept without materialising it: `x differentFrom x` is a
//! second `eq-diff1` rule, and `AllDifferent` lists naming one individual twice are
//! inconsistent (`eq-diff2/3` in [`super::lists`]).
//!
//! The other profiles are the RDFS rules plus a named part of the OWL 2 RL table
//! ([`Ruleset::owl_rules`]), as GraphDB's rulesets of the same names are:
//!
//! | Ruleset | Rules |
//! |---|---|
//! | `rdfs` | rdfs2, 3, 5, 7, 9, 11: what queries over data use (GraphDB's partial RDFS) |
//! | `rdfs-full` | every RDFS entailment rule (RDF 1.1 Semantics §9.2) and the finite RDF and RDFS axiomatic triples |
//! | `rdfs-plus` | `rdfs` with equality, inverse, symmetric, transitive, functional and inverse functional properties, equivalent classes and properties |
//! | `owl-horst` | `rdfs-plus` with `hasValue`, `someValuesFrom` and `allValuesFrom` (ter Horst's pD*) |
//! | `owl2-ql` | what OWL 2 QL axioms entail without inventing individuals: hierarchies, domains, ranges, inverses, reflexive properties, `someValuesFrom owl:Thing`, and checks for disjointness, complements and an empty `owl:Thing`; the built-in rules skip annotation assertions, an annotation property's domain or range, and sub-property axioms with one at either end (Direct Semantics §1; `guard_annotation_readings`); user rules on top are left as written |
//! | `owl2-rl` | the OWL 2 RL/RDF rules |
//!
//! Every ruleset is validated by the tests: it parses, every rule is safe, and the
//! evaluator's closure equals the owlrl oracle's on the benchmark data.

use std::sync::OnceLock;

use super::ir::{
    AnnotationIds, Atom, Guard, OWL, ParseError, Rule, RuleSource, Term, Triple, Vocabulary,
    parse_rules, parse_sources, rule_sources,
};
use crate::vocabulary::LocalVocabulary;

/// The named rulesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruleset {
    /// RDFS entailment rules without axiomatic triples (GraphDB's `rdfs` with partialRDFS).
    Rdfs,
    /// All RDFS entailment rules and the axiomatic triples (RDF 1.1 Semantics §9.2).
    RdfsFull,
    /// RDFS with the property characteristics and equivalences of RDFS-Plus.
    RdfsPlus,
    /// ter Horst's pD* (OWL-Horst).
    OwlHorst,
    /// OWL 2 QL, materialised.
    Owl2Ql,
    /// The OWL 2 RL/RDF rules.
    Owl2Rl,
}

/// Every ruleset, in order of what it derives.
pub const ALL: [Ruleset; 6] = [
    Ruleset::Rdfs,
    Ruleset::RdfsFull,
    Ruleset::RdfsPlus,
    Ruleset::OwlHorst,
    Ruleset::Owl2Ql,
    Ruleset::Owl2Rl,
];

/// The evaluation semantics beyond the rule text, as versioned parts: the one place that
/// decides which stores rebuild after a build changes what rules derive or reject.
/// - The base, this version, covers what every program depends on: list compilation,
///   equality handling, modules. Every fingerprint feeds it, so bumping it rebuilds every
///   store.
/// - A [`SemanticPart`] covers what only some user rulesets use. A user ruleset's
///   fingerprint feeds the version of each part its validating compile reports using
///   ([`crate::RuleProgram::fingerprint`]), so bumping a part rebuilds only those stores.
///   A built-in ruleset's fingerprint feeds the parts its compile uses ([`Ruleset::parts`]).
///   A new part starts at 1.
///
/// The fingerprint also covers the rule text and the relations a GraphDB ruleset's contexts
/// compile to; a change of those names needs no bump.
///
/// 3 (28 Oct 2026): version 2's maintenance could recompute a split without what schema
/// statements alone derive (`C owl:equivalentClass C` from `C rdfs:subClassOf C` once
/// `C owl:sameAs D` went), so a closure it wrote is no longer read as current.
///
/// 4 (7 Nov 2026): the datatype checks (dt-not-type, dt-diff) read literals as OWL 2's
/// datatype map does (`engine::datatypes`): an ill-typed literal under a range, and values
/// version 3 didn't judge, can now be inconsistencies, so a version 3 verdict is rechecked.
///
/// 5 (17 Nov 2026): under `reasoner.equality = "compact"` the datatype checks read every
/// literal of an `owl:sameAs` class, not only the representative's statements: two
/// different values in one class, or an ill-typed member under a range, are now
/// inconsistencies there too, so a version 4 closure and verdict are rebuilt once.
///
/// 6 (18 Nov 2026): a commit maintained by a rebuild (a split over a compact stack, a
/// store with an inverse property expression, a policy rebuild, a first write) is judged by
/// the rebuild's own violations. Version 5 could accept such a commit and record the store
/// consistent over a clash, so a version 5 verdict is rechecked once.
///
/// 7 (19 Nov 2026): the OWL 2 RL closure includes the annotation property and class
/// declarations from prp-ap, cls-thing and cls-nothing1, so version 6 stores rebuild once.
///
/// 8 (19 Nov 2026): an incomplete branch or a side loop no longer hides the
/// rules of a list's walks to `rdf:nil`. An older store rematerialises once.
///
/// 9 (21 Nov 2026): every accepting list walk contributes its rule conclusions,
/// including walks through cycles and walks beyond the rule encoder's bounds.
/// An older store rematerialises once.
pub const SEMANTICS_VERSION: u32 = 9;

/// A part of the evaluation semantics only some rulesets use ([`SEMANTICS_VERSION`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticPart {
    /// Rules with guards, run over representatives of `owl:sameAs` classes. 1 (26 Oct 2026):
    /// the guards hold where members of the classes satisfy them, not the representatives
    /// alone (`representatives::over_classes`).
    GuardsOverClasses,
    /// Rules named like OWL 2 RL's equality rules, or holding them under other names. 1 (16 Nov
    /// 2026): the equality module stands in for the rules that say what those say, whatever
    /// their names (`equality_rules`), not for the rules so named.
    EqualityRules,
    /// A built-in ruleset's rules guarded at compile against what the OWL reader reads as an
    /// annotation (`guard_annotation_readings`); its fingerprint feeds this part
    /// ([`Ruleset::parts`]). 1 (21 Nov 2026): `owl2-ql`'s rules no longer read an annotation
    /// property's assertions, domains, ranges or sub-properties, so its stores rebuild once.
    AnnotationReadings,
}

impl SemanticPart {
    /// The part's name and version, as fingerprints feed them.
    pub fn version(self) -> (&'static str, u32) {
        match self {
            Self::GuardsOverClasses => ("guards-over-classes", 1),
            Self::EqualityRules => ("equality-rules", 1),
            Self::AnnotationReadings => ("annotation-readings", 1),
        }
    }
}

impl Ruleset {
    /// Identifies what this ruleset derives: FNV-1a over [`SEMANTICS_VERSION`], the name,
    /// the rule text, the name and text of every OWL 2 RL rule it adds, its axioms, and the
    /// versions of the semantic parts its compile uses ([`Self::parts`]) (the bodies it
    /// compiles, so editing one changes the fingerprint of every ruleset using it: the
    /// review of 3 October 2026, C6). Stable across builds and platforms (no std hasher,
    /// which may change).
    ///
    /// The text is static, so the hashes are computed once per process, for every ruleset
    /// at the first ask: a store asks on every commit.
    pub fn fingerprint(self) -> u64 {
        static FINGERPRINTS: OnceLock<[u64; ALL.len()]> = OnceLock::new();
        // `ALL` lists the variants in declaration order, so a variant's discriminant is its index.
        FINGERPRINTS.get_or_init(|| ALL.map(Self::hash_parts))[self as usize]
    }

    /// The FNV-1a hash [`Ruleset::fingerprint`] returns.
    fn hash_parts(self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for &byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        feed(&SEMANTICS_VERSION.to_le_bytes());
        feed(self.name().as_bytes());
        feed(self.text().as_bytes());
        for rule in self.added_rules() {
            feed(rule.name.as_bytes());
            feed(rule.source.as_bytes());
        }
        feed(self.axioms().as_bytes());
        for part in self.parts() {
            let (name, version) = part.version();
            feed(name.as_bytes());
            feed(&version.to_le_bytes());
        }
        hash
    }

    /// The semantic parts the ruleset's compile uses beyond its text ([`SemanticPart`]).
    pub fn parts(self) -> &'static [SemanticPart] {
        match self {
            Self::Owl2Ql => &[SemanticPart::AnnotationReadings],
            _ => &[],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rdfs => "rdfs",
            Self::RdfsFull => "rdfs-full",
            Self::RdfsPlus => "rdfs-plus",
            Self::OwlHorst => "owl-horst",
            Self::Owl2Ql => "owl2-ql",
            Self::Owl2Rl => "owl2-rl",
        }
    }

    /// The ruleset named `name` (as [`Ruleset::name`] gives it).
    pub fn from_name(name: &str) -> Option<Self> {
        ALL.into_iter().find(|ruleset| ruleset.name() == name)
    }

    /// The ruleset's own rule text; the OWL 2 RL rules it adds are [`Ruleset::owl_rules`].
    pub fn text(self) -> &'static str {
        match self {
            Self::Rdfs | Self::RdfsPlus | Self::OwlHorst => RDFS,
            Self::RdfsFull => RDFS_FULL,
            Self::Owl2Ql => OWL2_QL_OWN,
            Self::Owl2Rl => OWL2_RL,
        }
    }

    /// The OWL 2 RL rules (by their W3C names) the ruleset adds to its own text.
    pub fn owl_rules(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Rdfs | Self::RdfsFull | Self::Owl2Rl => None,
            Self::RdfsPlus => Some(RDFS_PLUS),
            Self::OwlHorst => Some(OWL_HORST),
            Self::Owl2Ql => Some(OWL2_QL),
        }
    }

    /// Statements that hold in every graph the ruleset reasons over, as `s p o` lines of
    /// prefixed names: they seed the closure and are never retracted.
    pub fn axioms(self) -> &'static str {
        match self {
            Self::RdfsFull => RDFS_AXIOMS,
            Self::Owl2Rl => {
                static AXIOMS: OnceLock<String> = OnceLock::new();
                AXIOMS.get_or_init(|| {
                    [OWL2_RL_DATATYPE_AXIOMS, OWL2_RL_ANNOTATION_AND_CLASS_AXIOMS].concat()
                })
            }
            _ => "",
        }
    }

    /// Whether the list-axiom rules apply (OWL 2 RL only).
    pub fn has_list_rules(self) -> bool {
        self == Self::Owl2Rl
    }

    /// Whether the compiled templates enable the engine's equality handling.
    pub(crate) fn may_equate_literals(self) -> bool {
        static CAPABILITIES: OnceLock<[bool; ALL.len()]> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ALL.map(|ruleset| {
                let mut vocabulary = LocalVocabulary::default();
                let same_as = vocabulary.iri(&format!("{OWL}sameAs"));
                ruleset.rules(&mut vocabulary).map_or(true, |rules| {
                    super::representatives::same_as(&rules, same_as).is_some()
                })
            })
        })[self as usize]
    }

    /// Compiles the rule templates. For evaluation over facts in a local vocabulary, use
    /// [`Self::rules_for`] to bind any guards that read the asserted annotation properties.
    pub fn rules(self, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
        let mut rules = parse_rules(self.text(), vocabulary)?;
        rules.extend(parse_sources(self.added_rules(), vocabulary)?);
        if self.parts().contains(&SemanticPart::AnnotationReadings) {
            guard_annotation_readings(&mut rules, vocabulary);
        }
        Ok(rules)
    }

    /// The rules for one evaluation of `asserted`, using that state's annotation properties.
    /// The triples must use `vocabulary`'s ids.
    pub fn rules_for(
        self,
        vocabulary: &mut LocalVocabulary,
        asserted: &[Triple],
    ) -> Result<Vec<Rule>, ParseError> {
        let mut rules = self.rules(vocabulary)?;
        if self.parts().contains(&SemanticPart::AnnotationReadings) {
            use nrese_owl::mapping::AnnotationProperties;
            let mut properties = AnnotationProperties::new(&mut |iri| Some(vocabulary.iri(iri)));
            for &fact in asserted {
                if !properties.declares(fact) {
                    continue;
                }
                let iri = vocabulary
                    .text(fact[0])
                    .strip_prefix('<')
                    .and_then(|s| s.strip_suffix('>'));
                properties.read(fact, iri.is_some_and(nrese_owl::may_name_annotation));
            }
            bind_annotations(
                &mut rules,
                &AnnotationIds::new(properties.properties().collect()),
            );
        }
        Ok(rules)
    }

    /// The OWL 2 RL rules [`Ruleset::owl_rules`] names, as written in [`OWL2_RL`]: what
    /// [`Ruleset::rules`] compiles and [`Ruleset::fingerprint`] hashes.
    fn added_rules(self) -> Vec<RuleSource> {
        let Some(names) = self.owl_rules() else {
            return Vec::new();
        };
        rule_sources(OWL2_RL)
            .expect("the OWL 2 RL text starts with a rule")
            .into_iter()
            .filter(|rule| names.contains(&rule.name.as_str()))
            .collect()
    }

    /// The axiomatic triples ([`Ruleset::axioms`]) as ids.
    pub fn axiom_triples(
        self,
        vocabulary: &mut impl Vocabulary,
    ) -> Result<Vec<[u64; 3]>, ParseError> {
        let mut out = Vec::new();
        for line in self
            .axioms()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            out.push(
                super::ir::parse_triple(line, vocabulary)
                    .map_err(|message| ParseError::new(line, message))?,
            );
        }
        Ok(out)
    }
}

/// Gives each annotation guard the set read from this evaluation's asserted state.
pub(crate) fn bind_annotations(rules: &mut [Rule], set: &AnnotationIds) {
    for rule in rules {
        for guard in &mut rule.guards {
            if let Guard::NotAnnotation(_, ids) = guard {
                *ids = set.clone();
            }
        }
    }
}

/// Guards `rules` against what the OWL reader reads as an annotation: annotations "have no
/// semantic meaning in OWL 2" (Direct Semantics §1), so no rule may consume one. Each variable
/// in a body position the reader reads as an annotation when it names an annotation property
/// gets one [`Guard::NotAnnotation`] (filled with the data's set per run, `engine::Program`):
/// 1. the predicate of any atom, `(?x ?p ?y)`: an annotation assertion;
/// 2. the subject of an `rdfs:domain` or `rdfs:range` atom: an annotation axiom;
/// 3. either end of an `rdfs:subPropertyOf` atom: an annotation axiom.
///
/// Constants aren't guarded. Applied to a built-in ruleset's own rules at their compile
/// ([`SemanticPart::AnnotationReadings`]); a user's rules on top keep what they wrote.
fn guard_annotation_readings(rules: &mut [Rule], vocabulary: &mut impl Vocabulary) {
    let rdfs = |local: &str| format!("{}{local}", super::ir::RDFS);
    let [domain, range, sub] =
        ["domain", "range", "subPropertyOf"].map(|local| Term::Const(vocabulary.iri(&rdfs(local))));
    for rule in rules {
        let mut read = Vec::new();
        for &Atom([s, p, o]) in &rule.body {
            read.push(p);
            if p == domain || p == range {
                read.push(s);
            } else if p == sub {
                read.extend([s, o]);
            }
        }
        for term in read {
            let guarded = |g: &Guard| matches!(g, Guard::NotAnnotation(t, _) if *t == term);
            if matches!(term, Term::Var(_)) && !rule.guards.iter().any(guarded) {
                rule.guards
                    .push(Guard::NotAnnotation(term, AnnotationIds::default()));
            }
        }
    }
}

/// RDFS entailment (RDF 1.1 Semantics §9.2), rules rdfs2, 3, 5, 7, 9, 11, without the
/// axiomatic and "everything is a resource" rules.
pub const RDFS: &str = r#"
rdfs2:  (?p rdfs:domain ?c), (?x ?p ?y) -> (?x rdf:type ?c)
rdfs3:  (?p rdfs:range ?c), (?x ?p ?y) -> (?y rdf:type ?c)
rdfs5:  (?p rdfs:subPropertyOf ?q), (?q rdfs:subPropertyOf ?r) -> (?p rdfs:subPropertyOf ?r)
rdfs7:  (?p rdfs:subPropertyOf ?q), (?x ?p ?y) -> (?x ?q ?y)
rdfs9:  (?c rdfs:subClassOf ?d), (?x rdf:type ?c) -> (?x rdf:type ?d)
rdfs11: (?c rdfs:subClassOf ?d), (?d rdfs:subClassOf ?e) -> (?c rdfs:subClassOf ?e)
"#;

/// RDFS entailment (RDF 1.1 Semantics §9.2.1), every rule: rdfD2 (a predicate is a
/// property), rdfs2 to rdfs13. rdfs1 (every recognised datatype is an `rdfs:Datatype`) is
/// in the axioms; rdfD1, which invents a blank node for every literal, is left out as
/// every system leaves it out.
pub const RDFS_FULL: &str = r#"
rdfD2:  (?x ?p ?y) -> (?p rdf:type rdf:Property)
rdfs2:  (?p rdfs:domain ?c), (?x ?p ?y) -> (?x rdf:type ?c)
rdfs3:  (?p rdfs:range ?c), (?x ?p ?y) -> (?y rdf:type ?c)
rdfs4a: (?x ?p ?y) -> (?x rdf:type rdfs:Resource)
rdfs4b: (?x ?p ?y) -> (?y rdf:type rdfs:Resource)
rdfs5:  (?p rdfs:subPropertyOf ?q), (?q rdfs:subPropertyOf ?r) -> (?p rdfs:subPropertyOf ?r)
rdfs6:  (?p rdf:type rdf:Property) -> (?p rdfs:subPropertyOf ?p)
rdfs7:  (?p rdfs:subPropertyOf ?q), (?x ?p ?y) -> (?x ?q ?y)
rdfs8:  (?c rdf:type rdfs:Class) -> (?c rdfs:subClassOf rdfs:Resource)
rdfs9:  (?c rdfs:subClassOf ?d), (?x rdf:type ?c) -> (?x rdf:type ?d)
rdfs10: (?c rdf:type rdfs:Class) -> (?c rdfs:subClassOf ?c)
rdfs11: (?c rdfs:subClassOf ?d), (?d rdfs:subClassOf ?e) -> (?c rdfs:subClassOf ?e)
rdfs12: (?p rdf:type rdfs:ContainerMembershipProperty) -> (?p rdfs:subPropertyOf rdfs:member)
rdfs13: (?d rdf:type rdfs:Datatype) -> (?d rdfs:subClassOf rdfs:Literal)
"#;

/// The RDF and RDFS axiomatic triples (RDF 1.1 Semantics §8.1.1, §9.1), without the
/// infinitely many about `rdf:_1`, `rdf:_2`, …, and rdfs1 for the datatypes every
/// system recognises (`rdf:langString`, `xsd:string`).
/// OWL 2 RL's rule `dt-type1`: every datatype the profile supports (all of OWL 2's but
/// `owl:real` and `owl:rational`) is an `rdfs:Datatype`.
pub const OWL2_RL_DATATYPE_AXIOMS: &str = r#"
rdf:PlainLiteral rdf:type rdfs:Datatype
rdf:XMLLiteral rdf:type rdfs:Datatype
rdfs:Literal rdf:type rdfs:Datatype
xsd:decimal rdf:type rdfs:Datatype
xsd:integer rdf:type rdfs:Datatype
xsd:nonNegativeInteger rdf:type rdfs:Datatype
xsd:nonPositiveInteger rdf:type rdfs:Datatype
xsd:positiveInteger rdf:type rdfs:Datatype
xsd:negativeInteger rdf:type rdfs:Datatype
xsd:long rdf:type rdfs:Datatype
xsd:int rdf:type rdfs:Datatype
xsd:short rdf:type rdfs:Datatype
xsd:byte rdf:type rdfs:Datatype
xsd:unsignedLong rdf:type rdfs:Datatype
xsd:unsignedInt rdf:type rdfs:Datatype
xsd:unsignedShort rdf:type rdfs:Datatype
xsd:unsignedByte rdf:type rdfs:Datatype
xsd:double rdf:type rdfs:Datatype
xsd:float rdf:type rdfs:Datatype
xsd:string rdf:type rdfs:Datatype
xsd:normalizedString rdf:type rdfs:Datatype
xsd:token rdf:type rdfs:Datatype
xsd:language rdf:type rdfs:Datatype
xsd:Name rdf:type rdfs:Datatype
xsd:NCName rdf:type rdfs:Datatype
xsd:NMTOKEN rdf:type rdfs:Datatype
xsd:boolean rdf:type rdfs:Datatype
xsd:hexBinary rdf:type rdfs:Datatype
xsd:base64Binary rdf:type rdfs:Datatype
xsd:anyURI rdf:type rdfs:Datatype
xsd:dateTime rdf:type rdfs:Datatype
xsd:dateTimeStamp rdf:type rdfs:Datatype
"#;

/// OWL 2 RL's empty-body rules prp-ap, cls-thing and cls-nothing1.
const OWL2_RL_ANNOTATION_AND_CLASS_AXIOMS: &str = r#"
rdfs:label rdf:type owl:AnnotationProperty
rdfs:comment rdf:type owl:AnnotationProperty
rdfs:seeAlso rdf:type owl:AnnotationProperty
rdfs:isDefinedBy rdf:type owl:AnnotationProperty
owl:deprecated rdf:type owl:AnnotationProperty
owl:versionInfo rdf:type owl:AnnotationProperty
owl:priorVersion rdf:type owl:AnnotationProperty
owl:backwardCompatibleWith rdf:type owl:AnnotationProperty
owl:incompatibleWith rdf:type owl:AnnotationProperty
owl:Thing rdf:type owl:Class
owl:Nothing rdf:type owl:Class
"#;

pub const RDFS_AXIOMS: &str = r#"
rdf:type rdf:type rdf:Property
rdf:subject rdf:type rdf:Property
rdf:predicate rdf:type rdf:Property
rdf:object rdf:type rdf:Property
rdf:first rdf:type rdf:Property
rdf:rest rdf:type rdf:Property
rdf:value rdf:type rdf:Property
rdf:nil rdf:type rdf:List
rdf:type rdfs:domain rdfs:Resource
rdfs:domain rdfs:domain rdf:Property
rdfs:range rdfs:domain rdf:Property
rdfs:subPropertyOf rdfs:domain rdf:Property
rdfs:subClassOf rdfs:domain rdfs:Class
rdf:subject rdfs:domain rdf:Statement
rdf:predicate rdfs:domain rdf:Statement
rdf:object rdfs:domain rdf:Statement
rdfs:member rdfs:domain rdfs:Resource
rdf:first rdfs:domain rdf:List
rdf:rest rdfs:domain rdf:List
rdfs:seeAlso rdfs:domain rdfs:Resource
rdfs:isDefinedBy rdfs:domain rdfs:Resource
rdfs:comment rdfs:domain rdfs:Resource
rdfs:label rdfs:domain rdfs:Resource
rdf:value rdfs:domain rdfs:Resource
rdf:type rdfs:range rdfs:Class
rdfs:domain rdfs:range rdfs:Class
rdfs:range rdfs:range rdfs:Class
rdfs:subPropertyOf rdfs:range rdf:Property
rdfs:subClassOf rdfs:range rdfs:Class
rdf:subject rdfs:range rdfs:Resource
rdf:predicate rdfs:range rdfs:Resource
rdf:object rdfs:range rdfs:Resource
rdfs:member rdfs:range rdfs:Resource
rdf:first rdfs:range rdfs:Resource
rdf:rest rdfs:range rdf:List
rdfs:seeAlso rdfs:range rdfs:Resource
rdfs:isDefinedBy rdfs:range rdfs:Resource
rdfs:comment rdfs:range rdfs:Literal
rdfs:label rdfs:range rdfs:Literal
rdf:value rdfs:range rdfs:Resource
rdf:Alt rdfs:subClassOf rdfs:Container
rdf:Bag rdfs:subClassOf rdfs:Container
rdf:Seq rdfs:subClassOf rdfs:Container
rdfs:ContainerMembershipProperty rdfs:subClassOf rdf:Property
rdfs:isDefinedBy rdfs:subPropertyOf rdfs:seeAlso
rdfs:Datatype rdfs:subClassOf rdfs:Class
rdf:langString rdf:type rdfs:Datatype
rdf:HTML rdf:type rdfs:Datatype
rdf:XMLLiteral rdf:type rdfs:Datatype
xsd:string rdf:type rdfs:Datatype
"#;

/// RDFS-Plus: RDFS with the OWL 2 RL rules for equality, inverse, symmetric, transitive,
/// functional and inverse functional properties, equivalent classes and properties.
pub const RDFS_PLUS: &[&str] = &[
    "eq-sym", "eq-trans", "eq-rep-s", "eq-rep-p", "eq-rep-o", "prp-fp", "prp-ifp", "prp-symp",
    "prp-trp", "prp-eqp1", "prp-eqp2", "prp-inv1", "prp-inv2", "cax-eqc1", "cax-eqc2", "scm-eqc1",
    "scm-eqc2", "scm-eqp1", "scm-eqp2",
];

/// OWL-Horst (pD*): RDFS-Plus with `hasValue`, `someValuesFrom` and `allValuesFrom`.
pub const OWL_HORST: &[&str] = &[
    "eq-sym", "eq-trans", "eq-rep-s", "eq-rep-p", "eq-rep-o", "prp-fp", "prp-ifp", "prp-symp",
    "prp-trp", "prp-eqp1", "prp-eqp2", "prp-inv1", "prp-inv2", "cax-eqc1", "cax-eqc2", "scm-eqc1",
    "scm-eqc2", "scm-eqp1", "scm-eqp2", "cls-hv1", "cls-hv2", "cls-svf1", "cls-svf2", "cls-avf",
];

/// OWL 2 QL's own rules, beside the OWL 2 RL ones [`OWL2_QL`] names: a reflexive property
/// (`ReflexiveObjectProperty`, in QL but not in RL) relates every individual to itself.
/// Under the Direct Semantics that is every element; materialised, every term used as
/// an individual: a class's member, either end of an object property, a data property's
/// subject, a declared named individual. Without them OWL2Bench QL's q01 (`?x :knows ?y`)
/// missed the 3,677 pairs `a :knows a` (GraphDB's 5,104 rows; office batch B, 5 October
/// 2026).
pub const OWL2_QL_OWN: &str = r#"
prp-refl-c: (?p rdf:type owl:ReflexiveProperty), (?x rdf:type ?c), (?c rdf:type owl:Class)
            -> (?x ?p ?x)
prp-refl-o: (?p rdf:type owl:ReflexiveProperty), (?x ?q ?y), (?q rdf:type owl:ObjectProperty)
            -> (?x ?p ?x), (?y ?p ?y)
prp-refl-d: (?p rdf:type owl:ReflexiveProperty), (?x ?q ?v), (?q rdf:type owl:DatatypeProperty)
            -> (?x ?p ?x)
prp-refl-i: (?p rdf:type owl:ReflexiveProperty), (?x rdf:type owl:NamedIndividual)
            -> (?x ?p ?x)
"#;

/// OWL 2 QL, materialised: its axioms without the existentials on the right of
/// `SubClassOf` (they would invent individuals), and its consistency checks.
pub const OWL2_QL: &[&str] = &[
    "prp-dom",
    "prp-rng",
    "prp-spo1",
    "prp-eqp1",
    "prp-eqp2",
    "prp-inv1",
    "prp-inv2",
    "prp-symp",
    "prp-asyp",
    "prp-irp",
    "prp-pdw",
    "cls-com",
    "cls-nothing2",
    "cls-thing-empty",
    "cls-svf2",
    "cax-sco",
    "cax-eqc1",
    "cax-eqc2",
    "cax-dw",
    "scm-cls",
    "scm-sco",
    "scm-eqc1",
    "scm-eqc2",
    "scm-op",
    "scm-dp",
    "scm-spo",
    "scm-eqp1",
    "scm-eqp2",
    "scm-dom1",
    "scm-dom2",
    "scm-rng1",
    "scm-rng2",
];

/// OWL 2 RL/RDF, tables 4-7 and 9 (list rules in `lists`), with the empty `owl:Thing` check.
pub const OWL2_RL: &str = r#"
# Table 4: equality
eq-sym:   (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)
eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)
eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)
eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)
eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)
eq-diff1: (?x owl:sameAs ?y), (?x owl:differentFrom ?y) -> false
eq-diff1: (?x owl:differentFrom ?x) -> false

# Table 5: properties
prp-dom:  (?p rdfs:domain ?c), (?x ?p ?y) -> (?x rdf:type ?c)
prp-rng:  (?p rdfs:range ?c), (?x ?p ?y) -> (?y rdf:type ?c)
prp-fp:   (?p rdf:type owl:FunctionalProperty), (?x ?p ?y1), (?x ?p ?y2), ?y1 != ?y2
          -> (?y1 owl:sameAs ?y2)
prp-ifp:  (?p rdf:type owl:InverseFunctionalProperty), (?x1 ?p ?y), (?x2 ?p ?y), ?x1 != ?x2
          -> (?x1 owl:sameAs ?x2)
prp-irp:  (?p rdf:type owl:IrreflexiveProperty), (?x ?p ?x) -> false
prp-symp: (?p rdf:type owl:SymmetricProperty), (?x ?p ?y) -> (?y ?p ?x)
prp-asyp: (?p rdf:type owl:AsymmetricProperty), (?x ?p ?y), (?y ?p ?x) -> false
prp-trp:  (?p rdf:type owl:TransitiveProperty), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)
prp-spo1: (?p1 rdfs:subPropertyOf ?p2), (?x ?p1 ?y) -> (?x ?p2 ?y)
prp-eqp1: (?p1 owl:equivalentProperty ?p2), (?x ?p1 ?y) -> (?x ?p2 ?y)
prp-eqp2: (?p1 owl:equivalentProperty ?p2), (?x ?p2 ?y) -> (?x ?p1 ?y)
prp-pdw:  (?p1 owl:propertyDisjointWith ?p2), (?x ?p1 ?y), (?x ?p2 ?y) -> false
prp-inv1: (?p1 owl:inverseOf ?p2), (?x ?p1 ?y) -> (?y ?p2 ?x)
prp-inv2: (?p1 owl:inverseOf ?p2), (?x ?p2 ?y) -> (?y ?p1 ?x)
prp-npa1: (?x owl:sourceIndividual ?i1), (?x owl:assertionProperty ?p),
          (?x owl:targetIndividual ?i2), (?i1 ?p ?i2) -> false
prp-npa2: (?x owl:sourceIndividual ?i), (?x owl:assertionProperty ?p),
          (?x owl:targetValue ?lt), (?i ?p ?lt) -> false

# Table 6: classes
cls-nothing2: (?x rdf:type owl:Nothing) -> false
# Beyond the table: owl:Thing covers the nonempty universe (OWL 2 RDF-Based Semantics, Table 5.2).
cls-thing-empty: (owl:Thing rdfs:subClassOf owl:Nothing) -> false
cls-com:  (?c1 owl:complementOf ?c2), (?x rdf:type ?c1), (?x rdf:type ?c2) -> false
cls-svf1: (?x owl:someValuesFrom ?y), (?x owl:onProperty ?p), (?u ?p ?v), (?v rdf:type ?y)
          -> (?u rdf:type ?x)
cls-svf2: (?x owl:someValuesFrom owl:Thing), (?x owl:onProperty ?p), (?u ?p ?v)
          -> (?u rdf:type ?x)
cls-avf:  (?x owl:allValuesFrom ?y), (?x owl:onProperty ?p), (?u rdf:type ?x), (?u ?p ?v)
          -> (?v rdf:type ?y)
cls-hv1:  (?x owl:hasValue ?y), (?x owl:onProperty ?p), (?u rdf:type ?x) -> (?u ?p ?y)
cls-hv2:  (?x owl:hasValue ?y), (?x owl:onProperty ?p), (?u ?p ?y) -> (?u rdf:type ?x)
cls-maxc1: (?x owl:maxCardinality "0"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
           (?u rdf:type ?x), (?u ?p ?y) -> false
cls-maxc2: (?x owl:maxCardinality "1"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
           (?u rdf:type ?x), (?u ?p ?y1), (?u ?p ?y2), ?y1 != ?y2 -> (?y1 owl:sameAs ?y2)
cls-maxqc1: (?x owl:maxQualifiedCardinality "0"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
            (?x owl:onClass ?c), (?u rdf:type ?x), (?u ?p ?y), (?y rdf:type ?c) -> false
cls-maxqc2: (?x owl:maxQualifiedCardinality "0"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
            (?x owl:onClass owl:Thing), (?u rdf:type ?x), (?u ?p ?y) -> false
cls-maxqc3: (?x owl:maxQualifiedCardinality "1"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
            (?x owl:onClass ?c), (?u rdf:type ?x), (?u ?p ?y1), (?y1 rdf:type ?c),
            (?u ?p ?y2), (?y2 rdf:type ?c), ?y1 != ?y2 -> (?y1 owl:sameAs ?y2)
cls-maxqc4: (?x owl:maxQualifiedCardinality "1"^^xsd:nonNegativeInteger), (?x owl:onProperty ?p),
            (?x owl:onClass owl:Thing), (?u rdf:type ?x), (?u ?p ?y1), (?u ?p ?y2), ?y1 != ?y2
            -> (?y1 owl:sameAs ?y2)

# Table 7: class axioms
cax-sco:  (?c1 rdfs:subClassOf ?c2), (?x rdf:type ?c1) -> (?x rdf:type ?c2)
cax-eqc1: (?c1 owl:equivalentClass ?c2), (?x rdf:type ?c1) -> (?x rdf:type ?c2)
cax-eqc2: (?c1 owl:equivalentClass ?c2), (?x rdf:type ?c2) -> (?x rdf:type ?c1)
cax-dw:   (?c1 owl:disjointWith ?c2), (?x rdf:type ?c1), (?x rdf:type ?c2) -> false

# Table 9: schema vocabulary
scm-cls:  (?c rdf:type owl:Class) -> (?c rdfs:subClassOf ?c), (?c owl:equivalentClass ?c),
          (?c rdfs:subClassOf owl:Thing), (owl:Nothing rdfs:subClassOf ?c)
scm-sco:  (?c1 rdfs:subClassOf ?c2), (?c2 rdfs:subClassOf ?c3) -> (?c1 rdfs:subClassOf ?c3)
scm-eqc1: (?c1 owl:equivalentClass ?c2) -> (?c1 rdfs:subClassOf ?c2), (?c2 rdfs:subClassOf ?c1)
scm-eqc2: (?c1 rdfs:subClassOf ?c2), (?c2 rdfs:subClassOf ?c1) -> (?c1 owl:equivalentClass ?c2)
scm-op:   (?p rdf:type owl:ObjectProperty) -> (?p rdfs:subPropertyOf ?p), (?p owl:equivalentProperty ?p)
scm-dp:   (?p rdf:type owl:DatatypeProperty) -> (?p rdfs:subPropertyOf ?p), (?p owl:equivalentProperty ?p)
scm-spo:  (?p1 rdfs:subPropertyOf ?p2), (?p2 rdfs:subPropertyOf ?p3) -> (?p1 rdfs:subPropertyOf ?p3)
scm-eqp1: (?p1 owl:equivalentProperty ?p2) -> (?p1 rdfs:subPropertyOf ?p2), (?p2 rdfs:subPropertyOf ?p1)
scm-eqp2: (?p1 rdfs:subPropertyOf ?p2), (?p2 rdfs:subPropertyOf ?p1) -> (?p1 owl:equivalentProperty ?p2)
scm-dom1: (?p rdfs:domain ?c1), (?c1 rdfs:subClassOf ?c2) -> (?p rdfs:domain ?c2)
scm-dom2: (?p2 rdfs:domain ?c), (?p1 rdfs:subPropertyOf ?p2) -> (?p1 rdfs:domain ?c)
scm-rng1: (?p rdfs:range ?c1), (?c1 rdfs:subClassOf ?c2) -> (?p rdfs:range ?c2)
scm-rng2: (?p2 rdfs:range ?c), (?p1 rdfs:subPropertyOf ?p2) -> (?p1 rdfs:range ?c)
scm-hv:   (?c1 owl:hasValue ?i), (?c1 owl:onProperty ?p1), (?c2 owl:hasValue ?i),
          (?c2 owl:onProperty ?p2), (?p1 rdfs:subPropertyOf ?p2) -> (?c1 rdfs:subClassOf ?c2)
scm-svf1: (?c1 owl:someValuesFrom ?y1), (?c1 owl:onProperty ?p), (?c2 owl:someValuesFrom ?y2),
          (?c2 owl:onProperty ?p), (?y1 rdfs:subClassOf ?y2) -> (?c1 rdfs:subClassOf ?c2)
scm-svf2: (?c1 owl:someValuesFrom ?y), (?c1 owl:onProperty ?p1), (?c2 owl:someValuesFrom ?y),
          (?c2 owl:onProperty ?p2), (?p1 rdfs:subPropertyOf ?p2) -> (?c1 rdfs:subClassOf ?c2)
scm-avf1: (?c1 owl:allValuesFrom ?y1), (?c1 owl:onProperty ?p), (?c2 owl:allValuesFrom ?y2),
          (?c2 owl:onProperty ?p), (?y1 rdfs:subClassOf ?y2) -> (?c1 rdfs:subClassOf ?c2)
scm-avf2: (?c1 owl:allValuesFrom ?y), (?c1 owl:onProperty ?p1), (?c2 owl:allValuesFrom ?y),
          (?c2 owl:onProperty ?p2), (?p1 rdfs:subPropertyOf ?p2) -> (?c2 rdfs:subClassOf ?c1)
"#;

#[cfg(test)]
mod tests {
    /// Spec (OWL 2 Direct Semantics §1): a declared annotation property's domain entails no
    /// membership. The local evaluation of the same facts without that declaration reads it
    /// as an object-property domain and derives the membership.
    #[test]
    fn a_local_ql_evaluation_reads_the_annotation_declarations_of_its_state() {
        use super::{LocalVocabulary, Ruleset, Vocabulary};
        use crate::batch::{Schema, materialise};

        let mut vocabulary = LocalVocabulary::default();
        let [note, doc, alice, bob] = ["note", "Doc", "alice", "bob"]
            .map(|name| vocabulary.iri(&format!("http://example.com/{name}")));
        let domain = vocabulary.iri("http://www.w3.org/2000/01/rdf-schema#domain");
        let rdf_type = vocabulary.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let annotation = vocabulary.iri("http://www.w3.org/2002/07/owl#AnnotationProperty");
        let schema = Schema::owl(&mut vocabulary);
        let asserted = [
            [note, domain, doc],
            [alice, note, bob],
            [note, rdf_type, annotation],
        ];
        for (facts, expected) in [(&asserted[..2], true), (&asserted[..], false)] {
            let rules = Ruleset::Owl2Ql.rules_for(&mut vocabulary, facts).unwrap();
            let result = materialise(facts, &rules, None, &schema);
            assert_eq!(
                result.derived.contains(&[alice, rdf_type, doc]),
                expected,
                "the declaration controls the domain inference"
            );
        }
    }

    use super::*;

    #[test]
    fn every_added_rule_is_in_the_owl2_rl_text() {
        for ruleset in ALL {
            let added = ruleset.added_rules();
            for name in ruleset.owl_rules().unwrap_or_default() {
                assert!(
                    added.iter().any(|rule| rule.name == *name),
                    "{}: {name} isn't an OWL 2 RL rule",
                    ruleset.name()
                );
            }
        }
    }

    /// OWL 2 Direct Semantics §2.2, OWL 2 RDF-Based Semantics Table 5.2 and RDF 1.1
    /// Semantics §5: `owl:Thing` is nonempty. Completing a subclass chain that puts it
    /// inside `owl:Nothing` makes the ontology inconsistent under both profiles.
    #[test]
    fn a_subclass_delta_making_owl_thing_empty_is_inconsistent() {
        use crate::delta::{MemoryBase, Rules, update};
        use crate::eval::Schema;
        use crate::ir::parse_triple;
        use crate::vocabulary::LocalVocabulary;

        let mut vocabulary = LocalVocabulary::default();
        let schema = Schema::owl(&mut vocabulary);
        let [thing, subclass, nothing] =
            parse_triple("owl:Thing rdfs:subClassOf owl:Nothing", &mut vocabulary).unwrap();
        let middle = vocabulary.iri("http://example.com/A");
        let left = [thing, subclass, middle];
        let right = [middle, subclass, nothing];
        let base = MemoryBase::new(&[left, right], &[]);
        let same_as = vocabulary.iri("http://www.w3.org/2002/07/owl#sameAs");
        let consistent = [Ruleset::Owl2Ql, Ruleset::Owl2Rl].map(|ruleset| {
            let rules = ruleset.rules(&mut vocabulary).unwrap();
            update(
                &base,
                &[right],
                &[],
                Rules {
                    rules: &rules,
                    lists: None,
                    schema: &schema,
                    equality: crate::equality_rules::of(&rules, same_as),
                },
                None,
            )
            .violations
            .consistent()
        });
        assert_eq!(
            consistent,
            [false, false],
            "completing the subclass chain must make the ontology inconsistent"
        );
    }

    /// A ruleset's fingerprint is FNV-1a (64-bit) over the semantics version, its name, its
    /// text, the name and text of each OWL 2 RL rule it adds, its axioms, and the name and
    /// version of each semantic part its compile uses, in that order: the value stored
    /// reasoning states carry, whether computed now or remembered.
    #[test]
    fn every_fingerprint_is_the_fnv1a_of_its_parts() {
        fn fnv1a(parts: &[&[u8]]) -> u64 {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in parts.iter().flat_map(|part| part.iter()) {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash
        }
        for ruleset in ALL {
            let version = SEMANTICS_VERSION.to_le_bytes();
            let added = ruleset.added_rules();
            let mut parts: Vec<&[u8]> = vec![
                &version,
                ruleset.name().as_bytes(),
                ruleset.text().as_bytes(),
            ];
            for rule in &added {
                parts.push(rule.name.as_bytes());
                parts.push(rule.source.as_bytes());
            }
            parts.push(ruleset.axioms().as_bytes());
            // The parts' versions, little-endian, as `SEMANTICS_VERSION`'s.
            let versions: Vec<(&str, [u8; 4])> = (ruleset.parts().iter())
                .map(|part| (part.version().0, part.version().1.to_le_bytes()))
                .collect();
            for (name, version) in &versions {
                parts.push(name.as_bytes());
                parts.push(version);
            }
            // Asked twice: the remembered value is the computed one.
            for _ in 0..2 {
                assert_eq!(ruleset.fingerprint(), fnv1a(&parts), "{}", ruleset.name());
            }
        }
    }

    /// Spec (reasoning-semantics.md, "Freshness"): both fingerprints include list
    /// semantics version 9. Only `owl2-ql` includes the annotation-reading version.
    /// The version 9 rule text would give `dc06866733200dca` for `owl2-ql` without
    /// that part. The expected values below were computed from the rule text.
    #[test]
    fn fingerprints_include_the_list_version_and_the_ql_annotation_part() {
        let [ql, rl] =
            [Ruleset::Owl2Ql, Ruleset::Owl2Rl].map(|r| format!("{:016x}", r.fingerprint()));
        assert_eq!(ql, "1653fb1d3428c636");
        assert_eq!(rl, "e9f63653de270f96");
    }
}
