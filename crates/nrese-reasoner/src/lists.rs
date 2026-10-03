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
//! give several member sequences, each instantiated. Malformed lists produce a diagnostic
//! and no rules; they're never silently truncated.
//!
//! Lists of any length are instantiated. The pairwise axioms (`owl:AllDifferent`,
//! `owl:AllDisjointClasses`, `owl:AllDisjointProperties`) would need a rule per pair, so
//! their lists longer than [`PAIRWISE_MEMBERS`] become one rule per kind of axiom whose
//! [`Guard::SameList`] asks an index of the members which lists they share: linear in the
//! members however many there are. The only length limit left is the rule format's: a
//! property chain or a key over more than [`MAX_RULE_MEMBERS`] properties can't be one
//! rule (variables are numbered by `u8`) and is diagnosed.

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
    /// Each variant comes with the `rdf:first`/`rdf:rest` facts of its path.
    fn list(&self, facts: &impl Facts, head: u64) -> Result<Vec<Variant>, ListProblem> {
        let mut variants = Vec::new();
        let mut path = Vec::new();
        let mut nodes = Vec::new();
        let mut used = Vec::new();
        self.walk(facts, head, &mut nodes, &mut path, &mut used, &mut variants)?;
        Ok(variants)
    }

    fn walk(
        &self,
        facts: &impl Facts,
        node: u64,
        nodes: &mut Vec<u64>,
        path: &mut Vec<u64>,
        used: &mut Vec<[u64; 3]>,
        variants: &mut Vec<Variant>,
    ) -> Result<(), ListProblem> {
        if node == self.nil {
            if variants.len() == MAX_VARIANTS {
                return Err(ListProblem::TooManyVariants);
            }
            variants.push((path.clone(), used.clone()));
            return Ok(());
        }
        if nodes.contains(&node) {
            return Err(ListProblem::Cycle { node });
        }
        let (firsts, rests) = (
            facts.objects(node, self.first),
            facts.objects(node, self.rest),
        );
        if firsts.is_empty() || rests.is_empty() {
            return Err(ListProblem::Malformed { node });
        }
        nodes.push(node);
        for &first in &firsts {
            path.push(first);
            used.push([node, self.first, first]);
            for &rest in &rests {
                used.push([node, self.rest, rest]);
                self.walk(facts, rest, nodes, path, used, variants)?;
                used.pop();
            }
            used.pop();
            path.pop();
        }
        nodes.pop();
        Ok(())
    }
}

/// A member sequence of a list, with the facts its path uses.
type Variant = (Vec<u64>, Vec<[u64; 3]>);

/// The most member sequences one list axiom may have (see `ListVocabulary::list`).
pub const MAX_VARIANTS: usize = 64;

/// Lists of pairwise axioms up to this length become a rule per pair; longer ones are
/// checked through a [`super::ir::ListIndex`] (an implementation choice, not a limit).
pub const PAIRWISE_MEMBERS: usize = 100;

/// The most properties of a chain or a key that one rule can hold (its variables are
/// numbered by `u8`).
pub const MAX_RULE_MEMBERS: usize = 250;

/// Why a list axiom wasn't instantiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListProblem {
    /// `node` lacks `rdf:first` or `rdf:rest` (the list doesn't end at `rdf:nil`).
    Malformed { node: u64 },
    /// The list returns to `node`.
    Cycle { node: u64 },
    /// Equality gives the list more than [`MAX_VARIANTS`] member sequences.
    TooManyVariants,
    /// A property chain or key with more than [`MAX_RULE_MEMBERS`] properties.
    TooLong { members: usize },
}

impl ListProblem {
    /// A stable name for reports: `malformed-list`, `cyclic-list`,
    /// `too-many-list-variants` or `list-too-long`.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Malformed { .. } => "malformed-list",
            Self::Cycle { .. } => "cyclic-list",
            Self::TooManyVariants => "too-many-list-variants",
            Self::TooLong { .. } => "list-too-long",
        }
    }

    /// The node the problem is at, if one.
    pub const fn node(self) -> Option<u64> {
        match self {
            Self::Malformed { node } | Self::Cycle { node } => Some(node),
            Self::TooManyVariants | Self::TooLong { .. } => None,
        }
    }
}

