//! Explanations over the engine: violations for reject reports, why a statement holds,
//! its justifications ([`super::explain_fact`], [`super::justify_fact`]).

use nrese_engine::{GraphSelector, ReadModel, Snapshot, TermId, Transaction};

use super::{Program, decoded, pattern, triple};
use crate::delta::Base;
use crate::ir::{Triple, Violation};

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

/// A violation decoded for a reject report: its premises (the facts that clash), each
/// marked asserted or inferred. Its terms are all decoded: for readers of every graph (and
/// the administrators' diagnostics) alone.
pub fn explain_violation(violation: &Violation, tx: &Transaction<'_>) -> crate::RejectExplanation {
    let decode = |id: u64| decoded(tx.decode(TermId::from_raw(id)), id);
    let evidence: Vec<crate::RejectEvidence> = violation
        .premises
        .iter()
        .map(|&[s, p, o]| crate::RejectEvidence {
            role: "premise",
            subject: decode(s),
            predicate: decode(p),
            object: decode(o),
            origin: match asserted(tx, [s, p, o]) {
                true => "asserted",
                false => "inferred",
            }
            .to_owned(),
        })
        .collect();
    // The instance the violation is about: the subject of the last premise (OWL 2 RL rules
    // list schema premises first), else the first binding.
    let focus = evidence
        .last()
        .map(|e| e.subject.clone())
        .or_else(|| violation.bindings.first().map(|&id| decode(id)))
        .unwrap_or_default();
    let shown: Vec<String> = match evidence.is_empty() {
        true => violation.bindings.iter().map(|&id| decode(id)).collect(),
        false => evidence
            .iter()
            .map(|e| format!("{} {} {} ({})", e.subject, e.predicate, e.object, e.origin))
            .collect(),
    };
    let separator = if evidence.is_empty() { ", " } else { "; " };
    crate::RejectExplanation {
        summary: format!(
            "{} ({}): {}",
            violation.rule,
            describe(&violation.rule),
            shown.join(separator)
        ),
        violated_constraint: violation.rule.clone(),
        focus_resource: focus,
        evidence,
    }
}

/// Whether `fact` is asserted in `tx`, in any graph.
fn asserted(tx: &Transaction<'_>, [s, p, o]: Triple) -> bool {
    tx.quads_for_pattern_in(
        ReadModel::Asserted,
        &pattern([Some(s), Some(p), Some(o)], GraphSelector::Any),
    )
    .next()
    .is_some()
}

/// One step of an inference's explanation ([`explain_fact`]), decoded: the fact, whether it
/// is asserted or inferred, the rule that derives it and its premises (indexes of steps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceStep {
    /// The statement's terms (N-Triples).
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `asserted` or `inferred`.
    pub origin: &'static str,
    /// The rule (`cax-sco`, `prp-trp`, ...); `None` for an asserted fact.
    pub rule: Option<String>,
    pub premises: Vec<usize>,
}

/// Facts an explanation examines at most.
const EXPLANATION_BUDGET: usize = 4096;

/// A requester's share of a committed state, where it may not read every graph: the
/// asserted statements of the graphs it may read, the inferred statements it sees, the
/// ruleset's axioms. An explanation reads nothing else: its rules are grounded, its facts
/// scanned and recognised as asserted, and its derivation chosen within that share, so
/// what it answers is the same whatever the graphs the requester can't read hold.
pub struct ProofScope<'a> {
    /// Whether the requester may read the graph.
    pub graph: &'a (dyn Fn(TermId) -> bool + Sync),
    /// Whether it sees inferred statements: then those of the snapshot the explanation
    /// reads, which is the requester's view (with only those its support graph sets let it
    /// see, where it sees them by support).
    pub inferred: bool,
}

/// What [`explain_fact`] found for a fact that holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Explained {
    /// A derivation from asserted facts, the fact first.
    Proof(Vec<InferenceStep>),
    /// None within the search's limits, which cut it short (the reason): the fact holds and
    /// may have a derivation past them. Not an answer that it doesn't hold.
    Incomplete(&'static str),
}

