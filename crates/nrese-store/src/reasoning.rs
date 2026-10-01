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
use nrese_reasoner::RuleProgram;
use nrese_reasoner::v2::batch::{self, Phases, Schema};
use nrese_reasoner::v2::delta::{self, Base, MemoryBase};
use nrese_reasoner::v2::eval::GroundProgram;
use nrese_reasoner::v2::ir::{Rule, Vocabulary};
use nrese_reasoner::v2::lists::ListDiagnostic;
use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::naive::{Triple, Violation};
use nrese_reasoner::v2::unnamed::UnnamedVocabulary;
use oxrdf::{LiteralRef, NamedNodeRef, TermRef};
use std::collections::HashSet;

/// A rule program compiled against the engine dictionary: its rules, list vocabulary and
/// schema vocabulary as engine ids. Ids never change once interned, so it is built once.
pub struct Program {
    pub program: RuleProgram,
    rules: Vec<Rule>,
    lists: Option<ListVocabulary>,
    schema: Schema,
    /// Leave out memberships in unnamed classes nothing consumes (W7).
    hide_unnamed_classes: bool,
    unnamed: UnnamedVocabulary,
    /// The rules make a declared class a subclass of `owl:Thing` (scm-cls).
    things: bool,
    /// For the datatype checks ([`crate::datatypes`]).
    rdf_type: u64,
    same_as: Option<u64>,
    /// The ruleset's axiomatic triples, sorted: they seed every closure, and a commit
    /// never retracts them.
    axioms: Vec<Triple>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("program", &self.program.name())
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

    /// Compiles `program`; `intern` gives ids to the constants.
    pub fn new(program: &RuleProgram, intern: &dyn Fn(TermRef<'_>) -> TermId) -> Self {
        let mut constants = Constants { intern };
        let rules = program
            .rules(&mut constants)
            .expect("the built-in rulesets parse (tested), user rules are checked at startup");
        let lists = program
            .has_list_rules()
            .then(|| ListVocabulary::new(&mut constants));
        let schema = Schema::owl(&mut constants);
        let unnamed = UnnamedVocabulary::new(&mut constants);
        let things = rules.iter().any(|rule| rule.name == "scm-cls");
        let rdf_type = constants.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let same_as = nrese_reasoner::v2::representatives::same_as(&rules);
        let mut axioms = program
            .axiom_triples(&mut constants)
            .expect("the built-in axioms parse (tested), user facts are checked at startup");
        axioms.sort_unstable();
        Self {
            program: program.clone(),
            rules,
            lists,
            schema,
            hide_unnamed_classes: false,
            unnamed,
            things,
            rdf_type,
            same_as,
            axioms,
        }
    }

    /// This program leaving out memberships in unnamed classes nothing consumes, or not.
    #[must_use]
    pub fn hiding_unnamed_classes(mut self, hide: bool) -> Self {
        self.hide_unnamed_classes = hide;
        self
    }

    /// The schema for a run over `snapshot`: with the unnamed classes to leave out, when
    /// the program does.
    fn schema_for(&self, snapshot: &Snapshot) -> Schema {
        if !self.hide_unnamed_classes {
            return self.schema.clone();
        }
        self.schema.clone().hiding(self.hidden_classes(snapshot))
    }

    /// The unnamed classes of `snapshot`'s asserted statements that can be left out.
    fn hidden_classes(&self, snapshot: &Snapshot) -> std::collections::HashMap<u64, bool> {
        let quads = |pattern: QuadPattern| -> Vec<Triple> {
            snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern)
                .map(triple)
                .collect()
        };
        let any =
            |s: Option<u64>, p: Option<u64>, o: Option<u64>| pattern([s, p, o], GraphSelector::Any);
        let union_of = self.unnamed.union_of();
        let mut candidates: Vec<u64> = quads(any(None, Some(union_of), None))
            .into_iter()
            .map(|t| t[0])
            .filter(|&s| TermId::from_raw(s).kind() == TermKind::BlankNode)
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        nrese_reasoner::v2::unnamed::hidden_classes(
            candidates,
            &|class| {
                let mut out = quads(any(Some(class), None, None));
                out.extend(quads(any(None, None, Some(class))));
                out.extend(quads(any(None, Some(class), None)));
                out
            },
            &self.unnamed,
            self.things,
        )
    }
}

