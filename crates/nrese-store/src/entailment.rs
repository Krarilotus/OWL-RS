//! Whether the store's data entails an RDF graph under its reasoning rules
//! ([`StoreService::entails`]): the OWL 2 entailment check of the W3C test cases, and of
//! anyone asking "does it follow?".
//!
//! - **Positive statements** are looked up in the closure: an ASK over the materialised
//!   data, blank nodes read as variables, the ontology header left out. For OWL 2 RL this
//!   is complete for ground atomic conclusions from RL premises (theorem PR1 of OWL 2
//!   Profiles §4.3).
//! - **Negative statements** the rules never derive are decided by refutation: their
//!   opposite is added in a speculative transaction (never committed), the reasoner
//!   maintains the closure over it, and a violated consistency rule refutes the opposite.
//!   The forms recognised:
//!   - `a owl:differentFrom b`, by adding `a owl:sameAs b`;
//!   - `owl:AllDifferent` with `owl:members` or `owl:distinctMembers`, each pair so;
//!   - `a rdf:type [owl:complementOf C]`, by adding `a rdf:type C`;
//!   - an `owl:NegativePropertyAssertion`, by adding the assertion.
//! - **An inconsistent premise** entails everything.
//!
//! The answer is sound; it is complete as far as the rules are for the premise (OWL 2 RL
//! premises and the forms above). A conclusion outside them may be entailed although the
//! answer is no.

use std::collections::HashSet;

use nrese_rdf::vocab::{owl, rdf};
use nrese_rdf::{GraphName, NamedOrBlankNode, Quad, Term, Triple};
use nrese_reasoner::RuleProgram;
use nrese_sparql::{QueryOptions, QueryResults};
use nrese_sparql_syntax::SparqlParser;

use crate::error::{StoreError, StoreResult};
use crate::service::StoreService;

/// Why a conclusion is entailed, or that it isn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entailment {
    /// The data is inconsistent under the rules: it entails everything.
    InconsistentPremise,
    /// Every statement is in the closure, and every negative one was refuted.
    Entailed,
    /// Some statement is neither in the closure nor refuted.
    NotEntailed,
}

impl Entailment {
    pub fn holds(self) -> bool {
        self != Self::NotEntailed
    }
}

/// A conclusion split into what the closure answers and what refutation does.
#[derive(Debug, Default)]
struct Split {
    positive: Vec<Triple>,
    /// Sets of statements whose addition must make the data inconsistent.
    refutations: Vec<Vec<Triple>>,
}

impl StoreService {
    /// Whether the data entails `conclusion` under `program`: see the module docs. The
    /// inferred statements must be `program`'s closure ([`Self::reasoning_is_current`]);
    /// otherwise the answer would be the closure's of other rules, and the call fails.
    pub fn entails(
        &self,
        program: impl Into<RuleProgram>,
        conclusion: &[Triple],
    ) -> StoreResult<Entailment> {
        let program = program.into();
        if !self.reasoning_is_current(program.clone()) {
            return Err(StoreError::Configuration(format!(
                "the inferred statements aren't {}'s closure: rematerialise first",
                program.name()
            )));
        }
        if matches!(
            self.consistency(),
            crate::ConsistencyStatus::Inconsistent { .. }
        ) {
            return Ok(Entailment::InconsistentPremise);
        }
        let split = split(conclusion);
        if !split.positive.is_empty() && !self.ask(&split.positive)? {
            return Ok(Entailment::NotEntailed);
        }
        for opposite in &split.refutations {
            if !self.refutes(&program, opposite) {
                return Ok(Entailment::NotEntailed);
            }
        }
        Ok(Entailment::Entailed)
    }

    /// Whether the materialised data matches `pattern`, blank nodes read as variables.
    fn ask(&self, pattern: &[Triple]) -> StoreResult<bool> {
        let term = |t: &Term| match t {
            Term::BlankNode(b) => format!("?b{}", hex(b.as_str())),
            other => other.to_string(),
        };
        let patterns: Vec<String> = pattern
            .iter()
            .map(|t| {
                let subject: Term = t.subject.clone().into();
                format!("{} {} {} .", term(&subject), t.predicate, term(&t.object))
            })
            .collect();
        let text = format!("ASK {{ {} }}", patterns.join(" "));
        let query = SparqlParser::new()
            .parse_query(&text)
            .map_err(|error| StoreError::Configuration(format!("{error}: {text}")))?;
        let snapshot = self.engine().snapshot();
        match nrese_sparql::evaluate_query(&snapshot, &query, &QueryOptions::default())? {
            QueryResults::Boolean(found) => Ok(found),
            _ => unreachable!("an ASK answers with a boolean"),
        }
    }