/// Why the fact `[s, p, o]` holds in `snapshot` under `program`: a derivation of it from
/// asserted facts ([`crate::explain`]), the fact first, or that a limit cut the search
/// short ([`Explained::Incomplete`]). `None` if it doesn't hold, or the search, within its
/// limits, found no derivation.
///
/// `scope`, where the requester may not read every graph: the search within the
/// requester's share ([`ProofScope`]), its limits counting only the share's work.
pub fn explain_fact(
    program: &Program,
    snapshot: &Snapshot,
    fact: Triple,
    scope: Option<&ProofScope<'_>>,
) -> Option<Explained> {
    let found = match scope {
        None => {
            let base = SnapshotBase {
                snapshot,
                axioms: &program.axioms,
            };
            crate::explain::explain(&base, program.rules(), fact, EXPLANATION_BUDGET)
        }
        Some(scope) => {
            let base = ScopedBase::new(snapshot, &program.axioms, scope);
            crate::explain::explain(&base, program.rules(), fact, EXPLANATION_BUDGET)
        }
    };
    let explanation = match found {
        Ok(explanation) => explanation,
        Err(crate::explain::Unexplained::Limit(reason)) => {
            return Some(Explained::Incomplete(reason));
        }
        Err(
            crate::explain::Unexplained::DoesNotHold | crate::explain::Unexplained::NoDerivation,
        ) => {
            return None;
        }
    };
    let decode = |id: u64| decoded(snapshot.decode(TermId::from_raw(id)), id);
    Some(Explained::Proof(
        explanation
            .steps
            .into_iter()
            .map(|step| {
                let [s, p, o] = step.fact;
                InferenceStep {
                    subject: decode(s),
                    predicate: decode(p),
                    object: decode(o),
                    origin: if step.rule.is_some() {
                        "inferred"
                    } else {
                        "asserted"
                    },
                    rule: step.rule,
                    premises: step.premises,
                }
            })
            .collect(),
    ))
}

/// Which justifications of an inferred statement to compute ([`justify_fact`]), in order
/// of cost (docs/design/owl2-dl.md §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustificationMode {
    /// One minimal set of asserted statements the statement follows from.
    One,
    /// The statements every justification has.
    Core,
    /// The statements some justification has (the relevant ones).
    Union,
    /// The `k` smallest justifications.
    Top(usize),
    /// Every justification, smallest first (up to [`JUSTIFICATIONS_AT_MOST`]).
    All,
}

/// Justifications listed at most (`all`, `top-k`): past it the answer says it isn't
/// complete.
pub const JUSTIFICATIONS_AT_MOST: usize = 1000;

/// Resolution steps a justification enumeration takes at most.
const JUSTIFICATION_BUDGET: usize = 500_000;

/// A statement of a justification, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JustifiedStatement {
    /// The statement's terms (N-Triples).
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `asserted`.
    pub origin: &'static str,
}

/// The justifications of an inferred statement ([`justify_fact`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JustificationAnswer {
    pub mode: JustificationMode,
    /// `one`, `top-k`, `all`: the justifications, smallest first; `core`, `union`: one set.
    pub sets: Vec<Vec<JustifiedStatement>>,
    /// Whether the answer is exact: every derivation was collected, and the enumeration
    /// (`top-k`, `all`, `union`) ended within its limits.
    pub complete: bool,
    /// For `one`, `top-k` and `all`: whether a proof of the statement from each listed
    /// justification was checked step by step against the rules (the proof checker:
    /// always, so a `false` is an engine error to report). `None` for `core` and `union`.
    pub verified: Option<bool>,
}

/// The justifications of the fact `[s, p, o]` in `snapshot` under `program`: minimal sets
/// of asserted statements it follows from, computed on its proof graph
/// ([`crate::explain::proof_graph`], the proof IR of `nrese-owl`). `None` if it
/// doesn't hold. `scope` as for [`explain_fact`]: the program grounded, the proof graph
/// built, the justifications enumerated within their limits, their completeness judged and
/// each listed one's proof checked, all within the requester's share; so the sets, the core
/// and union, and the flags are the same whatever the graphs it can't read hold.
pub fn justify_fact(
    program: &Program,
    snapshot: &Snapshot,
    fact: Triple,
    mode: JustificationMode,
    scope: Option<&ProofScope<'_>>,
) -> Option<JustificationAnswer> {
    let (sets, complete, verified) = match scope {
        None => justify_in(
            &SnapshotBase {
                snapshot,
                axioms: &program.axioms,
            },
            program,
            fact,
            mode,
        )?,
        Some(scope) => justify_in(
            &ScopedBase::new(snapshot, &program.axioms, scope),
            program,
            fact,
            mode,
        )?,
    };
    let decode = |id: u64| decoded(snapshot.decode(TermId::from_raw(id)), id);
    let statement = |[s, p, o]: Triple| JustifiedStatement {
        subject: decode(s),
        predicate: decode(p),
        object: decode(o),
        origin: "asserted",
    };
    Some(JustificationAnswer {
        mode,
        sets: sets
            .into_iter()
            .map(|set| set.into_iter().map(statement).collect())
            .collect(),
        complete,
        verified,
    })
}

