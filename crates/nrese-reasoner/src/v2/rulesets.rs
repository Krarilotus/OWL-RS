//! Built-in rulesets (reasoner-v2 design §3.3), as data in the [`parse_rules`] syntax.
//!
//! `OWL2_RL` is the W3C OWL 2 RL/RDF rule tables (OWL 2 Profiles, 2nd ed., §4.3), with
//! the W3C rule names. Rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`,
//! `cls-oo`, `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) aren't in the
//! text: they're instantiated per list axiom by [`super::lists`]. Left out, as in owlrl's
//! default mode and the reasoning benchmark's normalisation: `eq-ref` (`x sameAs x` for
//! every term), the datatype rules (table 8) and the axiomatic triples.
//!
//! Every ruleset is validated by the tests: it parses, every rule is safe, and the
//! evaluator's closure equals the owlrl oracle's on the benchmark data.

use super::ir::{ParseError, Rule, Vocabulary, parse_rules};

/// The named rulesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruleset {
    /// RDFS entailment rules without axiomatic triples (GraphDB's `rdfs` with partialRDFS).
    Rdfs,
    /// The OWL 2 RL/RDF rules.
    Owl2Rl,
}

impl Ruleset {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rdfs => "rdfs",
            Self::Owl2Rl => "owl2-rl",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Rdfs => RDFS,
            Self::Owl2Rl => OWL2_RL,
        }
    }

    /// Whether the list-axiom rules apply (OWL 2 RL only).
    pub fn has_list_rules(self) -> bool {
        self == Self::Owl2Rl
    }

    pub fn rules(self, vocabulary: &mut impl Vocabulary) -> Result<Vec<Rule>, ParseError> {
        parse_rules(self.text(), vocabulary)
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

/// OWL 2 RL/RDF, tables 4-7 and 9 (list rules in `lists`).
pub const OWL2_RL: &str = r#"
# Table 4: equality
eq-sym:   (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)
eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)
eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)
eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)
eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)
eq-diff1: (?x owl:sameAs ?y), (?x owl:differentFrom ?y) -> false

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