/// A closure computed over engine term ids.
#[derive(Debug, Default)]
pub struct Closure {
    /// The storable inferred statements (in the default graph).
    pub inferred: Vec<EncodedTriple>,
    pub violations: Vec<Violation>,
    /// List axioms that weren't instantiated.
    pub diagnostics: Vec<ListDiagnostic>,
    pub rounds: usize,
    pub phases: Phases,
}

/// The most diagnostics a report lists; `diagnostics_total` counts them all.
pub const MAX_REPORTED_DIAGNOSTICS: usize = 100;

/// A part of the ontology the reasoner couldn't use, with its terms decoded. Today these
/// are list axioms (property chains, keys, intersections, unions, enumerations,
/// AllDisjoint/AllDifferent members) whose list is malformed, cyclic or too large: their
/// rules are missing from the closure, and nothing else is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OntologyDiagnostic {
    /// `malformed-list`, `cyclic-list`, `too-many-list-variants` or `list-too-long`.
    pub kind: &'static str,
    /// The OWL 2 RL rules the axiom feeds.
    pub rules: &'static str,
    /// The axiom: its subject, its predicate and the list's first node.
    pub subject: String,
    pub predicate: String,
    pub list: String,
    /// The node the problem is at, for malformed and cyclic lists.
    pub node: Option<String>,
    pub message: String,
}

/// `found`, decoded for reports (at most [`MAX_REPORTED_DIAGNOSTICS`]).
fn decode_diagnostics(
    found: &[ListDiagnostic],
    decode: &dyn Fn(u64) -> String,
) -> Vec<OntologyDiagnostic> {
    found
        .iter()
        .take(MAX_REPORTED_DIAGNOSTICS)
        .map(|d| OntologyDiagnostic {
            kind: d.problem.kind(),
            rules: d.rules,
            subject: decode(d.subject),
            predicate: decode(d.predicate),
            list: decode(d.head),
            node: d.problem.node().map(decode),
            message: d.describe(decode),
        })
        .collect()
}

/// Logs `diagnostics` as warnings: they mean the closure lacks those axioms' rules.
pub(crate) fn log_diagnostics(diagnostics: &[OntologyDiagnostic], total: usize, context: &str) {
    for diagnostic in diagnostics {
        tracing::warn!(kind = diagnostic.kind, "{context}: {}", diagnostic.message);
    }
    if total > diagnostics.len() {
        tracing::warn!(
            omitted = total - diagnostics.len(),
            "{context}: further ontology diagnostics omitted"
        );
    }
}

/// What a rematerialisation or a commit-path run did.
#[derive(Debug, Clone, Default)]
pub struct MaterialisationReport {
    /// The rule program's name ([`RuleProgram::name`]).
    pub ruleset: String,
    /// The revision holding the new inferred stack (for commits: the commit's).
    pub revision: u64,
    pub asserted: u64,
    pub inferred: u64,
    pub inferred_inserted: u64,
    pub inferred_deleted: u64,
    pub violations: usize,
    /// Ontology parts the reasoner couldn't use: after a rematerialisation all of them,
    /// after a commit those the commit introduced. At most [`MAX_REPORTED_DIAGNOSTICS`].
    pub diagnostics: Vec<OntologyDiagnostic>,
    pub diagnostics_total: usize,
    pub rounds: usize,
    pub elapsed: Duration,
    /// A commit made an unnamed class that was left out consumable: its memberships are
    /// missing until the store rematerialises.
    pub needs_rematerialisation: bool,
}

impl MaterialisationReport {
    /// The report's diagnostics, decoding ids with `decode`.
    pub(crate) fn with_diagnostics(
        mut self,
        found: &[ListDiagnostic],
        decode: &dyn Fn(u64) -> String,
    ) -> Self {
        self.diagnostics = decode_diagnostics(found, decode);
        self.diagnostics_total = found.len();
        self
    }
}

