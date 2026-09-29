//! Reasoner v2 in the store (reasoner-v2 design §4.3, R4): the configured ruleset's closure
//! over the asserted data is materialised into the engine's inferred stack, so every read
//! under the default `Materialised` model sees it.
//!
//! - **After bulk loads and at startup:** [`StoreService::rematerialise`](crate::StoreService::rematerialise)
//!   computes the closure with the batch executor and replaces the inferred stack in one
//!   revision ([`Rematerialisation`](nrese_engine::Rematerialisation)).
//! - **On commits:** the mutation pipeline runs the delta executor over the transaction
//!   ([`apply_delta`]): it reads the committed indexes plus the pending changes through
//!   [`EngineBase`] and applies the inferred changes inside the same transaction, so
//!   asserted and inferred changes publish atomically. The cost follows the change, plus
//!   grounding the rules against the TBox.
//!
//! Rules match over the union of all graphs and inferences go to the default graph
//! (design §6.2). OWL 2 RL derives some generalised triples, such as a literal typed by a
//! datatype property's range; RDF can't store a literal subject or a non-IRI predicate, so
//! those are dropped. Commits therefore don't see them either, which matters only for
//! rules that turn them back into storable facts (an `owl:inverseOf` on a datatype
//! property, which OWL 2 doesn't allow).

use std::time::{Duration, Instant};

use nrese_engine::quad::Permutation;
use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TermKind,
    Transaction,
};
use nrese_reasoner::v2::batch::{self, Phases, Schema};
use nrese_reasoner::v2::delta::{self, Base, MemoryBase};
use nrese_reasoner::v2::eval::GroundProgram;
use nrese_reasoner::v2::ir::{Rule, Vocabulary};
use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::naive::{Triple, Violation};
use nrese_reasoner::v2::rulesets::Ruleset;
use oxrdf::{LiteralRef, NamedNodeRef, TermRef};
use std::collections::HashSet;

/// A ruleset compiled against the engine dictionary: its rules, list vocabulary and schema
/// vocabulary as engine ids. Ids never change once interned, so it is built once.
pub struct Program {
    pub ruleset: Ruleset,
    rules: Vec<Rule>,
    lists: Option<ListVocabulary>,
    schema: Schema,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("ruleset", &self.ruleset.name())
            .field("rules", &self.rules.len())
            .finish_non_exhaustive()
    }
}

impl Program {
    /// The rules as the delta executor takes them.
    pub fn rules(&self) -> delta::Rules<'_> {
        delta::Rules {
            rules: &self.rules,
            lists: self.lists.as_ref(),
            schema: &self.schema,
        }
    }

    /// Compiles `ruleset`; `intern` gives ids to the constants.
    pub fn new(ruleset: Ruleset, intern: &dyn Fn(TermRef<'_>) -> TermId) -> Self {
        let mut constants = Constants { intern };
        let rules = ruleset
            .rules(&mut constants)
            .expect("the built-in rulesets parse (tested)");
        let lists = ruleset
            .has_list_rules()
            .then(|| ListVocabulary::new(&mut constants));
        let schema = Schema::owl(&mut constants);
        Self {
            ruleset,
            rules,
            lists,
            schema,
        }
    }
}

/// A closure computed over engine term ids.
#[derive(Debug, Default)]
pub struct Closure {
    /// The storable inferred statements (in the default graph).
    pub inferred: Vec<EncodedTriple>,
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<String>,
    pub rounds: usize,
    pub phases: Phases,
}

/// What a rematerialisation or a commit-path run did.
#[derive(Debug, Clone, Default)]
pub struct MaterialisationReport {
    pub ruleset: &'static str,
    /// The revision holding the new inferred stack (for commits: the commit's).
    pub revision: u64,
    pub asserted: u64,
    pub inferred: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub violations: usize,
    pub rounds: usize,
    pub elapsed: Duration,
}

/// Interns the rules' constants into the engine dictionary.
struct Constants<'a> {
    intern: &'a dyn Fn(TermRef<'_>) -> TermId,
}

impl Vocabulary for Constants<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        (self.intern)(NamedNodeRef::new_unchecked(iri).into()).raw()
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        let literal = LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype));
        (self.intern)(literal.into()).raw()
    }
}

/// Whether RDF can store the triple: an IRI or blank node subject and an IRI predicate.
fn storable([subject, predicate, _]: Triple) -> bool {
    matches!(
        TermId::from_raw(subject).kind(),
        TermKind::Iri | TermKind::BlankNode
    ) && TermId::from_raw(predicate).kind() == TermKind::Iri
}

fn encode([s, p, o]: Triple) -> EncodedTriple {
    EncodedTriple::new(
        TermId::from_raw(s),
        TermId::from_raw(p),
        TermId::from_raw(o),
    )
}

