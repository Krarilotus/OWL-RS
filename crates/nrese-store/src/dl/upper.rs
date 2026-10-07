//! The upper bound U1 in a stack of its own (docs/design/owl2-dl.md §8, package 4.1).
//!
//! U1 is PAGOdA's datalog strengthening of the asserted ontology, compiled by
//! `nrese_dl::bounds` (disjunctions split, existentials c-Skolemised, `⊥` neutralised to
//! a clash fact). It is evaluated **over L ∪ data**: its input is the store's
//! materialised view (asserted and inferred statements, every graph) plus the program's
//! own facts, so U1 ⊇ L by construction. Only what it derives beyond that input is kept
//! here, in [`Stack`]: never in the engine's inferred stack, so never visible as inferred
//! data. Queries read it through a snapshot of their own ([`super::query`]).
//!
//! **Maintained per commit** by the reasoner's delta executor (`nrese_reasoner::delta`),
//! an ordinary incremental datalog materialisation: [`Upper::maintain`] takes the
//! commit's changes to the materialised view (asserted and inferred) as the change of
//! U1's input. A commit that changes the ontology's schema (anything but assertions over
//! the signature U1 was compiled for) recompiles U1 and evaluates it afresh
//! ([`is_assertion`]): its rules are the TBox's.
//!
//! **Data values by value** ([`Values`]): U1 reads every literal as its value's
//! representative, so keys, `hasValue` and joins over data values match equal values
//! written differently, as OWL 2 DL's identity of data values has it.
//!
//! **Equality by representatives** (ADR-0011's monotone backend, the reasoner's
//! `materialise_representatives_until`): U1's `owl:sameAs` classes are kept as one
//! representative each, never as the pairs of a class (OWL2Bench DL-1: one class of about
//! 78 k terms, 6 × 10⁹ pairs). The stack holds the closure over representatives and each
//! other identity as `identity sameAs representative`, which the upper view reads expanded
//! ([`Upper::classes`]). A U1 with classes is evaluated afresh on each commit, as is one a
//! commit's equality would give classes ([`Change::merges`]): the delta executor reads
//! facts as stored, and a class that may split is never maintained.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nrese_dl::bounds;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::{Normalised, Ontology};
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};
use nrese_reasoner::delta::{self, Base};
use nrese_reasoner::eval::{GroundProgram, Schema, Stop};
use nrese_reasoner::ir::{self, Triple};

/// U1's own facts beyond its input, indexed for every bound pattern.
#[derive(Debug, Default, Clone)]
pub struct Stack {
    spo: BTreeSet<[u64; 3]>,
    pos: BTreeSet<[u64; 3]>,
    osp: BTreeSet<[u64; 3]>,
    /// Facts per predicate, for the delta executor's estimates.
    per_predicate: std::collections::HashMap<u64, usize>,
}

impl Stack {
    pub fn len(&self) -> usize {
        self.spo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spo.is_empty()
    }

    pub fn contains(&self, t: &Triple) -> bool {
        self.spo.contains(t)
    }

    pub fn insert(&mut self, [s, p, o]: Triple) -> bool {
        if !self.spo.insert([s, p, o]) {
            return false;
        }
        self.pos.insert([p, o, s]);
        self.osp.insert([o, s, p]);
        *self.per_predicate.entry(p).or_default() += 1;
        true
    }

    pub fn remove(&mut self, [s, p, o]: Triple) -> bool {
        if !self.spo.remove(&[s, p, o]) {
            return false;
        }
        self.pos.remove(&[p, o, s]);
        self.osp.remove(&[o, s, p]);
        if let Some(n) = self.per_predicate.get_mut(&p) {
            *n -= 1;
        }
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = Triple> + '_ {
        self.spo.iter().copied()
    }

    /// An upper bound on the facts matching `pattern`, zero only if none does. O(log n).
    pub fn estimate(&self, pattern: [Option<u64>; 3]) -> usize {
        let bound = match pattern[1] {
            Some(p) => self.per_predicate.get(&p).copied().unwrap_or(0),
            None => self.len(),
        };
        if bound == 0 {
            return 0;
        }
        let mut any = false;
        self.first(pattern, &mut any);
        if any { bound } else { 0 }
    }

