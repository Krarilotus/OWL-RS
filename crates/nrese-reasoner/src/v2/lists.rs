//! List axioms compiled to fixed-arity rules (reasoner-v2 design §3.2, step 3).
//!
//! OWL 2 RL rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`, `cls-oo`,
//! `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) can't be written as fixed rules.
//! Instead each list axiom in the data becomes rules of its own arity. A chain of length
//! n becomes one n-atom rule, and an intersection one rule per direction. That removes
//! the `rdf:first`/`rdf:rest` helper rules that generic rulesets need.
//!
//! Lists are read from the facts: every node needs an `rdf:first` and an `rdf:rest`, and
//! the list must end at `rdf:nil` without cycles. Several `rdf:first`s (from equality)
//! give several member sequences, each instantiated. Malformed or oversized lists produce
//! a diagnostic and no rules; they're never silently truncated.

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

    /// Whether `fact` is list structure or a list axiom: the facts [`instantiate`] reads.
    pub fn is_list_fact(&self, [_, p, o]: [u64; 3]) -> bool {
        [
            self.first,
            self.rest,
            self.property_chain_axiom,
            self.has_key,
            self.intersection_of,
            self.union_of,
            self.one_of,
            self.members,
            self.distinct_members,
        ]
        .contains(&p)
            || (p == self.rdf_type
                && [
                    self.all_disjoint_classes,
                    self.all_disjoint_properties,
                    self.all_different,
                ]
                .contains(&o))
    }

    /// Every member sequence of the list starting at `head`, or why it is malformed.
    ///
    /// Equality makes a node's `rdf:first` (or `rdf:rest`) ambiguous: `eq-rep-o` copies
    /// `(node rdf:first a)` to every `b` with `a owl:sameAs b`. The rules' `LIST[…]`
    /// pattern matches each resulting path, so every path is a variant, up to
    /// [`MAX_VARIANTS`]. A node without `rdf:first` or `rdf:rest`, or a cycle, is malformed.
    fn list(&self, facts: &impl Facts, head: u64) -> Result<Vec<Vec<u64>>, &'static str> {
        let mut variants = Vec::new();
        let mut path = Vec::new();
        let mut nodes = Vec::new();
        self.walk(facts, head, &mut nodes, &mut path, &mut variants)?;
        Ok(variants)
    }

    fn walk(
        &self,
        facts: &impl Facts,
        node: u64,
        nodes: &mut Vec<u64>,
        path: &mut Vec<u64>,
        variants: &mut Vec<Vec<u64>>,
    ) -> Result<(), &'static str> {
        if node == self.nil {
            if variants.len() == MAX_VARIANTS {
                return Err("too many variants under equality");
            }
            variants.push(path.clone());
            return Ok(());
        }
        if nodes.contains(&node) {
            return Err("a cycle");
        }
        let (firsts, rests) = (
            facts.objects(node, self.first),
            facts.objects(node, self.rest),
        );
        if firsts.is_empty() || rests.is_empty() {
            return Err("a node without rdf:first or rdf:rest");
        }
        nodes.push(node);
        for &first in &firsts {
            path.push(first);
            for &rest in &rests {
                self.walk(facts, rest, nodes, path, variants)?;
            }
            path.pop();
        }
        nodes.pop();
        Ok(())
    }
}

/// The most member sequences one list axiom may have (see [`ListVocabulary::list`]).
const MAX_VARIANTS: usize = 64;

/// The longest list instantiated; longer ones are diagnosed.
const MAX_MEMBERS: usize = 100;

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
                Ok(variants) => {
                    for members in variants {
                        if members.len() <= MAX_MEMBERS {
                            make(subject, &members, rules);
                        } else {
                            diagnostics.push(format!(
                                "{name}: list of {} members at node {head} skipped (limit {MAX_MEMBERS})",
                                members.len()
                            ));
                        }
                    }
                }
                Err(problem) => diagnostics.push(format!(
                    "{name}: list at node {head} (subject {subject}) skipped: {problem}"
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