/// An id decoded for reports ([`term_text`]); `#id` if the dictionary lacks it.
pub(crate) fn decoded(term: Option<oxrdf::Term>, id: u64) -> String {
    term.map_or_else(|| format!("#{id}"), |term| term_text(&term))
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

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        let literal = LiteralRef::new_language_tagged_literal_unchecked(lexical, language);
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

/// The closure of `asserted` (any graphs) under `program`, its axioms included.
pub fn materialise(program: &Program, snapshot: &Snapshot) -> Closure {
    materialise_until(program, snapshot, nrese_reasoner::v2::eval::NEVER).expect("never stopped")
}

/// [`materialise`], polling `stop` throughout; `Err` if it fired.
pub fn materialise_until(
    program: &Program,
    snapshot: &Snapshot,
    stop: nrese_reasoner::v2::eval::Stop<'_>,
) -> Result<Closure, delta::Interrupted> {
    let mut input = asserted_by_predicate(snapshot);
    // Axioms nothing asserts are inferred statements, and premises of the rules.
    let mut axioms: Vec<Triple> = Vec::new();
    for &axiom in &program.axioms {
        let [s, p, o] = axiom;
        let at = input.partition_point(|(predicate, _)| *predicate < p);
        match input.get_mut(at) {
            Some((predicate, pairs)) if *predicate == p => match pairs.binary_search(&(o, s)) {
                Ok(_) => continue,
                Err(position) => pairs.insert(position, (o, s)),
            },
            _ => input.insert(at, (p, vec![(o, s)])),
        }
        axioms.push(axiom);
    }
    let schema = program.schema_for(snapshot);
    let result = batch::materialise_grouped_until(
        input,
        &program.rules,
        program.lists.as_ref(),
        &schema,
        stop,
    )?;
    let mut violations = result.violations;
    violations.extend(crate::datatypes::violations(
        &result.derived,
        program.rdf_type,
        program.same_as,
        &|id| snapshot.decode(TermId::from_raw(id)),
    ));
    Ok(Closure {
        inferred: result
            .derived
            .into_iter()
            .chain(axioms)
            .filter(|&t| storable(t))
            .map(encode)
            .collect(),
        violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        phases: result.phases,
    })
}

/// Equality by representatives against replication on the asserted data of `snapshot`
/// (work package W4): facts and time of both closures, and the classes. A measurement,
/// not a store operation.
pub fn equality_report(program: &Program, snapshot: &Snapshot) -> String {
    let input: Vec<Triple> = asserted_by_predicate(snapshot)
        .into_iter()
        .flat_map(|(p, pairs)| pairs.into_iter().map(move |(o, s)| [s, p, o]))
        .collect();
    let started = Instant::now();
    let replicated = batch::materialise(
        &input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
    );
    let replicated_time = started.elapsed();
    let started = Instant::now();
    let closure = nrese_reasoner::v2::representatives::materialise(
        &input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
    );
    let representative_time = started.elapsed();
    let classes = closure.classes.classes().count();
    let members: usize = closure.classes.classes().map(|(_, m)| m.len()).sum();
    let largest = closure
        .classes
        .classes()
        .map(|(_, m)| m.len())
        .max()
        .unwrap_or(0);
    format!(
        "equality: asserted {} | replicated closure {} facts in {:.3} s | representatives {} facts in {:.3} s, {} merges | {} classes, {} members, largest {}",
        input.len(),
        input.len() + replicated.derived.len(),
        replicated_time.as_secs_f64(),
        closure.facts.len(),
        representative_time.as_secs_f64(),
        closure.merges,
        classes,
        members,
        largest
    )
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
    /// The ruleset's axioms (sorted): they count as asserted, so no deletion retracts them.
    axioms: &'a [Triple],
}

impl<'a> EngineBase<'a> {
    pub fn new(tx: &'a Transaction<'_>, axioms: &'a [Triple]) -> Self {
        let inserts: Vec<Triple> = tx.inserted().map(triple).collect();
        Self {
            snapshot: tx.base(),
            inserts: MemoryBase::new(&inserts, &[]),
            deletes: tx.deleted().collect(),
            axioms,
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
        self.inserts.contains(fact)
            || self.axioms.binary_search(&fact).is_ok()
            || self.committed(ReadModel::Asserted, fact)
    }
}

/// Keeps the transaction's inferred stack equal to the closure of its asserted state,
/// given that it was before the transaction's changes: runs the delta executor and applies
/// its result. Returns the violations the change introduced and the counts.
///
/// `stop` is polled throughout the reasoning; when it fires, nothing has been applied to
/// `tx`'s inferred stack and [`delta::Interrupted`] is returned.
pub fn apply_delta(
    program: &Program,
    ground: Option<&GroundProgram>,
    tx: &mut Transaction<'_>,
    stop: nrese_reasoner::v2::eval::Stop<'_>,
) -> Result<(Vec<Violation>, MaterialisationReport, Option<GroundProgram>), delta::Interrupted> {
    let started = Instant::now();
    // Axioms the state lacks (a store that never rematerialised): added with this commit.
    let mut missing_axioms: Vec<Triple> = Vec::new();
    // Unnamed classes left out: as the commit leaves them; one it makes consumable
    // needs a rematerialisation for the memberships left out so far.
    let (schema, revived) = if program.hide_unnamed_classes {
        let before = program.hidden_classes(tx.base());
        let after = program.hidden_classes(&tx.pending_snapshot());
        let revived = before.keys().any(|class| !after.contains_key(class));
        (program.schema.clone().hiding(after), revived)
    } else {
        (program.schema.clone(), false)
    };
    let rules = delta::Rules {
        schema: &schema,
        ..program.rules()
    };
    let update = {
        let base = EngineBase::new(tx, &program.axioms);
        // Facts new to the state: in no graph and not inferred before the transaction.
        let mut inserted: Vec<Triple> = tx
            .inserted()
            .map(triple)
            .filter(|&t| !base.known_before(t))
            .collect();
        if !(inserted.is_empty() && tx.deleted().next().is_none()) {
            missing_axioms = program
                .axioms
                .iter()
                .copied()
                .filter(|&axiom| !base.known_before(axiom) && !base.inserts.contains(axiom))
                .collect();
            inserted.extend(missing_axioms.iter().copied());
        }
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
            delta::update_until(&base, &inserted, &deleted, rules, ground, stop)?
        }
    };
    let mut update = update;
    update.violations.extend(crate::datatypes::violations(
        &update.insert,
        program.rdf_type,
        program.same_as,
        &|id| tx.decode(TermId::from_raw(id)),
    ));
    let (mut inserted, mut removed) = (0, 0);
    // A statement asserted in any graph is explicit, never also inferred (the engine
    // enforces that for the default graph only).
    let asserted_now: Vec<EncodedTriple> = tx.inserted().map(EncodedTriple::from).collect();
    for fact in update.remove.iter().map(|&t| encode(t)).chain(asserted_now) {
        if tx.remove_inferred(fact) {
            removed += 1;
        }
    }
    for &fact in update
        .insert
        .iter()
        .chain(&missing_axioms)
        .filter(|&&t| storable(t))
    {
        if tx.insert_inferred(encode(fact)) {
            inserted += 1;
        }
    }
    let report = MaterialisationReport {
        ruleset: program.program.name(),
        revision: tx.base().revision() + 1,
        asserted: tx.len_in(ReadModel::Asserted),
        inferred: tx.len_in(ReadModel::Inferred),
        inferred_inserted: inserted,
        inferred_deleted: removed,
        violations: update.violations.len(),
        rounds: update.rounds,
        elapsed: started.elapsed(),
        needs_rematerialisation: revived,
        ..MaterialisationReport::default()
    }
    .with_diagnostics(&update.diagnostics, &|id| {
        decoded(tx.decode(TermId::from_raw(id)), id)
    });
    Ok((update.violations, report, update.program))
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
        "dt-not-type" => "a literal is typed with a datatype whose value space doesn't contain it",
        "dt-diff" => "two different data values would be the same",
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
    let decode = |id: u64| decoded(tx.decode(TermId::from_raw(id)), id);
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