    /// Whether a fact matches `pattern`, by the index whose prefix it binds.
    fn first(&self, pattern: [Option<u64>; 3], any: &mut bool) {
        let hit = |set: &BTreeSet<[u64; 3]>, prefix: &[u64], test: &dyn Fn(&[u64; 3]) -> bool| {
            let mut low = [0u64; 3];
            let mut high = [u64::MAX; 3];
            low[..prefix.len()].copy_from_slice(prefix);
            high[..prefix.len()].copy_from_slice(prefix);
            set.range(low..=high).any(test)
        };
        *any = match pattern {
            [Some(s), Some(p), o] => hit(&self.spo, &[s, p], &|t| o.is_none_or(|o| t[2] == o)),
            [Some(s), None, o] => hit(&self.spo, &[s], &|t| o.is_none_or(|o| t[2] == o)),
            [None, Some(p), Some(o)] => hit(&self.pos, &[p, o], &|_| true),
            [None, Some(p), None] => hit(&self.pos, &[p], &|_| true),
            [None, None, Some(o)] => hit(&self.osp, &[o], &|_| true),
            [None, None, None] => !self.is_empty(),
        };
    }

    /// The facts matching `pattern`, by the index whose prefix it binds. O(log n + k).
    pub fn scan(&self, pattern: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        let range = |set: &BTreeSet<[u64; 3]>, prefix: &[u64]| {
            let mut low = [0u64; 3];
            let mut high = [u64::MAX; 3];
            low[..prefix.len()].copy_from_slice(prefix);
            high[..prefix.len()].copy_from_slice(prefix);
            set.range(low..=high).copied().collect::<Vec<_>>()
        };
        let matches = |t: &Triple| {
            pattern
                .iter()
                .zip(t)
                .all(|(bound, value)| bound.is_none_or(|b| b == *value))
        };
        let found: Vec<Triple> = match pattern {
            [Some(s), Some(p), _] => range(&self.spo, &[s, p]),
            [Some(s), None, _] => range(&self.spo, &[s]),
            [None, Some(p), Some(o)] => range(&self.pos, &[p, o])
                .into_iter()
                .map(|[p, o, s]| [s, p, o])
                .collect(),
            [None, Some(p), None] => range(&self.pos, &[p])
                .into_iter()
                .map(|[p, o, s]| [s, p, o])
                .collect(),
            [None, None, Some(o)] => range(&self.osp, &[o])
                .into_iter()
                .map(|[o, s, p]| [s, p, o])
                .collect(),
            [None, None, None] => self.spo.iter().copied().collect(),
        };
        for t in found.into_iter().filter(matches) {
            f(t);
        }
    }
}

/// A pattern over every graph.
fn pattern([s, p, o]: [Option<u64>; 3]) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph: GraphSelector::Any,
    }
}

/// Data values by value (OWL 2's identity of data values, [`nrese_xsd::owl::Value`]): each
/// literal U1 meets has a representative, the first literal of its value it met, and U1
/// reads, joins and derives representatives only. So a key, a `hasValue` and every join
/// over data values match equal values written differently (`"07"` and `"7"` as
/// `xsd:integer`, `"7"^^xsd:integer` and `"7.0"^^xsd:decimal`), as OWL 2 DL does: without
/// it, U1 would miss what such an equality entails and stop being an upper bound. A
/// literal whose value can't be read (ill-typed, beyond the value spaces) is its own.
/// Representatives are ids the store has already: nothing is interned (replicas too).
#[derive(Debug, Default)]
pub(crate) struct Values {
    /// A literal's representative, where it isn't the literal itself.
    rep: HashMap<u64, u64>,
    /// A representative's literals (itself first), for values written two or more ways.
    members: HashMap<u64, Vec<u64>>,
    by_value: HashMap<nrese_xsd::owl::Value, u64>,
    /// Literals already read.
    seen: HashSet<u64>,
}

