//! The reasoner over NRESE's engine: what the store calls to reason, over its snapshots and
//! transactions (reasoner-v2 design §4.3, R4). The store decides when; this decides how.
//!
//! - **Compile** a rule program against the engine dictionary: [`Program::compile`].
//! - **Materialise** the closure of a snapshot's asserted statements with the batch
//!   executor, for a rematerialisation that replaces the inferred stack in one revision:
//!   [`materialise`], [`materialise_until`].
//! - **Maintain** a transaction's inferred stack on a commit with the delta executor: it
//!   reads the committed indexes plus the pending changes and applies the inferred changes
//!   inside the same transaction, so asserted and inferred changes publish atomically. The
//!   cost follows the change, plus grounding the rules against the TBox: [`maintain`].
//! - **Explain** a violation, why a statement holds, and its justifications:
//!   [`explain_violation`], [`explain_fact`], [`justify_fact`].
//!
//! Rules match over the union of all graphs and inferences go to the default graph
//! (design §6.2). OWL 2 RL derives some generalised triples, such as a literal typed by a
//! datatype property's range; RDF can't store a literal subject or a non-IRI predicate, so
//! those are dropped. Commits therefore don't see them either, which matters only for
//! rules that turn them back into storable facts (an `owl:inverseOf` on a datatype
//! property, which OWL 2 doesn't allow).

mod datatypes;
mod explain;
mod maintain;
mod materialise;

use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TermKind,
};
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};

use crate::RuleProgram;
use crate::batch::Schema;
use crate::delta;
use crate::eval::GroundProgram;
use crate::ir::{Rule, Triple, Vocabulary};
use crate::lists::ListVocabulary;
use crate::unnamed::UnnamedVocabulary;

pub use explain::{
    InferenceStep, JUSTIFICATIONS_AT_MOST, JustificationAnswer, JustificationMode,
    JustifiedStatement, explain_fact, explain_violation, justify_fact,
};
pub use maintain::{EngineBase, Maintenance, maintain};
pub use materialise::{Closure, equality_report, materialise, materialise_until};

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
    /// For the datatype checks ([`super::datatypes`]).
    rdf_type: u64,
    same_as: Option<u64>,
    /// Full materialisations compute the closure over representatives of the `owl:sameAs`
    /// classes and expand it (W4 stage A): the same closure, without rules copying every
    /// fact to every identity while it is computed (`reasoner.equality`).
    by_representatives: bool,
    /// With `by_representatives`: the inferred stack keeps the closure over
    /// representatives, each other identity stored as `identity sameAs representative`,
    /// and reads expand it (W4 stage B; `reasoner.equality = "compact"`).
    store_representatives: bool,
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

    /// Compiles `program` against the engine dictionary; `intern` gives ids to the
    /// constants.
    pub fn compile(program: &RuleProgram, intern: &dyn Fn(TermRef<'_>) -> TermId) -> Self {
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
        let same_as = crate::representatives::same_as(&rules);
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
            by_representatives: true,
            store_representatives: false,
            axioms,
        }
    }

    /// This program computing full closures over representatives of `owl:sameAs` classes
    /// (the default), or with the replacement rules.
    #[must_use]
    pub fn by_representatives(mut self, representatives: bool) -> Self {
        self.by_representatives = representatives;
        self
    }

    /// This program keeping the closure over representatives in the store (with
    /// [`Self::by_representatives`]), or every copy.
    #[must_use]
    pub fn storing_representatives(mut self, store: bool) -> Self {
        self.store_representatives = store;
        self
    }

    /// Whether the inferred stack holds the closure over representatives.
    pub fn stores_representatives(&self) -> bool {
        self.by_representatives && self.store_representatives && self.same_as.is_some()
    }

    /// This program leaving out memberships in unnamed classes nothing consumes, or not.
    #[must_use]
    pub fn hiding_unnamed_classes(mut self, hide: bool) -> Self {
        self.hide_unnamed_classes = hide;
        self
    }

    /// The ruleset's axiomatic triples, sorted.
    pub fn axioms(&self) -> &[Triple] {
        &self.axioms
    }

    /// The ground program of the committed state `snapshot`, as the delta executor
    /// builds it.
    pub fn ground_program(&self, snapshot: &Snapshot) -> GroundProgram {
        let schema = self.schema_for(snapshot);
        let base = explain::SnapshotBase {
            snapshot,
            axioms: &self.axioms,
        };
        delta::program(
            &base,
            delta::Rules {
                schema: &schema,
                ..self.rules()
            },
        )
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
        crate::unnamed::hidden_classes(
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

/// An id decoded for reports ([`term_text`]); `#id` if the dictionary lacks it.
pub fn decoded(term: Option<nrese_rdf::Term>, id: u64) -> String {
    term.map_or_else(|| format!("#{id}"), |term| term_text(&term))
}

/// A term as reject reports show it: an IRI plainly, anything else in N-Triples form.
pub(crate) fn term_text(term: &nrese_rdf::Term) -> String {
    match term {
        nrese_rdf::Term::NamedNode(node) => node.as_str().to_owned(),
        other => other.to_string(),
    }
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

    fn blank_node_ids(&self) -> Option<(u64, u64)> {
        let (low, high) = TermId::kind_range(TermKind::BlankNode);
        Some((low.raw(), high.raw()))
    }
}

/// Whether RDF can store the triple: an IRI or blank node subject and an IRI predicate.
pub(crate) fn storable([subject, predicate, _]: Triple) -> bool {
    matches!(
        TermId::from_raw(subject).kind(),
        TermKind::Iri | TermKind::BlankNode
    ) && TermId::from_raw(predicate).kind() == TermKind::Iri
}

pub(crate) fn encode([s, p, o]: Triple) -> EncodedTriple {
    EncodedTriple::new(
        TermId::from_raw(s),
        TermId::from_raw(p),
        TermId::from_raw(o),
    )
}

pub(crate) fn triple(quad: EncodedQuad) -> Triple {
    [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()]
}

pub(crate) fn pattern([s, p, o]: [Option<u64>; 3], graph: GraphSelector) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph,
    }
}