    /// Whether adding `opposite` to the data makes it inconsistent under `program`: the
    /// reasoner maintains the closure over a speculative transaction, which is dropped.
    fn refutes(&self, program: &RuleProgram, opposite: &[Triple]) -> bool {
        let config = self.config();
        let mut tx = self.engine().speculative();
        for t in opposite {
            let quad = Quad::new(
                t.subject.clone(),
                t.predicate.clone(),
                t.object.clone(),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        let compiled = crate::reasoning::Program::compile(program, &|term| tx.intern(term))
            .hiding_unnamed_classes(config.hide_unnamed_classes)
            .by_representatives(config.equality_by_representatives)
            .storing_representatives(config.equality_compact);
        let done =
            nrese_reasoner::engine::maintain(&compiled, None, &mut tx, nrese_reasoner::eval::NEVER)
                .expect("never stopped");
        !done.violations.is_empty()
    }
}

/// A blank node label as a variable name.
fn hex(label: &str) -> String {
    label
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => char::from(b).to_string(),
            _ => format!("_{b:02x}"),
        })
        .collect()
}

/// `conclusion` without its ontology header, its negative forms turned into refutations.
fn split(conclusion: &[Triple]) -> Split {
    // The statements about `node`, by index.
    let about = |node: &NamedOrBlankNode| -> Vec<usize> {
        (0..conclusion.len())
            .filter(|&i| &conclusion[i].subject == node)
            .collect()
    };
    let object_of = |node: &NamedOrBlankNode, predicate: nrese_rdf::NamedNodeRef<'_>| {
        conclusion
            .iter()
            .find(|t| &t.subject == node && t.predicate.as_ref() == predicate)
            .map(|t| &t.object)
    };
    let is_type = |t: &Triple, class: nrese_rdf::NamedNodeRef<'_>| {
        t.predicate.as_ref() == rdf::TYPE
            && matches!(&t.object, Term::NamedNode(n) if n.as_ref() == class)
    };
    // The statements a negative form or the header consumed, by index.
    let mut used: HashSet<usize> = HashSet::new();
    let mut split = Split::default();
    let same_as = |a: &Term, b: &Term| -> Option<Triple> {
        let subject = match a {
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
            _ => return None,
        };
        Some(Triple::new(subject, owl::SAME_AS.into_owned(), b.clone()))
    };
    for (i, t) in conclusion.iter().enumerate() {
        // The ontology header: everything said about the owl:Ontology node.
        if is_type(t, nrese_rdf::NamedNodeRef::new_unchecked(OWL_ONTOLOGY)) {
            used.extend(about(&t.subject));
        }
        // a owl:differentFrom b, both named.
        if t.predicate.as_ref() == owl::DIFFERENT_FROM
            && matches!(t.subject, NamedOrBlankNode::NamedNode(_))
            && matches!(t.object, Term::NamedNode(_))
            && let Some(opposite) = same_as(&t.subject.clone().into(), &t.object)
        {
            split.refutations.push(vec![opposite]);
            used.insert(i);
        }
        // [] a owl:AllDifferent; owl:members (a b ...).
        if is_type(t, owl::ALL_DIFFERENT) {
            let list = object_of(&t.subject, owl::MEMBERS)
                .or_else(|| object_of(&t.subject, owl::DISTINCT_MEMBERS));
            if let Some((members, nodes)) = list.and_then(|head| list_items(conclusion, head)) {
                for (i, a) in members.iter().enumerate() {
                    for b in &members[i + 1..] {
                        if let Some(opposite) = same_as(a, b) {
                            split.refutations.push(vec![opposite]);
                        }
                    }
                }
                used.extend(about(&t.subject));
                for node in &nodes {
                    used.extend(about(node));
                }
            }
        }
        // a rdf:type _:c . _:c owl:complementOf C
        if t.predicate.as_ref() == rdf::TYPE
            && let Term::BlankNode(c) = &t.object
        {
            let class = NamedOrBlankNode::BlankNode(c.clone());
            if let Some(Term::NamedNode(complemented)) = object_of(&class, owl::COMPLEMENT_OF) {
                split.refutations.push(vec![Triple::new(
                    t.subject.clone(),
                    rdf::TYPE.into_owned(),
                    complemented.clone(),
                )]);
                used.insert(i);
                used.extend(about(&class));
            }
        }
        // [] a owl:NegativePropertyAssertion; source, property, target (or value).
        if is_type(t, owl::NEGATIVE_PROPERTY_ASSERTION) {
            let node = &t.subject;
            let source = object_of(node, owl::SOURCE_INDIVIDUAL);
            let property = object_of(node, owl::ASSERTION_PROPERTY);
            let target = object_of(node, owl::TARGET_INDIVIDUAL)
                .or_else(|| object_of(node, owl::TARGET_VALUE));
            if let (Some(Term::NamedNode(a)), Some(Term::NamedNode(p)), Some(b)) =
                (source, property, target)
            {
                split
                    .refutations
                    .push(vec![Triple::new(a.clone(), p.clone(), b.clone())]);
                used.extend(about(node));
            }
        }
    }
    split.positive = conclusion
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, t)| t.clone())
        .collect();
    split
}