impl Values {
    /// Notes `id` if it is a literal: its value's representative from now on.
    fn add(&mut self, view: &Snapshot, id: u64) {
        if !is_literal(id) || !self.seen.insert(id) {
            return;
        }
        let Some(value) = value_of(view, id) else {
            return;
        };
        match self.by_value.entry(value) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(id);
            }
            std::collections::hash_map::Entry::Occupied(e) => {
                let rep = *e.get();
                self.rep.insert(id, rep);
                self.members
                    .entry(rep)
                    .or_insert_with(|| vec![rep])
                    .push(id);
            }
        }
    }

    /// Notes the literals among the objects of `facts` whose predicate U1's rules compare
    /// values of (`compared`).
    fn add_objects<'a>(
        &mut self,
        view: &Snapshot,
        facts: impl IntoIterator<Item = &'a Triple>,
        compared: &HashSet<u64>,
    ) {
        for t in facts {
            if compared.contains(&t[1]) {
                self.add(view, t[2]);
            }
        }
    }

    /// `id`'s representative (itself for anything but a literal of a value met before).
    pub(crate) fn of(&self, id: u64) -> u64 {
        self.rep.get(&id).copied().unwrap_or(id)
    }

    /// `fact` with its object's representative (literals stand only as objects).
    fn map(&self, [s, p, o]: Triple) -> Triple {
        [s, p, self.of(o)]
    }

    /// Every term `rep` stands for: the literals with its value, or `rep` alone.
    fn members(&self, rep: u64) -> impl Iterator<Item = u64> + '_ {
        let all = self.members.get(&rep);
        all.into_iter()
            .flatten()
            .copied()
            .chain(all.is_none().then_some(rep))
    }
}

/// The predicates whose object values U1's rules compare: in a rule body, with a constant
/// object or an object variable another atom reads (a key's values, a `hasValue`). Other
/// literals are never compared, so they needn't be read by value (LUBM's names, e-mail
/// addresses and telephone numbers: 107,410 literals at LUBM(10)). The equality rules'
/// atoms with a variable predicate join only over `owl:sameAs`, which no literal has in U1.
fn compared_predicates(program: &bounds::Program) -> HashSet<u64> {
    let mut out = HashSet::new();
    for rule in &program.rules {
        for (i, atom) in rule.body.iter().enumerate() {
            let [_, bounds::Slot::Const(p), object] = atom.0 else {
                continue;
            };
            let compared = match object {
                bounds::Slot::Const(_) => true,
                bounds::Slot::Var(v) => {
                    rule.body
                        .iter()
                        .enumerate()
                        .any(|(j, other)| j != i && other.0.contains(&bounds::Slot::Var(v)))
                        || atom.0[..2].contains(&bounds::Slot::Var(v))
                }
            };
            if compared {
                out.insert(p);
            }
        }
    }
    out
}

/// The OWL 2 data value of the literal `id`; `None` for anything else, or a literal whose
/// value can't be read.
fn value_of(view: &Snapshot, id: u64) -> Option<nrese_xsd::owl::Value> {
    let nrese_rdf::Term::Literal(literal) = view.decode(TermId::from_raw(id))? else {
        return None;
    };
    let datatype = nrese_xsd::owl::Datatype::from_iri(literal.datatype().as_str())?;
    nrese_xsd::owl::Value::parse(literal.value(), datatype, literal.language()).ok()
}

/// U1's input and its stack as the delta executor reads them: the store's materialised
/// view after the change and the program's facts count as asserted, the stack as
/// inferred. The view's literals are read as their representatives ([`Values`]).
struct UpperBase<'a> {
    view: &'a Snapshot,
    facts: &'a HashSet<Triple>,
    stack: &'a Stack,
    values: &'a Values,
}

impl UpperBase<'_> {
    /// Whether the view has `s p o`, `o` standing for every literal of its value.
    fn in_view(&self, [s, p, o]: Triple) -> bool {
        self.values.members(o).any(|o| {
            self.view
                .quads_for_pattern_in(
                    ReadModel::Materialised,
                    &pattern([Some(s), Some(p), Some(o)]),
                )
                .next()
                .is_some()
        })
    }
}

