//! List axioms compiled to fixed-arity rules (reasoner-v2 design §3.2, step 3).
//!
//! OWL 2 RL rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`, `cls-oo`,
//! `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) can't be written as fixed rules.
//! Instead each list axiom in the data becomes rules of its own arity. A chain of length
//! n becomes one n-atom rule, and an intersection one rule per direction. That removes
//! the `rdf:first`/`rdf:rest` helper rules that generic rulesets need.
//!
//! Lists are read from the facts: every node needs exactly one `rdf:first` and one
//! `rdf:rest`, ending at `rdf:nil`, without cycles. Malformed lists produce a diagnostic
//! and no rules; they're never silently truncated.

use super::ir::{Atom, Guard, Head, OWL, RDF, Rule, Term, Vocabulary};

/// Read access to facts for instantiation.
pub trait Facts {
    /// Objects of `(subject, predicate, ?)`.
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64>;
    /// `(subject, object)` pairs of `(?, predicate, ?)`.
    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)>;
}

/// The vocabulary ids the list rules use.
pub struct ListVocabulary {
    rdf_type: u64,
    first: u64,
    rest: u64,
    nil: u64,
    same_as: u64,
    sub_class_of: u64,
    property_chain_axiom: u64,
    has_key: u64,
    intersection_of: u64,
    union_of: u64,
    one_of: u64,
    members: u64,
    distinct_members: u64,
    all_disjoint_classes: u64,
    all_disjoint_properties: u64,
    all_different: u64,
}

