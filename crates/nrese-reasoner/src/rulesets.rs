//! Built-in rulesets (reasoner-v2 design §3.3), as data in the [`parse_rules`] syntax.
//!
//! `OWL2_RL` is the W3C OWL 2 RL/RDF rule tables (OWL 2 Profiles, 2nd ed., §4.3), with
//! the W3C rule names. Rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`,
//! `cls-oo`, `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) aren't in the
//! text: they're instantiated per list axiom by [`super::lists`]. Left out, as in owlrl's
//! default mode and the reasoning benchmark's normalisation: `eq-ref` (`x sameAs x` for
//! every term), the datatype rules (table 8) and the axiomatic triples. What `eq-ref`
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
//! | `owl2-ql` | what OWL 2 QL axioms entail without inventing individuals: hierarchies, domains, ranges, inverses, reflexive properties, `someValuesFrom owl:Thing`, and the disjointness checks |
//! | `owl2-rl` | the OWL 2 RL/RDF rules |
//!
//! Every ruleset is validated by the tests: it parses, every rule is safe, and the
//! evaluator's closure equals the owlrl oracle's on the benchmark data.

use super::ir::{
    ParseError, Rule, RuleSource, Vocabulary, parse_rules, parse_sources, rule_sources,
};

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

/// Version of the evaluation semantics beyond the rule text: list compilation, equality
/// handling, modules. Bump it with any change that alters what a ruleset derives or rejects,
/// so stores materialised by an older build rebuild instead of trusting their state.
pub const SEMANTICS_VERSION: u32 = 2;

impl Ruleset {
    /// Identifies what this ruleset derives: FNV-1a over [`SEMANTICS_VERSION`], the name,
    /// the rule text, and the name and text of every OWL 2 RL rule it adds (the bodies it
    /// compiles, so editing one changes the fingerprint of every ruleset using it: the
    /// review of 3 October 2026, C6). Stable across builds and platforms (no std hasher,
    /// which may change).
    pub fn fingerprint(self) -> u64 {
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
        hash
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
            Self::Owl2Rl => OWL2_RL_DATATYPE_AXIOMS,
            _ => "",
        }
    }

    /// Whether the list-axiom rules apply (OWL 2 RL only).
    pub fn has_list_rules(self) -> bool {
        self == Self::Owl2Rl
    }

    pub fn rules(self, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
        let mut rules = parse_rules(self.text(), vocabulary)?;
        rules.extend(parse_sources(self.added_rules(), vocabulary)?);
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
    "cls-nothing2",
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

/// OWL 2 RL/RDF, tables 4-7 and 9 (list rules in `lists`).
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
}