impl Base for UpperBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        let [s, p, o] = bound;
        let objects: Vec<Option<u64>> = match o {
            Some(o) => self.values.members(o).map(Some).collect(),
            None => vec![None],
        };
        for o in objects {
            for q in self
                .view
                .quads_for_pattern_in(ReadModel::Materialised, &pattern([s, p, o]))
            {
                f(self
                    .values
                    .map([q.subject.raw(), q.predicate.raw(), q.object.raw()]));
            }
        }
        for t in self.facts {
            if bound.iter().zip(t).all(|(b, v)| b.is_none_or(|b| b == *v)) {
                f(*t);
            }
        }
        self.stack.scan(bound, f);
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let view = self.view.count_in(ReadModel::Materialised, &pattern(bound));
        usize::try_from(view).unwrap_or(usize::MAX) + self.stack.estimate(bound) + self.facts.len()
    }

    fn contains(&self, fact: Triple) -> bool {
        self.stack.contains(&fact) || self.is_asserted(fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.facts.contains(&fact) || self.in_view(fact)
    }
}

/// Gives IRIs and literals their ids in the store's dictionary (U1's own terms: fresh
/// classes, Skolem constants, the clash): interned on a primary, looked up on a replica
/// ([`super::source::resolve`]), where a term no record brought yet is `missing`.
pub(crate) struct Interner<'a> {
    pub resolve: &'a dyn Fn(TermRef<'_>) -> Option<TermId>,
    pub missing: bool,
}

impl Interner<'_> {
    fn id(&mut self, term: TermRef<'_>) -> u64 {
        match (self.resolve)(term) {
            Some(id) => id.raw(),
            None => {
                self.missing = true;
                0
            }
        }
    }
}

impl ir::Vocabulary for Interner<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        self.id(NamedNodeRef::new_unchecked(iri).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.id(
            LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype)).into(),
        )
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.id(LiteralRef::new_language_tagged_literal_unchecked(lexical, language).into())
    }
}

/// A commit's change to the stack ([`Upper::change`]).
#[derive(Default)]
pub struct Change {
    moved: Vec<Triple>,
    insert: Vec<Triple>,
    remove: Vec<Triple>,
    program: Option<GroundProgram>,
    pub elapsed: Duration,
    /// The change derives an equality between two terms: U1 then has classes, which it
    /// keeps by representatives, so it is evaluated afresh instead.
    merges: bool,
}

impl Change {
    /// Whether the change equates two terms ([`Self::merges`] field).
    pub fn merges(&self) -> bool {
        self.merges
    }
}

/// Why U1 isn't available: its evaluation ran out of its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaveUp(pub String);

/// U1: the compiled program, its rules as the reasoner's, and its stack.
pub struct Upper {
    pub program: bounds::Program,
    rules: Vec<ir::Rule>,
    schema: Schema,
    /// The program's facts.
    facts: HashSet<Triple>,
    pub stack: Stack,
    /// The classes U1 was compiled for: memberships in others change it.
    signature: HashSet<u64>,
    /// Its object and data properties: an assertion that uses one as the other kind, or
    /// another property, changes it.
    object_properties: HashSet<u64>,
    data_properties: HashSet<u64>,
    /// The ground program of the delta executor, for the next commit.
    ground: Option<GroundProgram>,
    /// The literals U1 compares and their values' representatives. A commit's new
    /// literals are noted when its change is computed: a representative stays one
    /// whether the commit happens or not.
    values: Mutex<Values>,
    /// The predicates whose values U1's rules compare ([`compared_predicates`]).
    compared: HashSet<u64>,
    /// The `owl:sameAs` classes, by representative: the stack is over them.
    classes: nrese_reasoner::representatives::EqualityClasses,
    rdf_type: u64,
    same_as: u64,
    /// How long the last evaluation or maintenance took.
    pub elapsed: Duration,
}

impl std::fmt::Debug for Upper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upper")
            .field("rules", &self.rules.len())
            .field("facts", &self.facts.len())
            .field("stack", &self.stack.len())
            .finish_non_exhaustive()
    }
}