/// [`justify_fact`]'s sets over `base`, whether the answer is complete, and whether each
/// listed set's proof checks.
fn justify_in<B: Base>(
    base: &B,
    program: &Program,
    fact: Triple,
    mode: JustificationMode,
) -> Option<(Vec<Vec<Triple>>, bool, Option<bool>)> {
    let ground = crate::delta::program(base, program.rules());
    let graph = crate::explain::proof_graph(base, &ground, fact, EXPLANATION_BUDGET)?;
    let (sets, complete) = match mode {
        JustificationMode::One => (graph.one().into_iter().collect(), graph.complete),
        JustificationMode::Core => (vec![graph.core()], graph.complete),
        JustificationMode::Union => {
            let (union, complete) = graph.union(JUSTIFICATION_BUDGET);
            (vec![union], complete)
        }
        JustificationMode::Top(_) | JustificationMode::All => {
            let limit = match mode {
                JustificationMode::Top(k) => k.min(JUSTIFICATIONS_AT_MOST),
                _ => JUSTIFICATIONS_AT_MOST,
            };
            let found = graph.justifications(limit, JUSTIFICATION_BUDGET);
            // Top-k asks for k: having them is the whole answer.
            let complete = found.complete
                || matches!(mode, JustificationMode::Top(k) if found.found.len() == k && graph.complete);
            (found.found, complete)
        }
    };
    let verified = match mode {
        JustificationMode::Core | JustificationMode::Union => None,
        _ => Some(sets.iter().all(|set| {
            graph
                .proof_from(&|a| set.binary_search(a).is_ok())
                .is_some_and(|proof| {
                    proof
                        .check(&|step| crate::explain::valid_step(base, &ground, step))
                        .is_ok()
                })
        })),
    };
    Some((sets, complete, verified))
}

/// A committed state as the reasoner reads it: asserted and inferred statements of every
/// graph, the ruleset's axioms counting as asserted.
pub(crate) struct SnapshotBase<'a> {
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) axioms: &'a [Triple],
}

impl Base for SnapshotBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for quad in self
            .snapshot
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any))
        {
            f(triple(quad));
        }
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let count = self
            .snapshot
            .estimate_in(ReadModel::Materialised, &pattern(bound, GraphSelector::Any));
        usize::try_from(count).unwrap_or(usize::MAX)
    }

    fn contains(&self, fact: Triple) -> bool {
        self.snapshot.exists_in(
            ReadModel::Materialised,
            &pattern(fact.map(Some), GraphSelector::Any),
        )
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.axioms.binary_search(&fact).is_ok()
            || self.snapshot.exists_in(
                ReadModel::Asserted,
                &pattern(fact.map(Some), GraphSelector::Any),
            )
    }
}

/// Matches a [`ScopedBase`] counts for an estimate at most.
const ESTIMATE_CAP: usize = 1024;

/// A [`ProofScope`]'s share of a committed state as the reasoner reads it: the asserted
/// statements of readable graphs, the inferred ones where the scope sees them, the
/// ruleset's axioms counting as asserted.
pub(crate) struct ScopedBase<'a> {
    snapshot: &'a Snapshot,
    axioms: &'a [Triple],
    scope: &'a ProofScope<'a>,
    /// The graphs the scope may read: the only ones scanned, so the work, like the answer,
    /// follows the share and not what the other graphs hold.
    graphs: Vec<TermId>,
    /// Asserted statements read from the snapshot (its tests check that none of an
    /// unreadable graph is).
    #[cfg(test)]
    visited: std::sync::atomic::AtomicUsize,
}