impl ListVocabulary {
    pub fn new(vocabulary: &mut impl Vocabulary) -> Self {
        let mut rdf = |local: &str| vocabulary.iri(&format!("{RDF}{local}"));
        let (rdf_type, first, rest, nil) = (rdf("type"), rdf("first"), rdf("rest"), rdf("nil"));
        let mut owl = |local: &str| vocabulary.iri(&format!("{OWL}{local}"));
        Self {
            rdf_type,
            first,
            rest,
            nil,
            same_as: owl("sameAs"),
            sub_class_of: vocabulary.iri("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
            property_chain_axiom: vocabulary.iri(&format!("{OWL}propertyChainAxiom")),
            has_key: vocabulary.iri(&format!("{OWL}hasKey")),
            intersection_of: vocabulary.iri(&format!("{OWL}intersectionOf")),
            union_of: vocabulary.iri(&format!("{OWL}unionOf")),
            one_of: vocabulary.iri(&format!("{OWL}oneOf")),
            members: vocabulary.iri(&format!("{OWL}members")),
            distinct_members: vocabulary.iri(&format!("{OWL}distinctMembers")),
            all_disjoint_classes: vocabulary.iri(&format!("{OWL}AllDisjointClasses")),
            all_disjoint_properties: vocabulary.iri(&format!("{OWL}AllDisjointProperties")),
            all_different: vocabulary.iri(&format!("{OWL}AllDifferent")),
        }
    }

    /// The members of the list starting at `head`, or `None` if it is malformed.
    fn list(&self, facts: &impl Facts, head: u64) -> Option<Vec<u64>> {
        let mut members = Vec::new();
        let mut node = head;
        let mut seen = std::collections::HashSet::new();
        while node != self.nil {
            if !seen.insert(node) {
                return None; // a cycle
            }
            let (first, rest) = (
                facts.objects(node, self.first),
                facts.objects(node, self.rest),
            );
            let ([first], [rest]) = (first.as_slice(), rest.as_slice()) else {
                return None;
            };
            members.push(*first);
            node = *rest;
        }
        Some(members)
    }
}

fn v(n: usize) -> Term {
    Term::Var(u8::try_from(n).expect("list rules stay below 256 variables"))
}

fn c(id: u64) -> Term {
    Term::Const(id)
}

fn rule(name: &str, body: Vec<Atom>, guards: Vec<Guard>, head: Head) -> Rule {
    Rule {
        name: name.to_owned(),
        body,
        guards,
        head,
    }
}

/// Turns one list axiom (its subject and members) into rules.
type MakeRules<'a> = dyn FnMut(u64, &[u64], &mut Vec<Rule>) + 'a;

/// The rules for every list axiom in `facts`, plus diagnostics for malformed lists.
pub fn instantiate(vocabulary: &ListVocabulary, facts: &impl Facts) -> (Vec<Rule>, Vec<String>) {
    let voc = vocabulary;
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    let mut lists = |predicate: u64, name: &str, rules: &mut Vec<Rule>, make: &mut MakeRules| {
        for (subject, head) in facts.pairs(predicate) {
            match voc.list(facts, head) {
                Some(members) if members.len() <= 100 => make(subject, &members, rules),
                Some(members) => diagnostics.push(format!(
                    "{name}: list of {} members skipped (limit 100)",
                    members.len()
                )),
                None => diagnostics.push(format!(
                    "{name}: malformed list at node {head} (subject {subject})"
                )),
            }
        }
    };
    let ty = voc.rdf_type;
    // prp-spo2: (x0 p1 x1) … (x(n-1) pn xn) -> (x0 p xn)
    lists(
        voc.property_chain_axiom,
        "prp-spo2",
        &mut rules,
        &mut |p, chain, rules| {
            if chain.is_empty() {
                return;
            }
            let body = chain
                .iter()
                .enumerate()
                .map(|(i, &pi)| Atom([v(i), c(pi), v(i + 1)]))
                .collect();
            rules.push(rule(
                "prp-spo2",
                body,
                vec![],
                Head::Facts(vec![Atom([v(0), c(p), v(chain.len())])]),
            ));
        },
    );
    // prp-key: two instances of c agreeing on every key property are the same.
    lists(
        voc.has_key,
        "prp-key",
        &mut rules,
        &mut |class, keys, rules| {
            if keys.len() > 60 {
                return;
            }
            let (x, y) = (v(0), v(1));
            let mut body = vec![Atom([x, c(ty), c(class)]), Atom([y, c(ty), c(class)])];
            for (i, &p) in keys.iter().enumerate() {
                body.push(Atom([x, c(p), v(i + 2)]));
                body.push(Atom([y, c(p), v(i + 2)]));
            }
            rules.push(rule(
                "prp-key",
                body,
                vec![Guard::NotEqual(x, y)],
                Head::Facts(vec![Atom([x, c(voc.same_as), y])]),
            ));
        },
    );
    // cls-int1, cls-int2, scm-int
    lists(
        voc.intersection_of,
        "cls-int",
        &mut rules,
        &mut |class, members, rules| {
            let body = members
                .iter()
                .map(|&ci| Atom([v(0), c(ty), c(ci)]))
                .collect();
            rules.push(rule(
                "cls-int1",
                body,
                vec![],
                Head::Facts(vec![Atom([v(0), c(ty), c(class)])]),
            ));
            for &ci in members {
                rules.push(rule(
                    "cls-int2",
                    vec![Atom([v(0), c(ty), c(class)])],
                    vec![],
                    Head::Facts(vec![Atom([v(0), c(ty), c(ci)])]),
                ));
                rules.push(rule(
                    "scm-int",
                    vec![],
                    vec![],
                    Head::Facts(vec![Atom([c(class), c(voc.sub_class_of), c(ci)])]),
                ));
            }
        },
    );
    // cls-uni, scm-uni
    lists(
        voc.union_of,
        "cls-uni",
        &mut rules,
        &mut |class, members, rules| {
            for &ci in members {
                rules.push(rule(
                    "cls-uni",
                    vec![Atom([v(0), c(ty), c(ci)])],
                    vec![],
                    Head::Facts(vec![Atom([v(0), c(ty), c(class)])]),
                ));
                rules.push(rule(
                    "scm-uni",
                    vec![],
                    vec![],
                    Head::Facts(vec![Atom([c(ci), c(voc.sub_class_of), c(class)])]),
                ));
            }
        },
    );
    // cls-oo: every listed individual is an instance.
    lists(
        voc.one_of,
        "cls-oo",
        &mut rules,
        &mut |class, members, rules| {
            let heads = members
                .iter()
                .map(|&y| Atom([c(y), c(ty), c(class)]))
                .collect();
            rules.push(rule("cls-oo", vec![], vec![], Head::Facts(heads)));
        },
    );
    // cax-adc, eq-diff2, prp-adp: pairwise over `members`, by the type of the axiom node.
    let typed = |node: u64, class: u64| facts.objects(node, ty).contains(&class);
    lists(
        voc.members,
        "members",
        &mut rules,
        &mut |node, members, rules| {
            for (i, &a) in members.iter().enumerate() {
                for &b in &members[i + 1..] {
                    if typed(node, voc.all_disjoint_classes) {
                        rules.push(rule(
                            "cax-adc",
                            vec![Atom([v(0), c(ty), c(a)]), Atom([v(0), c(ty), c(b)])],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                    if typed(node, voc.all_different) {
                        rules.push(rule(
                            "eq-diff2",
                            vec![Atom([c(a), c(voc.same_as), c(b)])],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                    if typed(node, voc.all_disjoint_properties) {
                        rules.push(rule(
                            "prp-adp",
                            vec![Atom([v(0), c(a), v(1)]), Atom([v(0), c(b), v(1)])],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                }
            }
        },
    );
    lists(
        voc.distinct_members,
        "eq-diff3",
        &mut rules,
        &mut |node, members, rules| {
            if !typed(node, voc.all_different) {
                return;
            }
            for (i, &a) in members.iter().enumerate() {
                for &b in &members[i + 1..] {
                    rules.push(rule(
                        "eq-diff3",
                        vec![Atom([c(a), c(voc.same_as), c(b)])],
                        vec![],
                        Head::Inconsistent,
                    ));
                }
            }
        },
    );
    (rules, diagnostics)
}