fn slot(s: bounds::Slot) -> ir::Term {
    match s {
        bounds::Slot::Var(v) => ir::Term::Var(v),
        bounds::Slot::Const(t) => ir::Term::Const(t),
    }
}

/// U1's rules as the reasoner's (the atoms copied one to one, each constant literal as
/// its value's representative).
fn reasoner_rules(program: &bounds::Program, values: &Values) -> Vec<ir::Rule> {
    let slot = |s: bounds::Slot| match slot(s) {
        ir::Term::Const(t) => ir::Term::Const(values.of(t)),
        var => var,
    };
    let atom = |a: &bounds::Atom| ir::Atom(a.0.map(slot));
    program
        .rules
        .iter()
        .map(|r| ir::Rule {
            name: r.name.clone(),
            body: r.body.iter().map(atom).collect(),
            guards: r
                .distinct
                .iter()
                .map(|&(a, b)| ir::Guard::NotEqual(slot(a), slot(b)))
                .collect(),
            head: ir::Head::Facts(r.head.iter().map(atom).collect()),
        })
        .collect()
}

/// The materialised view's facts (every graph, each once).
fn view_facts(view: &Snapshot) -> Vec<Triple> {
    let mut facts: Vec<Triple> = view
        .quads_for_pattern_in(ReadModel::Materialised, &pattern([None, None, None]))
        .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
        .collect();
    facts.sort_unstable();
    facts.dedup();
    facts
}