fn triple(quad: EncodedQuad) -> Triple {
    [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()]
}

/// The closure of `asserted` (any graphs) under `program`.
pub fn materialise(program: &Program, snapshot: &Snapshot) -> Closure {
    let result = batch::materialise_grouped(
        asserted_by_predicate(snapshot),
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
    );
    Closure {
        inferred: result
            .derived
            .into_iter()
            .filter(|&t| storable(t))
            .map(encode)
            .collect(),
        violations: result.violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        phases: result.phases,
    }
}

/// The asserted facts (any graph) of `snapshot` per predicate, as sorted, distinct
/// `(object, subject)` pairs: one POSG scan, where a fact asserted in several graphs comes
/// out adjacently.
fn asserted_by_predicate(snapshot: &Snapshot) -> Vec<(u64, Vec<(u64, u64)>)> {
    let scan = snapshot
        .scan_sorted_in(ReadModel::Asserted, &QuadPattern::all(), Permutation::Posg)
        .expect("the asserted stack keeps POSG");
    let mut groups: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
    for quad in scan {
        let (p, pair) = (
            quad.predicate.raw(),
            (quad.object.raw(), quad.subject.raw()),
        );
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => {
                if pairs.last() != Some(&pair) {
                    pairs.push(pair);
                }
            }
            _ => groups.push((p, vec![pair])),
        }
    }
    groups
}

fn pattern([s, p, o]: [Option<u64>; 3], graph: GraphSelector) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph,
    }
}

/// A transaction's state as the delta executor reads it: the committed indexes (asserted
/// and inferred, any graph) without the pending deletes, plus the pending inserts, indexed
/// in memory. Scanning the transaction itself would filter every pending insert on each
/// lookup.
pub struct EngineBase<'a> {
    snapshot: &'a Snapshot,
    inserts: MemoryBase,
    deletes: HashSet<EncodedQuad>,
}

impl<'a> EngineBase<'a> {
    pub fn new(tx: &'a Transaction<'_>) -> Self {
        let inserts: Vec<Triple> = tx.inserted().map(triple).collect();
        Self {
            snapshot: tx.base(),
            inserts: MemoryBase::new(&inserts, &[]),
            deletes: tx.deleted().collect(),
        }
    }

    /// Whether `fact` was in the state before the transaction (any graph, either stack).
    fn known_before(&self, fact: Triple) -> bool {
        let [s, p, o] = fact.map(Some);
        self.snapshot
            .quads_for_pattern_in(
                ReadModel::Materialised,
                &pattern([s, p, o], GraphSelector::Any),
            )
            .next()
            .is_some()
    }

    fn committed(&self, model: ReadModel, fact: Triple) -> bool {
        let [s, p, o] = fact.map(Some);
        self.snapshot
            .quads_for_pattern_in(model, &pattern([s, p, o], GraphSelector::Any))
            .any(|quad| !self.deletes.contains(&quad))
    }
}

impl Base for EngineBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any))
        {
            if !self.deletes.contains(&quad) {
                f(triple(quad));
            }
        }
        self.inserts.scan(bound, f);
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let committed = self
            .snapshot
            .count_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any));
        usize::try_from(committed).unwrap_or(usize::MAX) + self.inserts.estimate(bound)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.inserts.contains(fact) || self.committed(ReadModel::Materialised, fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.inserts.contains(fact) || self.committed(ReadModel::Asserted, fact)
    }
}

/// Keeps the transaction's inferred stack equal to the closure of its asserted state,
/// given that it was before the transaction's changes: runs the delta executor and applies
/// its result. Returns the violations the change introduced and the counts.
pub fn apply_delta(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
) -> (Vec<Violation>, MaterialisationReport, Option<GroundProgram>) {
    let started = Instant::now();
    let update = {
        let base = EngineBase::new(tx);
        // Facts new to the state: in no graph and not inferred before the transaction.
        let mut inserted: Vec<Triple> = tx
            .inserted()
            .map(triple)
            .filter(|&t| !base.known_before(t))
            .collect();
        inserted.sort_unstable();
        inserted.dedup();
        // A triple deleted from one graph but still asserted in another stays a fact.
        let mut deleted: Vec<Triple> = tx
            .deleted()
            .map(triple)
            .filter(|&t| !base.is_asserted(t))
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        if inserted.is_empty() && deleted.is_empty() {
            delta::Update::default()
        } else {
            delta::update(&base, &inserted, &deleted, program.rules(), ground)
        }
    };
    let (mut inserted, mut removed) = (0, 0);
    // A statement asserted in any graph is explicit, never also inferred (the engine
    // enforces that for the default graph only).
    let asserted_now: Vec<EncodedTriple> = tx.inserted().map(EncodedTriple::from).collect();
    for fact in update.remove.iter().map(|&t| encode(t)).chain(asserted_now) {
        if tx.remove_inferred(fact) {
            removed += 1;
        }
    }
    for &fact in update.insert.iter().filter(|&&t| storable(t)) {
        if tx.insert_inferred(encode(fact)) {
            inserted += 1;
        }
    }
    let report = MaterialisationReport {
        ruleset: program.ruleset.name(),
        revision: tx.base().revision() + 1,
        asserted: tx.len_in(ReadModel::Asserted),
        inferred: tx.len_in(ReadModel::Inferred),
        inferred_inserted: inserted,
        inferred_deleted: removed,
        violations: update.violations.len(),
        rounds: update.rounds,
        elapsed: started.elapsed(),
    };
    (update.violations, report, update.program)
}