/// A list axiom the reasoner skipped: its rules are missing from the closure. Lists are
/// never truncated, so this is the whole effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListDiagnostic {
    /// The OWL 2 RL rules the axiom feeds (`prp-spo2`, `cls-int`, …).
    pub rules: &'static str,
    /// The axiom `(subject predicate head)`; `head` starts the list.
    pub subject: u64,
    pub predicate: u64,
    pub head: u64,
    pub problem: ListProblem,
}

impl ListDiagnostic {
    /// A sentence for reports, with `term` rendering ids.
    pub fn describe(&self, term: &dyn Fn(u64) -> String) -> String {
        let problem = match self.problem {
            ListProblem::Malformed { node } => {
                format!("node {} has no rdf:first or no rdf:rest", term(node))
            }
            ListProblem::Cycle { node } => format!("the list returns to node {}", term(node)),
            ListProblem::TooManyVariants => {
                format!("owl:sameAs gives it more than {MAX_VARIANTS} member sequences")
            }
            ListProblem::TooLong { members } => {
                format!("it has {members} properties, more than a rule holds ({MAX_RULE_MEMBERS})")
            }
        };
        format!(
            "{} {} {} skipped ({}): {problem}",
            term(self.subject),
            term(self.predicate),
            term(self.head),
            self.rules
        )
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
pub fn instantiate(
    vocabulary: &ListVocabulary,
    facts: &impl Facts,
) -> (Vec<Rule>, Vec<ListDiagnostic>) {
    let (rules, _, diagnostics) = instantiate_with_premises(vocabulary, facts);
    (rules, diagnostics)
}

/// [`instantiate`], with the facts each rule comes from (its premises): the axiom, the
/// list path's `rdf:first`/`rdf:rest` facts, and the axiom node's `rdf:type` facts for
/// `owl:members` / `owl:distinctMembers` axioms.
pub fn instantiate_with_premises(
    vocabulary: &ListVocabulary,
    facts: &impl Facts,
) -> (Vec<Rule>, Vec<Vec<[u64; 3]>>, Vec<ListDiagnostic>) {
    let voc = vocabulary;
    let mut rules = Vec::new();
    let mut premises: Vec<Vec<[u64; 3]>> = Vec::new();
    let mut diagnostics: Vec<ListDiagnostic> = Vec::new();
    let mut lists =
        |predicate: u64, name: &'static str, rules: &mut Vec<Rule>, make: &mut MakeRules| {
            for (subject, head) in facts.pairs(predicate) {
                let mut diagnose = |problem| {
                    let diagnostic = ListDiagnostic {
                        rules: name,
                        subject,
                        predicate,
                        head,
                        problem,
                    };
                    if !diagnostics.contains(&diagnostic) {
                        diagnostics.push(diagnostic);
                    }
                };
                match voc.list(facts, head) {
                    Ok(variants) => {
                        for (members, used) in variants {
                            let rule_sized = !(predicate == voc.property_chain_axiom
                                || predicate == voc.has_key)
                                || members.len() <= MAX_RULE_MEMBERS;
                            if rule_sized {
                                let before = rules.len();
                                make(subject, &members, rules);
                                let mut facts_used = used;
                                facts_used.push([subject, predicate, head]);
                                if predicate == voc.members || predicate == voc.distinct_members {
                                    for class in [
                                        voc.all_disjoint_classes,
                                        voc.all_disjoint_properties,
                                        voc.all_different,
                                    ] {
                                        if facts.objects(subject, voc.rdf_type).contains(&class) {
                                            facts_used.push([subject, voc.rdf_type, class]);
                                        }
                                    }
                                }
                                for _ in before..rules.len() {
                                    premises.push(facts_used.clone());
                                }
                            } else {
                                diagnose(ListProblem::TooLong {
                                    members: members.len(),
                                });
                            }
                        }
                    }
                    Err(problem) => diagnose(problem),
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
    // The premise that makes two members of an AllDifferent axiom inconsistent: `a sameAs b`,
    // or, when the list names one individual twice, the axiom itself (`a sameAs a` holds by
    // eq-ref, which isn't materialised).
    let different_members = |a: u64, b: u64, node: u64| {
        if a == b {
            Atom([c(node), c(ty), c(voc.all_different)])
        } else {
            Atom([c(a), c(voc.same_as), c(b)])
        }
    };
    // Long lists of the pairwise axioms, by kind: their members indexed, and the axioms'
    // type facts (the rules' schema premises).
    let long = std::cell::RefCell::new(Long::default());
    lists(
        voc.members,
        "cax-adc, prp-adp, eq-diff2",
        &mut rules,
        &mut |node, members, rules| {
            if members.len() > PAIRWISE_MEMBERS {
                let mut long = long.borrow_mut();
                for (kind, index, premises) in [
                    (voc.all_disjoint_classes, 0, 0),
                    (voc.all_different, 1, 1),
                    (voc.all_disjoint_properties, 2, 2),
                ] {
                    if typed(node, kind) {
                        long.indexes[index].add(members);
                        long.premises[premises].push([node, ty, kind]);
                        if kind == voc.all_different {
                            duplicates(members, node, voc, rules, "eq-diff2");
                        }
                    }
                }
                return;
            }
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
                            vec![different_members(a, b, node)],
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
            if members.len() > PAIRWISE_MEMBERS {
                let mut long = long.borrow_mut();
                long.indexes[3].add(members);
                long.premises[3].push([node, ty, voc.all_different]);
                duplicates(members, node, voc, rules, "eq-diff3");
                return;
            }
            for (i, &a) in members.iter().enumerate() {
                for &b in &members[i + 1..] {
                    rules.push(rule(
                        "eq-diff3",
                        vec![different_members(a, b, node)],
                        vec![],
                        Head::Inconsistent,
                    ));
                }
            }
        },
    );
    // One rule per kind of long axiom, its pairs checked by the index.
    let long = long.into_inner();
    let [classes, different, properties, distinct] = long.indexes;
    let shared = super::ir::SharedListIndex::new;
    let mut native = |name: &str, body: Vec<Atom>, x: Term, y: Term, index, used: Vec<[u64; 3]>| {
        rules.push(rule(
            name,
            body,
            vec![Guard::NotEqual(x, y), Guard::SameList(x, y, shared(index))],
            Head::Inconsistent,
        ));
        premises.push(used);
    };
    let [p_classes, p_different, p_properties, p_distinct] = long.premises;
    if !classes.is_empty() {
        native(
            "cax-adc",
            vec![Atom([v(0), c(ty), v(1)]), Atom([v(0), c(ty), v(2)])],
            v(1),
            v(2),
            classes,
            p_classes,
        );
    }
    for (index, used, name) in [
        (different, p_different, "eq-diff2"),
        (distinct, p_distinct, "eq-diff3"),
    ] {
        if !index.is_empty() {
            native(
                name,
                vec![Atom([v(0), c(voc.same_as), v(1)])],
                v(0),
                v(1),
                index,
                used,
            );
        }
    }
    if !properties.is_empty() {
        native(
            "prp-adp",
            vec![Atom([v(0), v(1), v(2)]), Atom([v(0), v(3), v(2)])],
            v(1),
            v(3),
            properties,
            p_properties,
        );
    }
    (rules, premises, diagnostics)
}

/// The long lists of the pairwise axioms ([`PAIRWISE_MEMBERS`]): disjoint classes,
/// different individuals (`owl:members`), disjoint properties, different individuals
/// (`owl:distinctMembers`); their indexes and the axioms' type facts.
#[derive(Default)]
struct Long {
    indexes: [super::ir::ListIndex; 4],
    premises: [Vec<[u64; 3]>; 4],
}

/// A long `owl:AllDifferent` list that names an individual twice is inconsistent by
/// itself (`a sameAs a` holds by eq-ref, which isn't materialised): the axiom's rule.
fn duplicates(members: &[u64], node: u64, voc: &ListVocabulary, rules: &mut Vec<Rule>, name: &str) {
    let mut seen = std::collections::HashSet::new();
    for &member in members {
        if !seen.insert(member) {
            rules.push(rule(
                name,
                vec![Atom([c(node), c(voc.rdf_type), c(voc.all_different)])],
                vec![],
                Head::Inconsistent,
            ));
            return;
        }
    }
}
