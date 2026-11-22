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
//! those are dropped. Commits therefore don't see them either, which matters for rules
//! that turn them back into storable facts (an `owl:inverseOf` on a datatype property,
//! which OWL 2 doesn't allow), and for an `owl:sameAs` between two spellings of one value,
//! whose copies of a later commit's statements are missing: the closure and each commit
//! report one ([`Closure::literal_equality`], [`Maintenance::literal_equality`]), and
//! [`Program::derives_literal_equality`] finds one in a stored state.

mod datatypes;
mod explain;
mod maintain;
mod materialise;

use nrese_engine::{
    EncodedQuad, EncodedTriple, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TermKind,
};
use nrese_owl::mapping::AnnotationProperties;
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};

use crate::RuleProgram;
use crate::batch::Schema;
use crate::delta;
use crate::eval::GroundProgram;
use crate::ir::{AnnotationIds, Guard, Rule, Triple, Vocabulary};
use crate::lists::ListVocabulary;
use crate::unnamed::UnnamedVocabulary;

pub use explain::{
    InferenceStep, JUSTIFICATIONS_AT_MOST, JustificationAnswer, JustificationMode,
    JustifiedStatement, explain_fact, explain_violation, justify_fact,
};
pub use maintain::{
    EngineBase, Maintenance, install_closure, maintain, maintain_with, maintain_with_report_budget,
    maintain_within,
};
pub use materialise::{
    Closure, equality_report_until, equality_report_with, materialise, materialise_until,
    materialise_with, materialise_within,
};

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
    /// The program makes a declared class's members things
    /// ([`crate::unnamed::members_are_things`]), asked only of a program that hides unnamed
    /// classes.
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
    /// The maintenance plan every commit takes where it applies (the differential tests
    /// of each plan); `None`: the cheapest ([`delta::policy`]).
    plan: Option<delta::policy::Plan>,
    /// Whether the axioms' own closure concludes only facts a store can hold, once a first
    /// load asked ([`Self::axioms_conclude_only_storable`]).
    axioms_storable: std::sync::OnceLock<bool>,
    /// Where a rule reads the data's annotation properties ([`Guard::NotAnnotation`]): the
    /// built-in ones and the declaring statements, by the OWL reader's rule.
    annotations: Option<AnnotationProperties>,
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
    /// The list vocabulary by `snapshot`'s ids: an IRI it doesn't hold gets an id no statement
    /// uses.
    fn list_vocabulary(snapshot: &Snapshot) -> ListVocabulary {
        let absent = TermId::from_raw(u64::MAX >> 1);
        let intern = |term: TermRef<'_>| {
            Ok::<_, std::convert::Infallible>(snapshot.lookup(term).unwrap_or(absent))
        };
        ListVocabulary::new(&mut Constants {
            intern: &intern,
            refused: None,
        })
    }

    /// Whether a run of a program with list rules over `snapshot` would leave out a list
    /// axiom (any list diagnostic). Only the list vocabulary is bound, by the snapshot's ids
    /// ([`Self::list_vocabulary`]); no rule is compiled.
    pub fn leaves_out_a_list(snapshot: &Snapshot) -> bool {
        let lists = Self::list_vocabulary(snapshot);
        let base = explain::SnapshotBase {
            snapshot,
            axioms: &[],
        };
        let facts = crate::eval::AllFacts(&crate::explain::Whole(&base));
        !crate::lists::instantiate(&lists, &facts).1.is_empty()
    }

    /// Whether one of `quads` (by `at`'s ids) can change what [`Self::leaves_out_a_list`]
    /// reads: a list fact ([`ListVocabulary::is_list_fact`]); any, where `at`'s reads expand by
    /// `owl:sameAs` classes (compact equality). If none can, a version that differs from its
    /// base by `quads` alone leaves out the lists its base does.
    pub fn touches_a_list(at: &Snapshot, mut quads: impl Iterator<Item = EncodedQuad>) -> bool {
        let lists = Self::list_vocabulary(at);
        at.equality_classes().is_some() || quads.any(|quad| lists.is_list_fact(triple(quad)))
    }

    /// Whether one of `quads` (by `lookup`'s ids) has the shape of either annotation
    /// declaration. A change with one may change the set ([`Self::annotation_set_changed`]).
    pub fn touches_a_declaration(
        lookup: &dyn Fn(TermRef<'_>) -> Option<TermId>,
        mut quads: impl Iterator<Item = EncodedQuad>,
    ) -> bool {
        let mut id = |iri: &str| lookup(NamedNodeRef::new_unchecked(iri).into()).map(TermId::raw);
        let declaring = AnnotationProperties::new(&mut id);
        quads.any(|quad| declaring.declares(triple(quad)))
    }

    /// Whether the asserted statements change the annotation properties read by this program.
    pub fn annotation_set_changed(&self, base: &Snapshot, pending: &Snapshot) -> bool {
        self.changed_annotation_ids(self.annotation_ids(base).as_ref(), pending)
            .is_some()
    }

    /// The pending annotation properties when they differ from the base's.
    fn changed_annotation_ids(
        &self,
        base: Option<&AnnotationIds>,
        pending: &Snapshot,
    ) -> Option<AnnotationIds> {
        self.annotation_ids(pending)
            .filter(|after| Some(after) != base)
    }

    /// The rules for a run over `snapshot`: where a rule reads the annotation properties,
    /// with [`Self::annotation_ids`], fixed for the run.
    fn rules_for(&self, snapshot: &Snapshot) -> std::borrow::Cow<'_, [Rule]> {
        self.rules_with(self.annotation_ids(snapshot).as_ref())
    }

    /// Where a rule reads the annotation properties, those of `snapshot`'s asserted
    /// statements, read as the OWL reader reads them (an IRI it may name one, declared in any
    /// graph, and the built-in ones); `None` where none does.
    fn annotation_ids(&self, snapshot: &Snapshot) -> Option<AnnotationIds> {
        let annotations = self.annotations.as_ref()?;
        let mut read = annotations.clone();
        let named = |s: TermId| match snapshot.decode(s) {
            Some(nrese_rdf::Term::NamedNode(n)) => nrese_owl::may_name_annotation(n.as_str()),
            _ => false,
        };
        for [p, o] in annotations.declaring() {
            let declared = pattern([None, Some(p), Some(o)], GraphSelector::Any);
            for quad in snapshot.quads_for_pattern_in(ReadModel::Asserted, &declared) {
                read.read(triple(quad), named(quad.subject));
            }
        }
        Some(AnnotationIds::new(read.properties().collect()))
    }

    /// The rules, where a rule reads the annotation properties, with `set`.
    fn rules_with(&self, set: Option<&AnnotationIds>) -> std::borrow::Cow<'_, [Rule]> {
        let Some(set) = set else {
            return std::borrow::Cow::Borrowed(&self.rules);
        };
        let mut rules = self.rules.clone();
        crate::rulesets::bind_annotations(&mut rules, set);
        std::borrow::Cow::Owned(rules)
    }

    /// The rules as the delta executor takes them; a rule that reads the annotation
    /// properties with none: a run over a state takes them from [`Self::rules_for`].
    pub fn rules(&self) -> delta::Rules<'_> {
        delta::Rules {
            rules: &self.rules,
            lists: self.lists.as_ref(),
            schema: &self.schema,
            equality: self.same_as,
        }
    }

    /// Compiles `program` against the engine dictionary; `intern` gives ids to the
    /// constants.
    pub fn compile(program: &RuleProgram, intern: &dyn Fn(TermRef<'_>) -> TermId) -> Self {
        match Self::try_compile(program, &|term| {
            Ok::<_, std::convert::Infallible>(intern(term))
        }) {
            Ok(compiled) => compiled,
            Err(never) => match never {},
        }
    }

    /// [`Self::compile`] with an interner that can refuse (a private transaction's, whose
    /// budget charges its terms: dictionary lifecycle V1): the first refusal, and no
    /// program, whose constants would hold no id for the refused term.
    pub fn try_compile<E>(
        program: &RuleProgram,
        intern: &dyn Fn(TermRef<'_>) -> Result<TermId, E>,
    ) -> Result<Self, E> {
        let mut constants = Constants {
            intern,
            refused: None,
        };
        let rules = program
            .rules(&mut constants)
            .expect("the built-in rulesets parse (tested), user rules are checked at startup");
        let blank_nodes = constants.blank_node_ids().filter(|_| program.named_keys);
        let lists = program
            .has_list_rules()
            .then(|| ListVocabulary::new(&mut constants).named_keys(blank_nodes));
        let schema = Schema::owl(&mut constants);
        let unnamed = UnnamedVocabulary::new(&mut constants);
        let rdf_type = constants.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let same_as = crate::representatives::same_as(&rules, schema.same_as());
        let mut axioms = program
            .axiom_triples(&mut constants)
            .expect("the built-in axioms parse (tested), user facts are checked at startup");
        axioms.sort_unstable();
        let reads =
            |rule: &Rule| (rule.guards.iter()).any(|g| matches!(g, Guard::NotAnnotation(..)));
        let annotations = rules
            .iter()
            .any(reads)
            .then(|| AnnotationProperties::new(&mut |iri| Some(constants.iri(iri))));
        if let Some(refused) = constants.refused {
            return Err(refused);
        }
        Ok(Self {
            program: program.clone(),
            rules,
            lists,
            schema,
            hide_unnamed_classes: false,
            unnamed,
            things: false,
            rdf_type,
            same_as,
            by_representatives: true,
            store_representatives: false,
            axioms,
            plan: None,
            axioms_storable: std::sync::OnceLock::new(),
            annotations,
        })
    }

    /// This program computing full closures over representatives of `owl:sameAs` classes
    /// (the default), or with the replacement rules.
    #[must_use]
    pub fn by_representatives(mut self, representatives: bool) -> Self {
        self.by_representatives = representatives;
        self
    }

    /// This program keeping the closure over representatives in the store (with
    /// [`Self::by_representatives`]), or every copy. Only a program whose name says it
    /// closes equality ([`RuleProgram::closes_equality`]) keeps them: that name is what the
    /// read side expands by, a reopened store's included.
    #[must_use]
    pub fn storing_representatives(mut self, store: bool) -> Self {
        self.store_representatives = store && RuleProgram::closes_equality(&self.program.name());
        self
    }

    /// Whether the inferred stack holds the closure over representatives.
    pub fn stores_representatives(&self) -> bool {
        self.by_representatives && self.store_representatives && self.same_as.is_some()
    }

    /// The `owl:sameAs` id the inferred stack is kept over, if it is
    /// ([`Self::stores_representatives`]): what reads expand by.
    pub fn stored_equality(&self) -> Option<TermId> {
        self.same_as
            .filter(|_| self.stores_representatives())
            .map(TermId::from_raw)
    }

    /// This program maintaining every commit by `plan` where it applies, or by the
    /// cheapest plan (`None`, the default).
    #[must_use]
    pub fn maintaining_by(mut self, plan: Option<delta::policy::Plan>) -> Self {
        self.plan = plan;
        self
    }

    /// This program leaving out memberships in unnamed classes nothing consumes, or not; or
    /// the interruption of asking whether its rules make those classes' members things, by
    /// the process's memory limit ([`crate::unnamed::members_are_things`]).
    pub fn hiding_unnamed_classes(mut self, hide: bool) -> Result<Self, delta::Interrupted> {
        self.hide_unnamed_classes = hide;
        self.things = hide && crate::unnamed::members_are_things(&self.program)?;
        Ok(self)
    }

    /// The ruleset's axiomatic triples, sorted.
    pub fn axioms(&self) -> &[Triple] {
        &self.axioms
    }

    /// The ground program of the committed state `snapshot`, as the delta executor
    /// builds it.
    pub fn ground_program(&self, snapshot: &Snapshot) -> GroundProgram {
        let schema = self.schema_for(snapshot);
        let rules = self.rules_for(snapshot);
        let base = explain::SnapshotBase {
            snapshot,
            axioms: &self.axioms,
        };
        delta::program(
            &base,
            delta::Rules {
                rules: &rules,
                schema: &schema,
                ..self.rules()
            },
        )
    }

    /// Whether the rules equate a literal with another term over `snapshot`'s statements (an
    /// `owl:sameAs` with a literal subject), as its closure did and dropped: for a store
    /// reopened without a rematerialisation, which kept no record of it. Those statements
    /// are the closure but for statements about literals (a literal subject or predicate,
    /// which no store holds) and what an equality of one would copy, so one step of each rule
    /// that can conclude one decides. Where a body atom reads a term of the conclusion at its
    /// subject or predicate, it may need such a statement (`prp-rng` types a literal, and
    /// `cls-maxqc3` reads that type): the rule runs without it, and a match counts where the
    /// conclusion's subject is a literal or left unbound, so the answer errs towards an
    /// equality (`literal_equality_atoms`). Where the program holds a statement with a literal
    /// subject without premises (an axiom, a bodiless instance: `cls-oo` over a literal), or a
    /// rule concludes one over the statements held (the first such statement the closure had
    /// has only held premises: `prp-rng` typing a literal), such statements may fill any atom whose
    /// subject is a variable no atom reads as a predicate (`cls-hv1` giving that literal two
    /// values of a functional property, which `prp-fp` reads): those atoms are left out too.
    /// `Err` once `stop` fires.
    pub fn derives_literal_equality(
        &self,
        snapshot: &Snapshot,
        stop: crate::eval::Stop<'_>,
    ) -> Result<bool, delta::Interrupted> {
        use crate::eval::{Poller, Seg, plan, value, walk_until};
        use crate::ir::{Head, Term};
        let Some(same_as) = self.same_as else {
            return Ok(false);
        };
        let interrupted = || delta::Interrupted::default();
        let poll = Poller::new(stop).ok_or_else(interrupted)?;
        let base = explain::SnapshotBase {
            snapshot,
            axioms: &self.axioms,
        };
        let source = crate::explain::Whole(&base);
        let ground = self.ground_program(snapshot);
        // Whether `atoms` of `rule` match with `found` taking a match: every atom left out
        // matches, as nothing the store holds decides against it.
        let search =
            |rule: &Rule, atoms: &[usize], found: &mut dyn FnMut(&[Option<u64>]) -> bool| {
                let Some(&first) = atoms.first() else {
                    return true;
                };
                let order = plan(&source, rule, atoms, first, |_| Seg::All, &poll);
                let mut bindings = vec![None; rule.variables()];
                walk_until(
                    &source,
                    &rule.body,
                    &rule.guards,
                    &order,
                    0,
                    &mut bindings,
                    &poll,
                    found,
                )
            };
        let literal = |id: u64| {
            !matches!(
                TermId::from_raw(id).kind(),
                TermKind::Iri | TermKind::BlankNode
            )
        };
        let concluded = std::cell::OnceCell::new();
        // Every source of a first statement with a literal subject: a fact the program holds
        // without premises (an axiom, a bodiless instance: `cls-oo` typing a literal its
        // `owl:oneOf` lists), or a rule's conclusion over held premises, whose subject is then
        // a constant or a term a premise reads at its object (a premise's subject is held,
        // never a literal; one at its predicate needs `eq-rep-p` over an `owl:sameAs` with a
        // literal, the equality looked for). The transitive closure's subject is a premise's.
        let literal_subjects = || {
            *concluded.get_or_init(|| {
                let fact = |triple: &crate::ir::Triple| literal(triple[0]);
                (self.axioms.iter()).any(fact)
                    || ground.bodiless.iter().any(fact)
                    || ground.rules.iter().any(|rule| {
                        let Head::Facts(heads) = &rule.head else {
                            return false;
                        };
                        heads.iter().any(|head| {
                            let subject = head.0[0];
                            let may = match subject {
                                Term::Const(c) => literal(c),
                                Term::Var(_) => rule.body.iter().any(|atom| atom.0[2] == subject),
                            };
                            let all: Vec<usize> = (0..rule.body.len()).collect();
                            may && search(rule, &all, &mut |b| {
                                value(subject, b).is_some_and(literal)
                            })
                        })
                    })
            })
        };
        for rule in &ground.rules {
            let Head::Facts(heads) = &rule.head else {
                continue;
            };
            if rule.body.is_empty() {
                continue;
            }
            for head in heads {
                let Some(atoms) = literal_equality_atoms(rule, head, same_as, false) else {
                    continue;
                };
                let [subject, _, object] = head.0;
                let mut equates = |bindings: &[Option<u64>]| {
                    let object = value(object, bindings);
                    value(subject, bindings)
                        .is_none_or(|subject| object != Some(subject) && literal(subject))
                };
                let mut found = search(rule, &atoms, &mut equates);
                let wider = literal_equality_atoms(rule, head, same_as, true).unwrap_or_default();
                if !found && !poll.stopped() && wider.len() < atoms.len() && literal_subjects() {
                    found = search(rule, &wider, &mut equates);
                }
                if poll.stopped() {
                    return Err(interrupted());
                }
                if found {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Whether the rules can conclude an `owl:sameAs` at all over `snapshot`'s schema, whatever
    /// its data: a rule instance concludes one from premises none of which is an `owl:sameAs`
    /// (a functional or inverse-functional property, an at-most-one restriction, a key, a
    /// property a schema term makes `owl:sameAs`; the equality rules only spread one), or a
    /// fact the program holds without premises is an `owl:sameAs` with a literal. For a store
    /// whose closure keeps one representative of equal values (`storing_representatives`),
    /// whose statements can't show what a premise held ([`Self::derives_literal_equality`]
    /// counts them): its premises are counted as present.
    pub fn concludes_equality(&self, snapshot: &Snapshot) -> bool {
        use crate::ir::{Head, Term};
        let Some(same_as) = self.same_as else {
            return false;
        };
        let ground = self.ground_program(snapshot);
        let literal = |id: u64| {
            !matches!(
                TermId::from_raw(id).kind(),
                TermKind::Iri | TermKind::BlankNode
            )
        };
        let fact = |t: &Triple| t[1] == same_as && (literal(t[0]) || literal(t[2]));
        // A head whose predicate is `owl:sameAs`, or a variable that may be.
        let may_be = |p: Term| p == Term::Const(same_as) || matches!(p, Term::Var(_));
        (self.axioms.iter()).any(fact)
            || ground.bodiless.iter().any(fact)
            || ground.rules.iter().any(|rule| {
                let Head::Facts(heads) = &rule.head else {
                    return false;
                };
                rule.body
                    .iter()
                    .all(|atom| atom.0[1] != Term::Const(same_as))
                    && heads
                        .iter()
                        .any(|head| may_be(head.0[1]) && head.0[0] != head.0[2])
            })
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

/// An id decoded for reports (`term_text`); `#id` if the dictionary lacks it.
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

/// Interns the rules' constants into the engine dictionary. A refusal is kept (the first),
/// its constant given a placeholder id that [`Program::try_compile`] never lets out: it
/// returns the refusal instead of the program.
struct Constants<'a, E> {
    intern: &'a dyn Fn(TermRef<'_>) -> Result<TermId, E>,
    refused: Option<E>,
}

impl<E> Constants<'_, E> {
    fn id(&mut self, term: TermRef<'_>) -> u64 {
        match (self.intern)(term) {
            Ok(id) => id.raw(),
            Err(refused) => {
                self.refused.get_or_insert(refused);
                0
            }
        }
    }
}

impl<E> Vocabulary for Constants<'_, E> {
    fn iri(&mut self, iri: &str) -> u64 {
        self.id(NamedNodeRef::new_unchecked(iri).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        let literal = LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype));
        self.id(literal.into())
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        let literal = LiteralRef::new_language_tagged_literal_unchecked(lexical, language);
        self.id(literal.into())
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

/// Whether `fact` equates a literal with another term: an `owl:sameAs` (`same_as`) between
/// two different terms whose subject no store holds (a literal: [`storable`]). The rules
/// derive one where a functional property or an at-most-one restriction holds one value in
/// two spellings; dropped, it copies nothing into a later commit's statements.
pub(crate) fn equates_literal([subject, predicate, object]: Triple, same_as: Option<u64>) -> bool {
    Some(predicate) == same_as
        && subject != object
        && !matches!(
            TermId::from_raw(subject).kind(),
            TermKind::Iri | TermKind::BlankNode
        )
}

/// The body atoms of `rule` that a search for an equality of a literal by `head` runs
/// ([`equates_literal`]), or `None` where `head` concludes none: it isn't an `owl:sameAs`
/// whose subject is a variable other than its object. Every atom but one that reads a term
/// of `head` at its subject or predicate: no stored statement has a literal there, while
/// the closure had such statements and dropped them (`prp-rng` typing a literal). An
/// `owl:sameAs` atom stays: a literal at its subject is an equality of a literal already,
/// and the first one the rules conclude is found by a run of the rule that concluded it.
/// `wide`, where the rules conclude statements with a literal subject: also every other atom
/// whose subject is a variable that no atom reads as a predicate, which may be a literal.
fn literal_equality_atoms(
    rule: &Rule,
    head: &crate::ir::Atom,
    same_as: u64,
    wide: bool,
) -> Option<Vec<usize>> {
    use crate::ir::Term;
    let [subject, predicate, object] = head.0;
    if !matches!(subject, Term::Var(_)) || predicate != Term::Const(same_as) || object == subject {
        return None;
    }
    let concluded =
        |term: Term| matches!(term, Term::Var(_)) && (term == subject || term == object);
    let literal = |term: Term| {
        wide && matches!(term, Term::Var(_)) && rule.body.iter().all(|atom| atom.0[1] != term)
    };
    let kept = (0..rule.body.len()).filter(|&i| {
        let [s, p, _] = rule.body[i].0;
        !concluded(p) && (p == Term::Const(same_as) || !(concluded(s) || literal(s)))
    });
    Some(kept.collect())
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