impl<'a> ScopedBase<'a> {
    pub(crate) fn new(
        snapshot: &'a Snapshot,
        axioms: &'a [Triple],
        scope: &'a ProofScope<'a>,
    ) -> Self {
        let graphs = std::iter::once(TermId::DEFAULT_GRAPH)
            .chain(snapshot.named_graphs())
            .filter(|&graph| (scope.graph)(graph))
            .collect();
        Self {
            snapshot,
            axioms,
            scope,
            graphs,
            #[cfg(test)]
            visited: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The asserted statements matching `bound` in the readable graphs.
    fn asserted_matches(&self, bound: [Option<u64>; 3]) -> impl Iterator<Item = Triple> + '_ {
        self.graphs.iter().flat_map(move |&graph| {
            self.snapshot
                .quads_for_pattern_in(
                    ReadModel::Asserted,
                    &pattern(bound, GraphSelector::Exact(graph)),
                )
                .map(|quad| {
                    #[cfg(test)]
                    self.visited
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    triple(quad)
                })
        })
    }

    fn readable_asserted(&self, fact: Triple) -> bool {
        self.graphs.iter().any(|&graph| {
            self.snapshot.exists_in(
                ReadModel::Asserted,
                &pattern(fact.map(Some), GraphSelector::Exact(graph)),
            )
        })
    }
}

impl Base for ScopedBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        self.asserted_matches(bound).for_each(&mut *f);
        if self.scope.inferred {
            for quad in self
                .snapshot
                .quads_for_pattern_in(ReadModel::Inferred, &pattern(bound, GraphSelector::Any))
            {
                f(triple(quad));
            }
        }
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        // The share's own matches, never the whole state's: an estimate orders the joins,
        // and the order which derivations are collected first, so it depends on nothing
        // outside the share. Counted by the index per readable graph (the matches a scan
        // would give, without producing them), and past the cap any larger count.
        let asserted: u64 = self
            .graphs
            .iter()
            .map(|&graph| {
                self.snapshot.count_in(
                    ReadModel::Asserted,
                    &pattern(bound, GraphSelector::Exact(graph)),
                )
            })
            .sum();
        let inferred = match self.scope.inferred {
            true => self
                .snapshot
                .count_in(ReadModel::Inferred, &pattern(bound, GraphSelector::Any)),
            false => 0,
        };
        match usize::try_from(asserted + inferred).unwrap_or(usize::MAX) {
            n if n > ESTIMATE_CAP => usize::MAX >> 1,
            n => n,
        }
    }

    fn contains(&self, fact: Triple) -> bool {
        self.readable_asserted(fact)
            || (self.scope.inferred
                && self.snapshot.exists_in(
                    ReadModel::Inferred,
                    &pattern(fact.map(Some), GraphSelector::Any),
                ))
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.axioms.binary_search(&fact).is_ok() || self.readable_asserted(fact)
    }
}

#[cfg(test)]
mod tests {
    use nrese_engine::{Engine, EngineConfig, TermId};
    use nrese_rdf::{GraphName, NamedNode, Quad};

    use super::*;

    /// A requester's share is read from its graphs alone (N12, attack review P2-7): a scan
    /// or an estimate of a pattern with many matches in a graph it can't read visits none of
    /// them, so the work, like the answer, doesn't grow with what that graph holds.
    #[test]
    fn a_scoped_scan_visits_no_statement_of_an_unreadable_graph() {
        let iri = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let quad = |o: String, graph: &str| {
            Quad::new(
                iri("a"),
                iri("p"),
                iri(&o),
                GraphName::NamedNode(iri(graph)),
            )
        };
        tx.insert(quad("b".to_owned(), "open").as_ref());
        for i in 0..10_000 {
            tx.insert(quad(format!("x{i}"), "secret").as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let id = |local: &str| snapshot.lookup(iri(local).as_ref().into()).unwrap();
        let open = id("open");
        let graph = |g: TermId| g == open;
        let scope = ProofScope {
            graph: &graph,
            inferred: false,
        };
        let base = ScopedBase::new(&snapshot, &[], &scope);
        let bound = [Some(id("a").raw()), Some(id("p").raw()), None];
        let mut found = Vec::new();
        base.scan(bound, &mut |fact| found.push(fact));
        assert_eq!(found, [[id("a").raw(), id("p").raw(), id("b").raw()]]);
        // The estimate counts the readable graph by its index: it reads no statement.
        assert_eq!(base.estimate(bound), 1);
        assert_eq!(base.visited.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