impl Upper {
    /// Compiles U1 for `ontology` and evaluates it over `view` (the materialised view the
    /// ontology was read from). `resolve` gives U1's own terms their ids (`None`: not in
    /// a replica's dictionary yet, and U1 isn't available); `stop` ends the evaluation (U1
    /// is then not available). O(the closure).
    pub fn build(
        ontology: &Ontology,
        normalised: &Normalised,
        view: &Snapshot,
        resolve: &dyn Fn(TermRef<'_>) -> Option<TermId>,
        stop: Stop<'_>,
    ) -> Result<Self, GaveUp> {
        let started = Instant::now();
        let mut vocabulary = Interner {
            resolve,
            missing: false,
        };
        let program = bounds::compile(ontology, normalised, &mut |iri| {
            ir::Vocabulary::iri(&mut vocabulary, iri)
        });
        let schema = Schema::owl(&mut vocabulary);
        if vocabulary.missing {
            return Err(GaveUp(
                "its terms haven't reached this replica yet: the primary interns them in a commit"
                    .to_owned(),
            ));
        }
        // The literals U1 compares, by value: of the view, the program's facts, its rules.
        let compared = compared_predicates(&program);
        let mut values = Values::default();
        let mut input = view_facts(view);
        values.add_objects(view, &input, &compared);
        let program_facts: Vec<Triple> = program.facts.iter().map(|(f, _)| *f).collect();
        values.add_objects(view, &program_facts, &compared);
        for rule in &program.rules {
            for atom in rule.body.iter().chain(&rule.head) {
                for slot in atom.0 {
                    if let bounds::Slot::Const(t) = slot {
                        values.add(view, t);
                    }
                }
            }
        }
        let rules = reasoner_rules(&program, &values);
        let facts: HashSet<Triple> = program_facts.iter().map(|&f| values.map(f)).collect();
        for t in &mut input {
            *t = values.map(*t);
        }
        input.extend(facts.iter().copied());
        input.sort_unstable();
        input.dedup();
        // Equality by representatives: the closure over one term per class, the input's
        // facts that mention a class rewritten beside it, each other identity placed in
        // its class.
        let m = nrese_reasoner::batch::materialise_representatives_until(
            nrese_reasoner::batch::Input::Facts(input),
            &rules,
            None,
            &schema,
            nrese_reasoner::batch::Listing::Stored,
            stop,
        )
        .map_err(|_| GaveUp("the upper bound's evaluation was stopped".to_owned()))?;
        let same_as = program.names.same_as;
        let mut stack = Stack::default();
        for t in m.derived {
            stack.insert(t);
        }
        for (representative, members) in m.classes.classes() {
            for &member in members.iter().filter(|&&m| m != representative) {
                stack.insert([member, same_as, representative]);
            }
        }
        let s = &program.signature;
        let signature = s.classes.iter().copied().collect();
        let object_properties = s.object_properties.iter().copied().collect();
        let data_properties = s.data_properties.iter().copied().collect();
        Ok(Self {
            rdf_type: program.names.rdf_type,
            same_as: program.names.same_as,
            program,
            rules,
            schema,
            facts,
            stack,
            signature,
            object_properties,
            data_properties,
            ground: None,
            values: Mutex::new(values),
            compared,
            classes: m.classes,
            elapsed: started.elapsed(),
        })
    }

    /// Whether a changed statement only asserts something about individuals over the
    /// signature U1 was compiled for, so U1's rules stay as they are: a class membership
    /// in a known class, an object property U1 knows as one between named individuals, a
    /// data property it knows as one with a literal, or an equality. Anything else
    /// (schema, blank nodes, OWL's vocabulary, new vocabulary, a property used as the
    /// other kind, which the reading of the ontology then gives it) means recompiling.
    pub fn is_assertion(&self, [s, p, o]: Triple) -> bool {
        let named = |t: u64| TermId::from_raw(t).kind() == nrese_engine::TermKind::Iri;
        if !named(s) {
            return false;
        }
        if p == self.rdf_type {
            return named(o) && self.signature.contains(&o);
        }
        if p == self.same_as {
            return named(o);
        }
        (named(o) && self.object_properties.contains(&p))
            || (is_literal(o) && self.data_properties.contains(&p))
    }

    /// The stack's change for a commit, computed before it and applied after it
    /// ([`Self::apply`]), so a commit that doesn't happen leaves U1 as it was: `view` is
    /// the materialised view after the commit, `inserted` the facts new to the view,
    /// `deleted` those gone from it (each checked here against the view and the
    /// program's facts). Cost: the change's.
    pub fn change(
        &self,
        view: &Snapshot,
        inserted: &[Triple],
        deleted: &[Triple],
        stop: Stop<'_>,
    ) -> Result<Change, GaveUp> {
        let started = Instant::now();
        let mut values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        values.add_objects(view, inserted, &self.compared);
        let mut inserted: Vec<Triple> = inserted
            .iter()
            .map(|&t| values.map(t))
            .filter(|t| !self.facts.contains(t))
            .collect();
        inserted.sort_unstable();
        inserted.dedup();
        // A fact U1 derived that is now in its input: it leaves the stack, and its
        // consequences are already there.
        let moved: Vec<Triple> = inserted
            .iter()
            .filter(|t| self.stack.contains(t))
            .copied()
            .collect();
        inserted.retain(|t| !self.stack.contains(t));
        let mut deleted: Vec<Triple> = deleted
            .iter()
            .map(|&t| values.map(t))
            .filter(|t| !self.facts.contains(t))
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        let base = UpperBase {
            view,
            facts: &self.facts,
            stack: &self.stack,
            values: &values,
        };
        deleted.retain(|t| !base.in_view(*t));
        let mut change = Change {
            moved,
            ..Change::default()
        };
        if inserted.is_empty() && deleted.is_empty() {
            return Ok(change);
        }
        let rules = delta::Rules {
            rules: &self.rules,
            lists: None,
            schema: &self.schema,
        };
        let update = delta::update_until(
            &base,
            &inserted,
            &deleted,
            rules,
            self.ground.as_ref(),
            stop,
        )
        .map_err(|_| GaveUp("the upper bound's maintenance was stopped".to_owned()))?;
        change.program = update.program;
        // An equality the commit brings (the RL closure's, say: a functional property is
        // OWL 2 RL) or U1 derives: U1 then has classes.
        let same_as = self.same_as;
        let equates = |t: &Triple| t[1] == same_as && t[0] != t[2];
        change.merges = inserted.iter().any(equates) || update.insert.iter().any(equates);
        change.remove = update.remove;
        change.insert = update
            .insert
            .into_iter()
            .filter(|t| !base.is_asserted(*t))
            .collect();
        change.elapsed = started.elapsed();
        Ok(change)
    }

    /// Applies a commit's change ([`Self::change`]) once the commit is done.
    pub fn apply(&mut self, change: Change) {
        for t in change.moved.into_iter().chain(change.remove) {
            self.stack.remove(t);
        }
        for t in change.insert {
            self.stack.insert(t);
        }
        if let Some(program) = change.program {
            self.ground = Some(program);
        }
        self.elapsed = change.elapsed;
    }

    /// The clashes the stack would have after `change`.
    pub fn clashes_after(&self, change: &Change) -> usize {
        let clash = self.program.names.clash;
        let is_clash = |t: &Triple| t[1] == clash && t[2] == clash;
        let gone = change
            .moved
            .iter()
            .chain(&change.remove)
            .filter(|t| is_clash(t) && self.stack.contains(t))
            .count();
        let new = change
            .insert
            .iter()
            .filter(|t| is_clash(t) && !self.stack.contains(t))
            .count();
        (self.clashes() + new).saturating_sub(gone)
    }

    /// The terms a clash was derived at (`⊥` fired in U1): the ontology may be
    /// inconsistent.
    pub fn clashes(&self) -> usize {
        let clash = self.program.names.clash;
        let mut n = 0;
        self.stack
            .scan([None, Some(clash), Some(clash)], &mut |_| n += 1);
        n
    }

    /// Whether U1 proves the ontology consistent (PAGOdA, Theorem 5.5 (i)): no clash, and
    /// U1 checks every `⊥` ([`bounds::Program::proves_consistency`]).
    pub fn proves_consistency(&self) -> bool {
        self.program.proves_consistency() && self.clashes() == 0
    }

    /// U1's `owl:sameAs` classes: its stack is over their representatives, each other
    /// identity stored as `identity sameAs representative`; empty where nothing is
    /// equated. A U1 with classes is evaluated afresh on each commit.
    pub fn classes(&self) -> &nrese_reasoner::representatives::EqualityClasses {
        &self.classes
    }

    /// `owl:sameAs`, as U1's program names it.
    pub fn same_as(&self) -> u64 {
        self.same_as
    }

    /// How many literals U1 reads by value ([`Values`]).
    pub fn literals_by_value(&self) -> usize {
        self.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .seen
            .len()
    }

    /// Whether `term` is one of U1's own (a Skolem constant, a fresh class, the clash).
    pub fn is_internal(&self, term: u64) -> bool {
        self.program.is_internal(term)
    }
}

/// Whether a term is a literal (of any of the engine's literal kinds).
fn is_literal(t: u64) -> bool {
    use nrese_engine::TermKind;
    !matches!(
        TermId::from_raw(t).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

#[cfg(test)]
mod tests {
    use super::Stack;

    /// The delta executor reads estimates per job: they must be upper bounds, and zero
    /// only where nothing matches (a zero skips the job), and cost an index probe, not a
    /// count of the matches.
    #[test]
    fn stack_estimates_are_bounds_and_zero_only_without_a_match() {
        let mut stack = Stack::default();
        for s in 0..50u64 {
            stack.insert([s, 1, s + 100]);
            stack.insert([s, 2, 7]);
        }
        stack.remove([0, 1, 100]);
        let patterns = [
            [None, None, None],
            [Some(3), None, None],
            [Some(3), Some(1), None],
            [Some(3), Some(1), Some(103)],
            [Some(3), Some(1), Some(104)],
            [None, Some(2), Some(7)],
            [None, Some(2), Some(8)],
            [None, None, Some(7)],
            [None, Some(9), None],
            [Some(0), Some(1), None],
        ];
        for pattern in patterns {
            let mut matches = 0;
            stack.scan(pattern, &mut |_| matches += 1);
            let estimate = stack.estimate(pattern);
            assert!(estimate >= matches, "{pattern:?}: {estimate} < {matches}");
            assert_eq!(estimate == 0, matches == 0, "{pattern:?}");
        }
        // Bounded by the predicate's facts, not the stack's.
        assert_eq!(stack.estimate([None, Some(2), Some(7)]), 50);
        assert_eq!(stack.estimate([Some(3), Some(1), None]), 49);
    }
}
