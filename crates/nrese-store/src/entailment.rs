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
//! answer is no, except under `owl2-dl`. There none of the above applies, as the closure
//! proves nothing under the Direct Semantics (an annotation property's domain puts its
//! subjects in the closure, for one): the DL engines decide the whole conclusion
//! ([`StoreService::entails_dl`]), an [`StoreError::Undecided`] error where they can't,
//! within one `dl.timeout` from the call. A store whose rules' consistency is undecided
//! gives no answer in any mode.
//! Refutation uses the store's process-memory watch; interruption is an error, never
//! a negative entailment answer or an assumed-unreachable panic.

use std::collections::HashSet;
use std::sync::Arc;

use nrese_dl::limits::Deadline;
use nrese_engine::Snapshot;
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
        // One certified closure: reads start only on a settled certificate (an
        // even epoch; odd, a change is in progress and the old certificate may still be
        // visible), and an answer stands only if no change began while they took their
        // snapshots. An open or changed certificate refuses without waiting or retrying.
        // The mode is one of those reads: switching it is a change of the certificate.
        let epoch = self.certificate_epoch();
        if epoch.is_multiple_of(2) {
            // The mode, read once: under owl2-dl, the DL route with one dl.timeout from here.
            let dl = self
                .dl()
                .active()
                .then(|| Deadline::after(self.config().dl.timeout));
            #[cfg(test)]
            crate::service::hooks::run(&crate::service::hooks::AFTER_MODE_READ);
            let answer = self.entails_once(&program, conclusion, dl)?;
            #[cfg(test)]
            crate::service::hooks::run(&crate::service::hooks::AFTER_ENTAILMENT_READS);
            if self.certificate_epoch() == epoch {
                // Under owl2-dl, an answer stands only within its time, the snapshot let go.
                if dl.is_some_and(Deadline::passed) {
                    return Err(out_of_time());
                }
                return Ok(answer);
            }
        }
        Err(StoreError::Configuration(format!(
            "the inferred statements aren't {}'s closure: rematerialise first",
            program.name()
        )))
    }

    /// One try of [`Self::entails`]; it stands only if the certificate didn't change meanwhile.
    /// Under owl2-dl, `dl` is the deadline before which the DL engines decide it.
    fn entails_once(
        &self,
        program: &RuleProgram,
        conclusion: &[Triple],
        dl: Option<Deadline>,
    ) -> StoreResult<Entailment> {
        if !self.reasoning_is_current(program.clone()) {
            return Err(StoreError::Configuration(format!(
                "the inferred statements aren't {}'s closure: rematerialise first",
                program.name()
            )));
        }
        #[cfg(test)]
        crate::service::hooks::run(&crate::service::hooks::AFTER_CERTIFIED);
        let consistency = self.consistency();
        // Not known to entail everything, nor not to: no answer.
        if let crate::ConsistencyStatus::Undecided { violations, why } = consistency {
            return Err(StoreError::Undecided(why.text(violations)));
        }
        // Under owl2-dl the closure is no proof, its inconsistency included: the DL engines
        // decide, the premise's consistency too.
        if let Some(deadline) = dl {
            return self.decided_by_dl(&self.engine().snapshot(), conclusion, deadline);
        }
        if let crate::ConsistencyStatus::Inconsistent { .. } = consistency {
            return Ok(Entailment::InconsistentPremise);
        }
        // One revision for the whole answer: the closure's read and every refutation are of it.
        let at = self.engine().snapshot();
        match self.shown_by_rules(&at, program, &split(conclusion))? {
            true => Ok(Entailment::Entailed),
            false => Ok(Entailment::NotEntailed),
        }
    }

    /// Whether the closure at `at` holds every positive statement and the rules refute every
    /// opposite over it.
    fn shown_by_rules(
        &self,
        at: &Snapshot,
        program: &RuleProgram,
        split: &Split,
    ) -> StoreResult<bool> {
        let asked = split.positive.is_empty() || self.ask(at, &split.positive)?;
        #[cfg(test)]
        crate::service::hooks::run(&crate::service::hooks::BETWEEN_ENTAILMENT_HALVES);
        if !asked {
            return Ok(false);
        }
        for opposite in &split.refutations {
            if !self.refutes(at, program, opposite)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [`Self::entails_dl`]'s answer at `at` before `deadline`, as an [`Entailment`];
    /// undecided where it is unknown.
    fn decided_by_dl(
        &self,
        at: &Snapshot,
        conclusion: &[Triple],
        deadline: Deadline,
    ) -> StoreResult<Entailment> {
        let decided = self.entails_dl_at(at, conclusion, deadline)?;
        match (decided.premise, decided.answer) {
            (crate::dl::Verdict::Inconsistent, _) => Ok(Entailment::InconsistentPremise),
            (_, crate::dl::entailment::Entailed::Yes) => Ok(Entailment::Entailed),
            (_, crate::dl::entailment::Entailed::No) => Ok(Entailment::NotEntailed),
            (_, crate::dl::entailment::Entailed::Unknown(why)) => {
                Err(StoreError::Undecided(format!("under OWL 2 DL: {why}")))
            }
        }
    }

    /// Whether the materialised data at `at` matches `pattern`, blank nodes read as variables.
    fn ask(&self, at: &Snapshot, pattern: &[Triple]) -> StoreResult<bool> {
        let patterns: Vec<String> = pattern
            .iter()
            .map(|t| format!("{} .", triple_pattern(t)))
            .collect();
        let text = format!("ASK {{ {} }}", patterns.join(" "));
        let query = SparqlParser::new()
            .parse_query(&text)
            .map_err(|error| StoreError::Configuration(format!("{error}: {text}")))?;
        match nrese_sparql::evaluate_query(at, &query, &QueryOptions::default())? {
            QueryResults::Boolean(found) => Ok(found),
            _ => unreachable!("an ASK answers with a boolean"),
        }
    }

    /// Whether adding `opposite` to the data at `at` makes it inconsistent under `program`:
    /// the reasoner maintains the closure over a speculative transaction, which is dropped.
    /// Undecided where a commit came after `at` (the transaction starts from the latest
    /// revision): its answer would be another revision's.
    fn refutes(
        &self,
        at: &Snapshot,
        program: &RuleProgram,
        opposite: &[Triple],
    ) -> StoreResult<bool> {
        let memory = self.memory_stop();
        let (stop, room) = (|| memory.exceeded(), || memory.room());
        let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
        let config = self.config();
        // Until V1b (the reasoner's maintenance over either transaction kind): the old shared
        // form, so the rule constants this compiles still enter the dictionary.
        #[allow(clippy::disallowed_methods)]
        let mut tx = self.engine().speculative_shared();
        if tx.base().revision() != at.revision() {
            return Err(StoreError::Undecided(CHANGED.to_owned()));
        }
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
            .map_err(|interrupted| self.interruption_refusal(interrupted))?
            .by_representatives(config.equality_by_representatives)
            .storing_representatives(config.equality_compact);
        let report = self.operation_memory();
        let done = nrese_reasoner::engine::maintain_with(
            &compiled, None, &mut tx, &operation, &report, false,
        )
        .map_err(|interrupted| {
            memory
                .error(interrupted)
                .unwrap_or(StoreError::ProcessMemoryLimit {
                    limit: nrese_exec::memory::process_limit().unwrap_or(0),
                })
        })?;
        Ok(!done.violations.consistent())
    }
}

/// Why an entailment check that saw a commit after its start gives no answer.
const CHANGED: &str = "the data changed during the entailment check: ask again";

/// An entailment check whose deadline passed gives no answer.
fn out_of_time() -> StoreError {
    StoreError::Undecided(nrese_dl::limits::Limit::Time.reached("the deadline passed"))
}

/// `triple` as a SPARQL triple pattern: see [`term_pattern`].
fn triple_pattern(triple: &Triple) -> String {
    let subject: Term = triple.subject.clone().into();
    format!(
        "{} {} {}",
        term_pattern(&subject),
        triple.predicate,
        term_pattern(&triple.object)
    )
}

/// `term` in a SPARQL pattern. A blank node is a variable named by its label at any depth:
/// inside a triple term it is the same node as outside it (RDF 1.2 Semantics; W3C test
/// `constrained-bnodes-in-triple-term-fail`).
fn term_pattern(term: &Term) -> String {
    match term {
        Term::BlankNode(b) => format!("?b{}", hex(b.as_str())),
        Term::Triple(t) => format!("<<( {} )>>", triple_pattern(t)),
        other => other.to_string(),
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
    /// ([`crate::dl::entailment`]) within one `dl.timeout` from the call: the premise's
    /// check and the conclusion's tests share it. A conclusion that isn't well-formed OWL 2 DL,
    /// or one left undecided at the deadline, is `unknown`. Reads only; never commits.
    pub fn entails_dl(&self, conclusion: &[Triple]) -> StoreResult<DlEntailment> {
        let deadline = Deadline::after(self.config().dl.timeout);
        self.entails_dl_at(&self.engine().snapshot(), conclusion, deadline)
    }

    /// [`Self::entails_dl`] over the asserted statements at `snapshot`, before `deadline`: past
    /// it, no verdict of any kind, the premise's included.
    fn entails_dl_at(
        &self,
        snapshot: &Snapshot,
        conclusion: &[Triple],
        deadline: Deadline,
    ) -> StoreResult<DlEntailment> {
        use crate::dl::{Verdict, entailment::Entailed};
        let decided = self.entails_dl_read(snapshot, conclusion, deadline)?;
        if !deadline.passed() {
            return Ok(decided);
        }
        let late = || nrese_dl::limits::Limit::Time.reached("the deadline passed");
        Ok(DlEntailment {
            answer: match decided.answer {
                Entailed::Unknown(why) => Entailed::Unknown(why),
                _ => Entailed::Unknown(late()),
            },
            premise: match decided.premise {
                Verdict::Unknown(why) => Verdict::Unknown(why),
                _ => Verdict::Unknown(late()),
            },
        })
    }

    /// [`Self::entails_dl_at`] before its check of the deadline.
    fn entails_dl_read(
        &self,
        snapshot: &Snapshot,
        conclusion: &[Triple],
        deadline: Deadline,
    ) -> StoreResult<DlEntailment> {
        use crate::dl::entailment::{Entailed, entails_ontology_within};
        use crate::dl::{Verdict, consistency, gate, source};
        use nrese_rdf::{BlankNode, NamedNodeRef, NamedOrBlankNode};
        if self.dl().replica() {
            // Reading a conclusion interns its terms, which a replica must not.
            return Err(StoreError::Configuration(
                "entailment checks run on the primary, not on a read replica".to_owned(),
            ));
        }
        let memory = self.envelope(None);
        // Undecided where the request's envelope can't take reading the premise (SYN-61).
        let treatments = &self.config().dl.treatments;
        let Ok((premise, read_statements)) =
            source::read_premise(snapshot, memory.clone(), treatments)
        else {
            let why = source::refused("the premise");
            return Ok(DlEntailment {
                answer: Entailed::Unknown(format!("the premise: {why}")),
                premise: Verdict::Unknown(why),
            });
        };
        let budget = gate::budget(self, None, &memory);
        let checked = consistency::check(&premise, &budget.remaining(deadline));
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
        #[cfg(test)]
        crate::service::hooks::run(&crate::service::hooks::AFTER_DL_PREMISE);
        // The OWL vocabulary and the fresh names are system vocabulary, shared (dictionary
        // lifecycle V1); the conclusion's terms are the request's own, in a private
        // vocabulary charged to its envelope, past the premise's ids.
        source::intern_system_vocabulary(self.engine())?;
        let tx = source::private_over(self.engine(), snapshot, Arc::new(memory.part()));
        let n = CONCLUSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let blank = |b: &BlankNode| BlankNode::new_unchecked(format!("nresec{n}x{}", b.as_str()));
        let id = |t: Term| -> nrese_engine::EngineResult<u64> {
            let t = match t {
                Term::BlankNode(b) => Term::BlankNode(blank(&b)),
                other => other,
            };
            Ok(tx.try_intern(t.as_ref())?.raw())
        };
        let statements: nrese_engine::EngineResult<Vec<nrese_owl::Statement>> = conclusion
            .iter()
            .map(|t| {
                let subject: Term = match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                    NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
                };
                Ok(nrese_owl::Statement {
                    triple: [
                        id(subject)?,
                        id(Term::NamedNode(t.predicate.clone()))?,
                        id(t.object.clone())?,
                    ],
                    graph: nrese_engine::TermId::DEFAULT_GRAPH.raw(),
                })
            })
            .collect();
        // Undecided where the envelope can't take the conclusion's terms, as the premise's.
        let Ok(statements) = statements else {
            return Ok(DlEntailment {
                answer: Entailed::Unknown(format!(
                    "the conclusion: {}",
                    source::refused("the conclusion")
                )),
                premise: checked.verdict,
            });
        };
        let decode = |term| tx.decode(term);
        let lookup = |iri: &str| tx.lookup(NamedNodeRef::new_unchecked(iri).into());
        // The conclusion reads SHACL content as one source of it and the premise would
        // (`nrese_owl::read_beside`); the premise keeps its own reading.
        let read = nrese_owl::read_beside(
            &statements,
            &read_statements,
            &source::StoreTerms {
                decode: &decode,
                lookup: &lookup,
            },
        );
        // The premise's statements are read: their charge goes back to the envelope before
        // the check draws on it.
        drop(read_statements);
        // A structure no axiom uses says nothing under the Direct Semantics (OWL 1
        // conclusions state expressions so); anything else unread leaves it undecided.
        // Named by the conclusion's IRIs and literals, as the requester wrote them; its blank
        // nodes, relabelled here, by none.
        if let Some(diagnostic) = read.diagnostics.iter().find(|d| d.is_fatal()) {
            let name = |t| {
                let written = decode(nrese_engine::TermId::from_raw(t));
                written
                    .and_then(consistency::as_written)
                    .unwrap_or_else(|| "_".into())
            };
            return Ok(DlEntailment {
                answer: Entailed::Unknown(format!(
                    "the conclusion isn't well-formed OWL 2 DL: {}",
                    diagnostic.describe(&name)
                )),
                premise: checked.verdict,
            });
        }
        // Shared names (above): the private vocabulary finds them below its start.
        let fresh: nrese_engine::EngineResult<Vec<u64>> = (0..source::FRESH)
            .map(|i| {
                let iri = source::fresh_iri(i);
                Ok(tx
                    .try_intern(NamedNodeRef::new_unchecked(&iri).into())?
                    .raw())
            })
            .collect();
        let fresh = fresh?;
        Ok(DlEntailment {
            answer: entails_ontology_within(&premise, &read, &fresh, &budget, deadline),
            premise: checked.verdict,
        })
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::StoreConfig;
    use nrese_reasoner::rulesets::Ruleset;

    #[test]
    fn embedded_refutation_and_equality_diagnostics_obey_the_store_watch() {
        if !nrese_exec::memory::READS_MEMORY {
            return;
        }
        let opposite = Triple::new(
            nrese_rdf::NamedNode::new_unchecked("urn:test:a"),
            owl::SAME_AS,
            nrese_rdf::NamedNode::new_unchecked("urn:test:b"),
        );
        for limit in [1, 0] {
            let store = StoreService::new(StoreConfig {
                process_memory_bytes: crate::ProcessMemory::Bytes(limit),
                ..StoreConfig::in_memory()
            })
            .unwrap();
            let result = store.refutes(
                &store.engine().snapshot(),
                &Ruleset::Owl2Rl.into(),
                std::slice::from_ref(&opposite),
            );
            let diagnostic = store.equality_report(Ruleset::Owl2Rl);
            if limit == 1 {
                assert!(matches!(
                    result,
                    Err(StoreError::ProcessMemoryLimit { limit: 1 })
                ));
                assert_eq!(diagnostic, "equality: stopped by the process memory limit");
            } else {
                assert!(!result.unwrap());
                assert!(diagnostic.contains("replicated closure"));
                assert!(!diagnostic.contains("stopped"));
            }
        }
    }
}

#[cfg(test)]
#[path = "entailment_dl_tests.rs"]
mod dl_tests;
