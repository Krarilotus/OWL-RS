//! Memberships in unnamed union classes (work package W7).
//!
//! Ontologies such as the GND's give properties an anonymous union as domain or range
//! (`gndo:placeOfActivity rdfs:domain [ owl:unionOf (…) ]`). OWL 2 RL then types every
//! subject into that class: with the GND ontology, 120,627 of 219,840 inferred statements
//! on the integration workload's example tier. Nobody can name the class in a query, and
//! when nothing but its definition uses it, no rule consumes the memberships either.
//!
//! [`hidden_classes`] finds such classes: blank nodes with an `owl:unionOf` that occur only
//! where memberships are produced (as the object of `rdfs:domain`, `rdfs:range`,
//! `rdf:type`, `rdfs:subClassOf` or `owl:allValuesFrom`) or in their own definition
//! (`owl:unionOf`, `rdf:type`). The schema then keeps the rules from deriving their
//! memberships ([`super::eval::Schema::hiding`]). One consequence would be lost: a class
//! declared `owl:Class` is a subclass of `owl:Thing` (scm-cls), so its members are
//! things; for those classes a membership becomes `owl:Thing` membership instead.
//!
//! The closure is the full closure without the hidden memberships (property test).

use std::collections::HashMap;

use super::ir::Triple;
use super::ir::{OWL, RDF, RDFS, Vocabulary};

/// The vocabulary the analysis reads.
pub struct UnnamedVocabulary {
    union_of: u64,
    rdf_type: u64,
    owl_class: u64,
    producing: [u64; 5],
}

impl UnnamedVocabulary {
    pub fn new(vocabulary: &mut impl Vocabulary) -> Self {
        let mut iri = |text: String| vocabulary.iri(&text);
        Self {
            union_of: iri(format!("{OWL}unionOf")),
            rdf_type: iri(format!("{RDF}type")),
            owl_class: iri(format!("{OWL}Class")),
            producing: [
                iri(format!("{RDFS}domain")),
                iri(format!("{RDFS}range")),
                iri(format!("{RDF}type")),
                iri(format!("{RDFS}subClassOf")),
                iri(format!("{OWL}allValuesFrom")),
            ],
        }
    }

    pub fn union_of(&self) -> u64 {
        self.union_of
    }
}

/// Whether `program` makes every member of a declared class an `owl:Thing`, as the rewrite of
/// hidden memberships assumes: asked of its rules with no guard of any kind, on a declared
/// class and a member, two blank nodes no rule can name. A rule with no guard carries over to
/// any store under any mapping of those two terms, and the whole program derives at least what
/// its guard-free part does; a program that makes things only through a guarded rule answers
/// false, and the memberships it hides are left out with nothing in their place. A process
/// past its memory limit interrupts the question: the interruption, never an answer.
pub fn members_are_things(program: &crate::RuleProgram) -> Result<bool, crate::delta::Interrupted> {
    let vocabulary = &mut crate::vocabulary::LocalVocabulary::default();
    let mut rules = program.rules(vocabulary).expect("checked at startup");
    rules.retain(|rule| rule.guards.is_empty());
    let mut input = program
        .axiom_triples(vocabulary)
        .expect("checked at startup");
    let terms = UnnamedVocabulary::new(vocabulary);
    let thing = vocabulary.iri(&format!("{OWL}Thing"));
    // Blank nodes: the top half of a local vocabulary's ids, where no constant is.
    let (class, member) = (1 << 63, (1 << 63) + 1);
    input.push([class, terms.rdf_type, terms.owl_class]);
    input.push([member, terms.rdf_type, class]);
    let schema = crate::eval::Schema::owl(vocabulary);
    let never = crate::eval::NEVER;
    let closure = crate::batch::materialise_owned_until(input, &rules, None, &schema, never)?;
    Ok(closure.derived.contains(&[member, terms.rdf_type, thing]))
}

/// The unnamed classes among `candidates` (subjects of `owl:unionOf` that are blank
/// nodes) whose memberships nothing consumes, each with whether its members are things:
/// it is declared `owl:Class` and the rules make its members things (`things`,
/// [`members_are_things`]). `mentions(c)` gives every fact `c` occurs in.
pub fn hidden_classes(
    candidates: impl IntoIterator<Item = u64>,
    mentions: &dyn Fn(u64) -> Vec<Triple>,
    vocabulary: &UnnamedVocabulary,
    things: bool,
) -> HashMap<u64, bool> {
    let mut out = HashMap::new();
    'classes: for class in candidates {
        let mut declared = false;
        for [s, p, o] in mentions(class) {
            let as_subject = s == class && (p == vocabulary.union_of || p == vocabulary.rdf_type);
            let as_object = o == class && vocabulary.producing.contains(&p);
            if s == class && p == vocabulary.rdf_type && o == vocabulary.owl_class {
                declared = true;
            }
            // A predicate position, or a use that consumes memberships.
            if p == class || !(as_subject || as_object) {
                continue 'classes;
            }
        }
        out.insert(class, declared && things);
    }
    out
}

/// [`hidden_classes`] over a list of facts; `blank` tells blank nodes from IRIs.
pub fn hidden_classes_in(
    facts: &[Triple],
    vocabulary: &UnnamedVocabulary,
    blank: &dyn Fn(u64) -> bool,
    things: bool,
) -> HashMap<u64, bool> {
    let candidates: Vec<u64> = facts
        .iter()
        .filter(|t| t[1] == vocabulary.union_of && blank(t[0]))
        .map(|t| t[0])
        .collect();
    let mut by_term: HashMap<u64, Vec<Triple>> = HashMap::new();
    for &fact in facts {
        for term in fact {
            if candidates.contains(&term) {
                let list = by_term.entry(term).or_default();
                if list.last() != Some(&fact) {
                    list.push(fact);
                }
            }
        }
    }
    hidden_classes(
        candidates,
        &|c| by_term.get(&c).cloned().unwrap_or_default(),
        vocabulary,
        things,
    )
}