const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";

/// The items of the RDF list at `head` in `triples`, and its nodes; `None` if malformed.
fn list_items(triples: &[Triple], head: &Term) -> Option<(Vec<Term>, Vec<NamedOrBlankNode>)> {
    let mut items = Vec::new();
    let mut nodes = Vec::new();
    let mut node = head.clone();
    while node != Term::NamedNode(rdf::NIL.into_owned()) {
        let subject = match &node {
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
            _ => return None,
        };
        let get = |p: nrese_rdf::NamedNodeRef<'_>| {
            triples
                .iter()
                .find(|t| t.subject == subject && t.predicate.as_ref() == p)
                .map(|t| t.object.clone())
        };
        items.push(get(rdf::FIRST)?);
        let rest = get(rdf::REST)?;
        if nodes.contains(&subject) {
            return None;
        }
        nodes.push(subject);
        node = rest;
    }
    Some((items, nodes))
}

/// Whether the store's asserted ontology entails a document under OWL 2 DL
/// ([`StoreService::entails_dl`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlEntailment {
    pub answer: crate::dl::entailment::Entailed,
    /// The premise's consistency: an inconsistent one entails everything.
    pub premise: crate::dl::Verdict,
}

impl DlEntailment {
    pub fn holds(&self) -> bool {
        self.premise == crate::dl::Verdict::Inconsistent
            || self.answer == crate::dl::entailment::Entailed::Yes
    }
}

/// Blank nodes of conclusions get labels of their own, so none is a premise's.
static CONCLUSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl StoreService {
    /// Whether the asserted statements (every graph) entail `conclusion` under the OWL 2
    /// Direct Semantics: each logical axiom of the conclusion read as an OWL 2 ontology
    /// (declarations and annotations hold no logical content), decided by the DL engines
    /// ([`crate::dl::entailment`]) within `dl.timeout` per test. A conclusion that isn't
    /// well-formed OWL 2 DL is `unknown`. Reads only; never commits.
    pub fn entails_dl(&self, conclusion: &[Triple]) -> StoreResult<DlEntailment> {
        use crate::dl::entailment::{Entailed, entails_ontology};
        use crate::dl::{Verdict, consistency, gate, source};
        use nrese_rdf::{BlankNode, NamedNodeRef, NamedOrBlankNode};
        let snapshot = self.engine().snapshot();
        let premise = source::read_snapshot(&snapshot);
        let budget = gate::budget(self, None);
        let checked = consistency::check(&premise, &budget);
        if checked.verdict != Verdict::Consistent {
            let answer = match &checked.verdict {
                Verdict::Unknown(why) => Entailed::Unknown(format!("the premise: {why}")),
                _ => Entailed::Yes,
            };
            return Ok(DlEntailment {
                answer,
                premise: checked.verdict,
            });
        }
        let tx = self.engine().speculative();
        source::intern_vocabulary(&tx);
        let n = CONCLUSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let blank = |b: &BlankNode| BlankNode::new_unchecked(format!("nresec{n}x{}", b.as_str()));
        let id = |t: Term| -> u64 {
            let t = match t {
                Term::BlankNode(b) => Term::BlankNode(blank(&b)),
                other => other,
            };
            tx.intern(t.as_ref()).raw()
        };
        let statements: Vec<nrese_owl::Statement> = conclusion
            .iter()
            .map(|t| {
                let subject: Term = match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                    NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
                };
                nrese_owl::Statement {
                    triple: [
                        id(subject),
                        id(Term::NamedNode(t.predicate.clone())),
                        id(t.object.clone()),
                    ],
                    graph: nrese_engine::TermId::DEFAULT_GRAPH.raw(),
                }
            })
            .collect();
        let decode = |term| tx.decode(term);
        let lookup = |iri: &str| tx.lookup(NamedNodeRef::new_unchecked(iri).into());
        let read = nrese_owl::read(
            &statements,
            &source::StoreTerms {
                decode: &decode,
                lookup: &lookup,
            },
        );
        // A structure no axiom uses says nothing under the Direct Semantics (OWL 1
        // conclusions state expressions so); anything else unread leaves it undecided.
        if let Some(diagnostic) = read.diagnostics.iter().find(|d| d.is_fatal()) {
            return Ok(DlEntailment {
                answer: Entailed::Unknown(format!(
                    "the conclusion isn't well-formed OWL 2 DL: {diagnostic:?}"
                )),
                premise: checked.verdict,
            });
        }
        let fresh: Vec<u64> = (0..8)
            .map(|i| {
                tx.intern(NamedNodeRef::new_unchecked(&format!("urn:nrese:dl:fresh:{i}")).into())
                    .raw()
            })
            .collect();
        Ok(DlEntailment {
            answer: entails_ontology(&premise, &read, &fresh, &budget),
            premise: checked.verdict,
        })
    }
}