/// What an OWL 2 RL consistency rule's violation means, for reject reports.
fn describe(rule: &str) -> &'static str {
    match rule {
        "cax-dw" => "an instance of two disjoint classes",
        "cax-adc" => "an instance of two classes of an owl:AllDisjointClasses axiom",
        "cls-com" => "an instance of a class and of its complement",
        "cls-nothing2" => "an instance of owl:Nothing",
        "cls-maxc1" | "cls-maxqc1" | "cls-maxqc2" => "a maximum cardinality of 0 is exceeded",
        "prp-irp" => "an irreflexive property relates a resource to itself",
        "prp-asyp" => "an asymmetric property holds in both directions",
        "prp-pdw" => "two disjoint properties relate the same pair",
        "prp-adp" => "two properties of an owl:AllDisjointProperties axiom relate the same pair",
        "prp-npa1" | "prp-npa2" => "a negative property assertion is contradicted",
        "eq-diff1" | "eq-diff2" | "eq-diff3" => "resources declared different are the same",
        _ => "a consistency rule is violated",
    }
}

/// A term as reject reports show it: an IRI plainly, anything else in N-Triples form.
fn term_text(term: &oxrdf::Term) -> String {
    match term {
        oxrdf::Term::NamedNode(node) => node.as_str().to_owned(),
        other => other.to_string(),
    }
}

/// A violation decoded for a reject report: the violated rule's premises under its
/// bindings (the facts that clash), each marked asserted or inferred.
pub fn explain(
    program: &Program,
    violation: &Violation,
    tx: &Transaction<'_>,
) -> nrese_reasoner::RejectExplanation {
    use nrese_reasoner::v2::ir::{Head, Term};
    let decode = |id: u64| {
        tx.decode(TermId::from_raw(id))
            .map_or_else(|| format!("#{id}"), |term| term_text(&term))
    };
    let value = |term: Term| match term {
        Term::Const(c) => Some(c),
        Term::Var(v) => violation.bindings.get(usize::from(v)).copied(),
    };
    let evidence: Vec<nrese_reasoner::RejectEvidence> = program
        .rules
        .iter()
        .find(|r| r.name == violation.rule && r.head == Head::Inconsistent)
        .map(|rule| {
            rule.body
                .iter()
                .filter_map(|atom| {
                    let [s, p, o] = [value(atom.0[0])?, value(atom.0[1])?, value(atom.0[2])?];
                    let asserted = tx
                        .quads_for_pattern_in(
                            ReadModel::Asserted,
                            &pattern([Some(s), Some(p), Some(o)], GraphSelector::Any),
                        )
                        .next()
                        .is_some();
                    Some(nrese_reasoner::RejectEvidence {
                        role: "premise",
                        subject: decode(s),
                        predicate: decode(p),
                        object: decode(o),
                        origin: if asserted { "asserted" } else { "inferred" }.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // The instance the violation is about: the subject of the last premise (OWL 2 RL
    // rules list schema premises first), else the first binding.
    let focus = evidence
        .last()
        .map(|e| e.subject.clone())
        .or_else(|| violation.bindings.first().map(|&id| decode(id)))
        .unwrap_or_default();
    let premises: Vec<String> = evidence
        .iter()
        .map(|e| format!("{} {} {} ({})", e.subject, e.predicate, e.object, e.origin))
        .collect();
    let summary = if premises.is_empty() {
        let bindings: Vec<String> = violation.bindings.iter().map(|&id| decode(id)).collect();
        format!(
            "{} ({}): {}",
            violation.rule,
            describe(&violation.rule),
            bindings.join(", ")
        )
    } else {
        format!(
            "{} ({}): {}",
            violation.rule,
            describe(&violation.rule),
            premises.join("; ")
        )
    };
    nrese_reasoner::RejectExplanation {
        summary,
        violated_constraint: violation.rule.clone(),
        focus_resource: focus,
        evidence,
    }
}
