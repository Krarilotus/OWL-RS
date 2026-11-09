//! Equality by representatives, read side (work package W4, stage B).
//!
//! With equality on ([`crate::Engine::set_equality`]), the default graph of the inferred
//! stack holds the closure over one representative per `owl:sameAs` class (its smallest
//! id) instead of every fact for every identity: a class of k identities saves up to k³
//! copies of a fact. Each identity other than the representative is stored as
//! `identity owl:sameAs representative`, which is how a version finds its classes.
//!
//! Reads of the default graph in the models with inferred statements see the closure as if
//! every copy were stored: the stored facts over representatives, each expanded to every
//! identity of its terms ([`Expand`]). Constants of a pattern are looked up by their
//! representative and kept in the answer. Stored facts that are not over representatives
//! (the `sameAs` facts that place an identity in its class, and asserted facts about other
//! identities, which their rewritten copy stands for) are skipped. Named graphs and the
//! asserted model read the statements as stored. As without equality by representatives,
//! a statement asserted only in a named graph is not in the default graph
//! ([`Visibility`]); the copies for its other identities are.
//!
//! Expansion keeps the scan order: a representative is its class's smallest id, so every
//! copy of a fact sorts at or after the fact itself, and a heap holds the copies until the
//! scan has passed them.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use hashbrown::HashMap;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
use parking_lot::Mutex;

use super::{ReadModel, Stack, Version};
use crate::quad::{EncodedQuad, GraphSelector, Key, Permutation, QuadPattern};
use crate::term::TermId;

/// The classes of one version, computed at the first read that needs them and passed on
/// to the next version while no commit changes a `sameAs` statement.
#[derive(Default)]
pub(crate) struct EqualityCell(Mutex<Option<CachedClasses>>);

/// `owl:sameAs`, its classes, and their canonical view once asked for.
type CachedClasses = (TermId, Arc<Classes>, Option<Arc<Classes>>);

impl std::fmt::Debug for EqualityCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EqualityCell").finish_non_exhaustive()
    }
}

impl EqualityCell {
    /// The classes of `version` under `same_as`.
    pub(crate) fn classes(&self, version: &Version, same_as: TermId) -> Arc<Classes> {
        self.classes_with(version, same_as, Unmetered)
            .expect("an unmetered build never stops")
    }

    /// [`Self::classes`], a build's work counted on `meter`; `None` where it stopped, nothing
    /// kept. Built outside the cell's lock (REAS-3, master 9 Nov 01:37 (d)): a reader that
    /// finds no classes builds its own, the first build finished is kept and a later one
    /// takes it, so a reader never waits out another's build unseen by its own stop; under
    /// contention two builds may run at once (their charges are transient). The rule 3.0c's
    /// own-state slot follows.
    pub(crate) fn classes_with<M: Metering>(
        &self,
        version: &Version,
        same_as: TermId,
        meter: M,
    ) -> Option<Arc<Classes>> {
        if let Some((id, classes, _)) = self.0.lock().as_ref()
            && *id == same_as
        {
            return Some(Arc::clone(classes));
        }
        let built = Arc::new(Classes::of_with(version, same_as, meter)?);
        let mut cell = self.0.lock();
        if let Some((id, classes, _)) = cell.as_ref()
            && *id == same_as
        {
            return Some(Arc::clone(classes));
        }
        *cell = Some((same_as, Arc::clone(&built), None));
        Some(built)
    }

    /// A cell already holding `classes` under `same_as`, for a version whose default graph
    /// is that of the version they are the classes of.
    fn holding(same_as: TermId, classes: Arc<Classes>) -> Self {
        Self(Mutex::new(Some((same_as, classes, None))))
    }

    /// The classes of `version` read canonically: the same aliases, no members to expand
    /// to ([`Classes::canonical_view`]); made once per version. Made outside the cell's lock,
    /// as the classes are (REAS-3a sol-loop r1 S5: a counted reader that only checks the cell
    /// mustn't wait out a copy of every alias), the first view finished kept.
    pub(crate) fn canonical(&self, version: &Version, same_as: TermId) -> Arc<Classes> {
        let full = self.classes(version, same_as);
        if let Some((_, classes, Some(canonical))) = self.0.lock().as_ref()
            && Arc::ptr_eq(classes, &full)
        {
            return Arc::clone(canonical);
        }
        let view = Arc::new(full.canonical_view());
        let mut cell = self.0.lock();
        match cell.as_mut() {
            Some((_, classes, canonical)) if Arc::ptr_eq(classes, &full) => {
                Arc::clone(canonical.get_or_insert(view))
            }
            _ => view,
        }
    }
}

/// What canonical reads of one version read, made at the first such read and owned by
/// that version (`Version::canonical`): it lives as long as the version, and holds
/// nothing that holds the version (the view is another version, over the same runs), so
/// it can't keep the version alive. The equality cell, which versions pass on, holds
/// only the classes.
#[derive(Default)]
pub(crate) struct CanonicalCache {
    /// The canonical view under `owl:sameAs` ([`canonical_version`]), which holds its own
    /// charge; `None` where the view is the version itself.
    view: Mutex<Option<(TermId, Option<View>)>>,
    /// Whether a triple term of the version names an alias under `owl:sameAs`
    /// (`Snapshot::triple_terms_name_aliases`).
    triple_terms: Mutex<Option<(TermId, bool)>>,
}

impl std::fmt::Debug for CanonicalCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanonicalCache").finish_non_exhaustive()
    }
}

/// A canonical view: the version canonical reads read, and its charge, shared by the cache
/// and its readers. The last holder frees the view and then releases the charge: it takes
/// the view out of the shared allocation, which goes first ([`Arc::into_inner`] gives the
/// view to exactly one holder, whichever drops last), then the view's data, then its
/// reservation, the last field of [`ViewData`] (fields drop in declaration order).
#[derive(Clone)]
pub(crate) struct View(Option<Arc<ViewData>>);

/// What a [`View`] holds.
struct ViewData {
    version: Arc<Version>,
    /// What the view's allocations are charged to. Last: released after everything it
    /// covers is freed.
    _held: Reservation,
}

impl View {
    pub(crate) fn version(&self) -> &Arc<Version> {
        &self
            .0
            .as_ref()
            .expect("taken only by the last drop")
            .version
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if let Some(data) = self.0.take().and_then(Arc::into_inner) {
            drop(data);
        }
    }
}

impl CanonicalCache {
    /// The view canonical reads of `version` read under `same_as`; `None` where that is
    /// `version` itself. Made on `views`, the store's budget for the views versions keep,
    /// and kept for the version; where `views` refuses, made on `fallback` (the reader's
    /// own) and not kept; where that refuses too, the error. Each view holds its charge
    /// ([`View`]), released by its last holder, the cache or a reader, after the view is
    /// freed. Readers that miss at once each make one, each
    /// charged in full; the last one made is kept.
    pub(crate) fn view(
        version: &Arc<Version>,
        same_as: TermId,
        views: &Arc<Budget>,
        fallback: &Arc<Budget>,
    ) -> Result<Option<View>, BudgetExceeded> {
        if let Some((id, view)) = version.canonical.view.lock().as_ref()
            && *id == same_as
        {
            return Ok(view.clone());
        }
        let classes = version.equality.classes(version, same_as);
        if version.asserted.layout() != crate::index::Layout::Quads || classes.aliases.is_empty() {
            *version.canonical.view.lock() = Some((same_as, None));
            return Ok(None);
        }
        match canonical_version(version, same_as, &classes, views) {
            Ok(view) => {
                *version.canonical.view.lock() = Some((same_as, view.clone()));
                Ok(view)
            }
            Err(_) => canonical_version(version, same_as, &classes, fallback),
        }
    }

    /// Whether a triple term of this cache's version names an alias under `same_as`
    /// (`find` decides it), once per version.
    pub(crate) fn triple_terms(&self, same_as: TermId, find: impl FnOnce() -> bool) -> bool {
        if let Some((id, found)) = *self.triple_terms.lock()
            && id == same_as
        {
            return found;
        }
        let found = find();
        *self.triple_terms.lock() = Some((same_as, found));
        found
    }
}

/// The named-graph statements of `version` that name another identity of a class than
/// its representative, in any position (the graph's name included), appended to `out`:
/// seeks per alias, or one scan of the named graphs where that is less.
fn renamed_statements(
    version: &Version,
    classes: &Classes,
    out: &mut ChargedVec<EncodedQuad>,
) -> Result<(), BudgetExceeded> {
    let asserted = &version.asserted;
    let named = QuadPattern {
        subject: None,
        predicate: None,
        object: None,
        graph: GraphSelector::AnyNamed,
    };
    let named_count = asserted.count_plan(&crate::quad::AccessPlan::for_pattern(&named));
    if (classes.aliases.len() as u64).saturating_mul(4) >= named_count {
        for quad in asserted
            .scan(&named)
            .filter(|quad| classes.names_alias(quad))
        {
            out.push(quad)?;
        }
        return Ok(());
    }
    for &alias in classes.aliases.keys() {
        let alias = TermId::from_raw(alias);
        let patterns = [
            QuadPattern {
                subject: Some(alias),
                ..named
            },
            QuadPattern {
                predicate: Some(alias),
                ..named
            },
            QuadPattern {
                object: Some(alias),
                ..named
            },
            QuadPattern {
                graph: GraphSelector::Exact(alias),
                ..named
            },
        ];
        for pattern in &patterns {
            for quad in asserted.scan(pattern) {
                out.push(quad)?;
            }
        }
    }
    Ok(())
}

/// `items`, sorted, each once.
fn dedup_sorted(items: &mut ChargedVec<EncodedQuad>) {
    let mut kept = 0;
    for i in 0..items.len() {
        if kept == 0 || items[i] != items[kept - 1] {
            items[kept] = items[i];
            kept += 1;
        }
    }
    items.truncate(kept);
}

/// `version` as canonical reads read it (work package W4, stage C; canonical answers):
/// its named graphs over representatives, as its default graph's closure is. A named-graph
/// statement that names another identity of a class ([`renamed_statements`]) is replaced
/// by its representatives' statement: one run of tombstones and inserts over the asserted
/// stack, so that every read, whatever its access path, gives representatives. The default
/// graph stays as it is (its canonical reads skip what isn't over representatives).
/// `None` where no statement is renamed.
///
/// Made on `budget`, everything reserved before it is allocated: the statements, the
/// run's arrays, its scans' working memory, the version and its list of runs, and the
/// view's own allocation. The [`View`] holds what it keeps reserved, released after it is
/// freed; a refusal leaves nothing made. O(a log n + k (log k + r log n)) for a aliases, k
/// such statements and r runs, or one scan of the named graphs where that is less.
fn canonical_version(
    version: &Arc<Version>,
    same_as: TermId,
    classes: &Arc<Classes>,
    budget: &Arc<Budget>,
) -> Result<Option<View>, BudgetExceeded> {
    let asserted = &version.asserted;
    let mut scanning = Reservation::new(Arc::clone(budget));
    scanning.charge(asserted.scan_bytes())?;
    let mut renamed: ChargedVec<EncodedQuad> = ChargedVec::new(Arc::clone(budget));
    renamed_statements(version, classes, &mut renamed)?;
    drop(scanning);
    if renamed.is_empty() {
        return Ok(None);
    }
    renamed.sort_unstable();
    dedup_sorted(&mut renamed);
    let mut inserts: ChargedVec<EncodedQuad> =
        ChargedVec::with_capacity(Arc::clone(budget), renamed.len())?;
    for quad in renamed.iter() {
        inserts.push(EncodedQuad::from_components(
            quad.components()
                .map(|id| classes.representative(TermId::from_raw(id)).raw()),
        ))?;
    }
    inserts.sort_unstable();
    dedup_sorted(&mut inserts);
    // An exact delta (`index/run.rs`): a representatives' statement the asserted stack
    // holds already, asserted beside an alias's that stands for it, is not inserted again,
    // or its key would count twice. No insert is a statement tombstoned: those name an alias.
    inserts.retain(|quad| !asserted.contains(quad));
    // What the view keeps besides its run's arrays: its run, the list of runs, the
    // version and its equality cell, and the view itself, each with its allocation's
    // header.
    let mut held = Reservation::new(Arc::clone(budget));
    let boxed = |bytes: usize| bytes + 2 * std::mem::size_of::<usize>();
    held.charge(
        boxed(std::mem::size_of::<crate::index::run::Run>())
            + asserted.run_list_bytes()
            + boxed(std::mem::size_of::<Version>())
            + boxed(std::mem::size_of::<EqualityCell>())
            + boxed(std::mem::size_of::<ViewData>()),
    )?;
    let run = crate::index::run::Run::from_delta_within(
        asserted.layout(),
        &inserts,
        &renamed,
        &mut held,
    )?;
    let asserted = asserted.with_run(run);
    drop((inserts, renamed));
    let version = Arc::new(Version {
        asserted,
        inferred: version.inferred.clone(),
        revision: version.revision,
        committed_micros: version.committed_micros,
        dictionary_len: version.dictionary_len,
        equality: Arc::new(EqualityCell::holding(same_as, Arc::clone(classes))),
        content: version.content,
        // Its asserted stack renamed to representatives: not the stored ids.
        asserted_content: super::AssertedIdentity::fresh(),
        canonical: Default::default(),
        own: Default::default(),
    });
    Ok(Some(View(Some(Arc::new(ViewData {
        version,
        _held: held,
    })))))
}

/// `owl:sameAs` classes of two or more identities.
#[derive(Debug, Default)]
pub struct Classes {
    /// Every identity of a class other than its representative, with the representative.
    aliases: HashMap<u64, u64>,
    /// Each representative's identities, sorted (the representative first).
    members: HashMap<u64, Box<[u64]>>,
    /// Some identity is a literal ([`Self::relate_literals`]).
    literals: bool,
}

impl Classes {
    /// The classes the `sameAs` statements of `version`'s default graph make, the build's
    /// work counted on `meter` (each statement its scan passes, each identity it copies);
    /// `None` where it stopped (REAS-3, sweep SW012).
    fn of_with<M: Metering>(version: &Version, same_as: TermId, meter: M) -> Option<Self> {
        let pattern = QuadPattern {
            subject: None,
            predicate: Some(same_as),
            object: None,
            graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
        };
        // The reasoner's union kernel (G7 of the investigation of 6 October 2026).
        let mut union = nrese_exec::classes::Classes::default();
        for stack in Stack::ALL {
            for quad in meter.scan(version.stack(stack), &pattern) {
                let (a, b) = (quad.subject.raw(), quad.object.raw());
                if a == b {
                    continue;
                }
                // A union moves the smaller side's members: counted before it is made
                // (REAS-3a sol-loop r1 S1). One union is one step, done whole once counted. A
                // pair already in one class (one member list) moves nothing and counts one
                // unit (sol-loop r2 S6: an inferred star over an asserted chain).
                if M::COUNTED {
                    let (class_a, class_b) = (union.class_of(a), union.class_of(b));
                    let moved = match (class_a, class_b) {
                        (Some(x), Some(y)) if std::ptr::eq(x, y) => 1,
                        _ => {
                            let size = |class: Option<&[u64]>| class.map_or(1, <[u64]>::len);
                            size(class_a).min(size(class_b)) as u64
                        }
                    };
                    if !meter.units(moved) {
                        return None;
                    }
                }
                union.union(a, b);
            }
        }
        if meter.stopped() {
            return None;
        }
        let mut copied = true;
        let members = union.classes().map_while(|(representative, members)| {
            copied = !M::COUNTED || meter.units(members.len() as u64);
            copied.then(|| (representative, members.to_vec()))
        });
        let mut classes = Self::default();
        for (_, mut identities) in members {
            if identities.len() < 2 {
                continue;
            }
            identities.sort_unstable();
            classes.literals |= identities
                .iter()
                .any(|&id| is_literal(TermId::from_raw(id)));
            // The smallest id represents the class, as the reasoner chooses.
            let representative = identities[0];
            for &identity in &identities[1..] {
                classes.aliases.insert(identity, representative);
            }
            classes
                .members
                .insert(representative, identities.into_boxed_slice());
        }
        copied.then_some(classes)
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty() && self.aliases.is_empty()
    }

    /// These classes for canonical reads (stage C): every alias still maps to its
    /// representative (constants are normalised, an alias's statements are left to its
    /// representative's copy), but no representative expands, so each stored statement is
    /// read once.
    pub(crate) fn canonical_view(&self) -> Self {
        Self {
            aliases: self.aliases.clone(),
            members: HashMap::new(),
            literals: self.literals,
        }
    }

    /// Whether a literal is an identity of some class (`ex:a owl:sameAs "a"`). Then a
    /// value an expression computes may name a class, which canonical answers can't map
    /// (`nrese_sparql`'s canonical answers refuse such classes).
    pub fn relate_literals(&self) -> bool {
        self.literals
    }

    /// Whether a term of `quad`, its graph included, is an identity of a class other than
    /// its representative.
    fn names_alias(&self, quad: &EncodedQuad) -> bool {
        quad.components().iter().any(|&id| self.is_alias(id))
    }

    /// The identities `id` stands for once expanded: its class's if it represents one,
    /// else itself.
    pub fn members_of<'a>(&'a self, id: &'a u64) -> &'a [u64] {
        match self.members.get(id) {
            Some(members) => members,
            None => std::slice::from_ref(id),
        }
    }

    /// The number of classes of two or more identities.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Each class of two or more identities: its representative and its identities,
    /// sorted (the representative first).
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u64])> {
        self.members.iter().map(|(&r, m)| (r, &m[..]))
    }

    /// The representative of `id` (`id` itself outside every class).
    pub fn representative(&self, id: TermId) -> TermId {
        self.aliases
            .get(&id.raw())
            .map_or(id, |&r| TermId::from_raw(r))
    }

    /// The identities of `id`'s class, sorted, if it has two or more.
    pub fn class_of(&self, id: TermId) -> Option<&[u64]> {
        self.members
            .get(&self.representative(id).raw())
            .map(|m| &m[..])
    }

    /// Whether `id` is an identity of a class other than its representative.
    pub(crate) fn is_alias(&self, id: u64) -> bool {
        self.aliases.contains_key(&id)
    }

    /// The identities a stored representative `id` stands for (`None`: itself only).
    fn expansion(&self, id: u64) -> Option<&[u64]> {
        self.members.get(&id).map(|m| &m[..])
    }

    /// `pattern` with its constants replaced by their representatives.
    pub(crate) fn normalised(&self, pattern: &QuadPattern) -> QuadPattern {
        QuadPattern {
            subject: pattern.subject.map(|t| self.representative(t)),
            predicate: pattern.predicate.map(|t| self.representative(t)),
            object: pattern.object.map(|t| self.representative(t)),
            graph: pattern.graph,
        }
    }

    /// `pattern` with its constants replaced by their representatives, the graph's name
    /// included: for canonical reads, whose every graph is over representatives.
    pub(crate) fn normalised_everywhere(&self, pattern: &QuadPattern) -> QuadPattern {
        QuadPattern {
            graph: match pattern.graph {
                GraphSelector::Exact(graph) => GraphSelector::Exact(self.representative(graph)),
                other => other,
            },
            ..self.normalised(pattern)
        }
    }

    /// Whether some term of `quad` has other identities.
    pub(crate) fn touches(&self, quad: &EncodedQuad) -> bool {
        [quad.subject, quad.predicate, quad.object]
            .iter()
            .any(|term| self.members.contains_key(&term.raw()))
    }

    /// Whether a stored default-graph quad is over representatives (else a copy of it is
    /// stored over representatives, or it places an identity in its class).
    pub(crate) fn is_canonical(&self, quad: &EncodedQuad) -> bool {
        !(self.is_alias(quad.subject.raw())
            || self.is_alias(quad.predicate.raw())
            || self.is_alias(quad.object.raw()))
    }

    /// How many quads a canonical stored quad stands for, given the pattern's constants
    /// (a constant stands for itself); `None` for more than a `u64` holds, never a wrapped
    /// or capped number (PR #70 fix checks 1 F2 and 2 F3: a class of 2^22 identities).
    pub(crate) fn multiplicity(&self, quad: &EncodedQuad, pattern: &QuadPattern) -> Option<u64> {
        let size = |bound: Option<TermId>, id: TermId| match bound {
            Some(_) => 1,
            None => self.expansion(id.raw()).map_or(1, |m| m.len() as u64),
        };
        size(pattern.subject, quad.subject)
            .checked_mul(size(pattern.predicate, quad.predicate))?
            .checked_mul(size(pattern.object, quad.object))
    }

    /// How many copies [`Self::expand`] gives `quad` for `pattern`: the product of its
    /// unbound components' class sizes.
    pub(crate) fn copies(&self, quad: &EncodedQuad, pattern: &QuadPattern) -> usize {
        let count = |bound: Option<TermId>, id: TermId| match bound {
            Some(_) => 1,
            None => self.expansion(id.raw()).map_or(1, <[u64]>::len),
        };
        count(pattern.subject, quad.subject)
            .saturating_mul(count(pattern.predicate, quad.predicate))
            .saturating_mul(count(pattern.object, quad.object))
    }

    /// The quads a canonical stored quad stands for, the pattern's constants in place, until
    /// `out` breaks (an existence question has its answer at the first one it counts).
    pub(crate) fn expand(
        &self,
        quad: &EncodedQuad,
        pattern: &QuadPattern,
        out: &mut impl FnMut(EncodedQuad) -> std::ops::ControlFlow<()>,
    ) {
        let values = |bound: Option<TermId>, id: TermId| -> Values<'_> {
            match bound {
                Some(constant) => Values::One(constant.raw()),
                None => match self.expansion(id.raw()) {
                    Some(members) => Values::Many(members),
                    None => Values::One(id.raw()),
                },
            }
        };
        let (s, p, o) = (
            values(pattern.subject, quad.subject),
            values(pattern.predicate, quad.predicate),
            values(pattern.object, quad.object),
        );
        let graph = quad.graph.raw();
        for &s in s.as_slice() {
            for &p in p.as_slice() {
                for &o in o.as_slice() {
                    if out(EncodedQuad::from_components([s, p, o, graph])).is_break() {
                        return;
                    }
                }
            }
        }
    }
}

/// Whether `id` is a literal: any kind but IRIs, blank nodes and triple terms.
fn is_literal(id: TermId) -> bool {
    id.kind().is_literal()
}

enum Values<'a> {
    One(u64),
    Many(&'a [u64]),
}

impl Values<'_> {
    fn as_slice(&self) -> &[u64] {
        match self {
            Values::One(value) => std::slice::from_ref(value),
            Values::Many(values) => values,
        }
    }
}

/// How an equality read counts its work ([`crate::visit`]): not at all ([`Unmetered`], what
/// every uncounted read compiles to, its calls inlined to nothing and its scans the plain
/// ones), or on a counted read's visitor. Its scans and probes are the index's counted twins
/// there; once the visitor has stopped they end early and the read is the visitor's to fail
/// (REAS-3, sweeps SW012–SW015).
pub(crate) trait Metering: Copy {
    /// Whether it counts anything (resolved when compiled).
    const COUNTED: bool;
    /// Its scans of an index.
    type Scan<'a>: Iterator<Item = EncodedQuad>
    where
        Self: 'a;
    /// Counts `n` units before they are done: false once stopped.
    fn units(self, n: u64) -> bool;
    /// Whether it has stopped, counting nothing.
    fn stopped(self) -> bool;
    /// A scan of `index` for `pattern`.
    fn scan<'a>(
        self,
        index: &'a crate::index::IndexVersion,
        pattern: &QuadPattern,
    ) -> Self::Scan<'a>
    where
        Self: 'a;
    /// Whether `index` holds `quad`; false once stopped.
    fn contains(self, index: &crate::index::IndexVersion, quad: &EncodedQuad) -> bool;
    /// `index`'s held count for `plan`; `None` once stopped.
    fn count_plan(
        self,
        index: &crate::index::IndexVersion,
        plan: &crate::quad::AccessPlan,
    ) -> Option<u64>;
}

/// A meter's count taken a unit at a time and asked for a run at a time: one call of the
/// visitor per run of at most `POLL_UNITS` units, a decrement per unit (REAS-2's r8 form:
/// a call per unit cost +0.6 % on replicate, the run form noise). Nothing for an unmetered
/// read.
#[derive(Default)]
pub(crate) struct Credit(u64);

impl Credit {
    /// Takes one unit, asking `meter` for the next run when none is left: `left` units at
    /// most (the ones still to come, this one included), and at most `POLL_UNITS`; false
    /// once the meter has stopped.
    #[inline]
    pub(crate) fn take<M: Metering>(&mut self, meter: M, left: u64) -> bool {
        if !M::COUNTED {
            return true;
        }
        if self.0 == 0 {
            let run = left.clamp(1, u64::from(nrese_exec::POLL_UNITS));
            if !meter.units(run) {
                return false;
            }
            self.0 = run;
        }
        self.0 -= 1;
        true
    }
}

/// Counting nothing: the plain reads.
#[derive(Clone, Copy)]
pub(crate) struct Unmetered;

impl Metering for Unmetered {
    const COUNTED: bool = false;
    type Scan<'a> = crate::index::QuadScan<'a>;

    #[inline(always)]
    fn units(self, _: u64) -> bool {
        true
    }

    #[inline(always)]
    fn stopped(self) -> bool {
        false
    }

    #[inline(always)]
    fn scan<'a>(
        self,
        index: &'a crate::index::IndexVersion,
        pattern: &QuadPattern,
    ) -> Self::Scan<'a>
    where
        Self: 'a,
    {
        index.scan(pattern)
    }

    #[inline(always)]
    fn contains(self, index: &crate::index::IndexVersion, quad: &EncodedQuad) -> bool {
        index.contains(quad)
    }

    #[inline(always)]
    fn count_plan(
        self,
        index: &crate::index::IndexVersion,
        plan: &crate::quad::AccessPlan,
    ) -> Option<u64> {
        Some(index.count_plan(plan))
    }
}

impl<'v> Metering for crate::visit::Visitor<'v> {
    const COUNTED: bool = true;
    type Scan<'a>
        = crate::index::CountedScan<'a, 'v>
    where
        Self: 'a;

    #[inline]
    fn units(self, n: u64) -> bool {
        crate::visit::units(self, n)
    }

    fn stopped(self) -> bool {
        crate::visit::stopped(self)
    }

    fn scan<'a>(
        self,
        index: &'a crate::index::IndexVersion,
        pattern: &QuadPattern,
    ) -> Self::Scan<'a>
    where
        Self: 'a,
    {
        index.scan_counted(pattern, self)
    }

    fn contains(self, index: &crate::index::IndexVersion, quad: &EncodedQuad) -> bool {
        index.contains_counted(quad, self).unwrap_or(false)
    }

    fn count_plan(
        self,
        index: &crate::index::IndexVersion,
        plan: &crate::quad::AccessPlan,
    ) -> Option<u64> {
        index.count_plan_visiting(plan, self)
    }
}

/// Which copies of a stored default-graph quad a read shows: in the inferred model none
/// that is asserted; in either model none asserted only in named graphs (a statement
/// asserted somewhere is never inferred).
#[derive(Clone, Copy)]
pub(crate) struct Visibility<'a> {
    version: &'a Version,
    inferred_only: bool,
    /// The asserted stack holds named graphs.
    named_graphs: bool,
}

impl<'a> Visibility<'a> {
    pub(crate) fn new(version: &'a Version, model: ReadModel) -> Self {
        Self {
            version,
            inferred_only: model == ReadModel::Inferred,
            named_graphs: version.asserted.layout() == crate::index::Layout::Quads,
        }
    }

    fn asserted_anywhere<M: Metering>(&self, copy: &EncodedQuad, meter: M) -> bool {
        if !self.named_graphs {
            return meter.contains(&self.version.asserted, copy);
        }
        let pattern = QuadPattern {
            subject: Some(copy.subject),
            predicate: Some(copy.predicate),
            object: Some(copy.object),
            graph: GraphSelector::Any,
        };
        meter
            .scan(&self.version.asserted, &pattern)
            .next()
            .is_some()
    }

    /// Whether `copy` (in the default graph), a copy of a stored quad that `touching`
    /// classes or not, is shown.
    pub(crate) fn shows(&self, copy: &EncodedQuad, touching: bool) -> bool {
        self.shows_with(copy, touching, Unmetered)
    }

    /// [`Self::shows`], its probes counted on `meter` (REAS-3, sweep SW015): once stopped,
    /// an answer the stopped read discards.
    pub(crate) fn shows_with<M: Metering>(
        &self,
        copy: &EncodedQuad,
        touching: bool,
        meter: M,
    ) -> bool {
        if self.inferred_only {
            return !self.asserted_anywhere(copy, meter);
        }
        // Without classes the stored quad is the only copy: asserted in the default graph,
        // or inferred (never asserted anywhere).
        if !touching || !self.named_graphs {
            return true;
        }
        meter.contains(&self.version.asserted, copy) || !self.asserted_anywhere(copy, meter)
    }

    /// Whether every copy of a stored quad is shown without asking each.
    pub(crate) fn shows_all(&self, touching: bool) -> bool {
        !self.inferred_only && (!touching || !self.named_graphs)
    }
}

/// A sorted scan of stored quads read with expansion: default-graph quads over
/// representatives expanded, others skipped; quads of named graphs passed on when
/// `named` (they match the pattern as given); copies shown as [`Visibility`] says. Sorted
/// by `permutation` as its input is.
///
/// The copies wait in a heap until no later input can sort before them. That is no bound
/// of one quad's copies: under a class of n identities in the permutation's first place,
/// the copies of every quad over its representative wait for the last of them. So the
/// heap's growth is charged to the reader's budget before it grows, the old allocation
/// beside the new while the copies move, and released once freed ([`Reads`]); a growth
/// the budget refuses ends the scan, the refusal kept for the reader.
///
/// A counted read's expansion ([`Metering`]) counts each copy it makes before it makes it
/// and each copy it hands on, and its visibility probes; its input is a counted scan. Once
/// the meter has stopped it hands on nothing more (REAS-3, sweeps SW013, SW014).
pub(crate) struct Expand<'a, I: Iterator<Item = EncodedQuad>, M: Metering = Unmetered> {
    input: std::iter::Peekable<I>,
    classes: Arc<Classes>,
    pattern: QuadPattern,
    permutation: Permutation,
    named: bool,
    visibility: Visibility<'a>,
    pending: BinaryHeap<Reverse<Key>>,
    /// The reader's budget, and what the heap's capacity is charged to it.
    reads: Option<super::snapshot::ReadBudget>,
    held: usize,
    refused: bool,
    meter: M,
    /// The units the meter has counted ahead and not yet used ([`Credit`]).
    credit: Credit,
}

impl<'a, I: Iterator<Item = EncodedQuad>> Expand<'a, I> {
    pub(crate) fn new(
        input: I,
        classes: Arc<Classes>,
        pattern: QuadPattern,
        permutation: Permutation,
        named: bool,
        model: ReadModel,
        version: &'a Version,
    ) -> Self {
        Expand::metered(
            input,
            classes,
            pattern,
            permutation,
            named,
            model,
            version,
            Unmetered,
        )
    }
}

impl<'a, I: Iterator<Item = EncodedQuad>, M: Metering> Expand<'a, I, M> {
    /// [`Expand::new`], its work counted on `meter`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn metered(
        input: I,
        classes: Arc<Classes>,
        pattern: QuadPattern,
        permutation: Permutation,
        named: bool,
        model: ReadModel,
        version: &'a Version,
        meter: M,
    ) -> Self {
        Self {
            meter,
            credit: Credit::default(),
            input: input.peekable(),
            classes,
            pattern,
            permutation,
            named,
            visibility: Visibility::new(version, model),
            pending: BinaryHeap::new(),
            reads: None,
            held: 0,
            refused: false,
        }
    }

    /// This scan charging its waiting copies to `reads`, the reader's budget, if any.
    pub(crate) fn charging(mut self, reads: Option<super::snapshot::ReadBudget>) -> Self {
        self.reads = reads;
        self
    }

    /// Room in the heap for `more` copies, its growth charged first; false where the
    /// reader's budget refuses it.
    fn room(&mut self, more: usize) -> bool {
        let needed = self.pending.len().saturating_add(more);
        if needed <= self.pending.capacity() {
            return true;
        }
        let Some(reads) = &self.reads else {
            self.pending.reserve(more);
            return true;
        };
        let size = size_of::<Reverse<Key>>();
        let capacity = needed.max(self.pending.capacity().saturating_mul(2));
        if !reads.charge(capacity.saturating_mul(size)) {
            return false;
        }
        self.pending.reserve_exact(capacity - self.pending.len());
        // The heap may be given more than asked: charged too, or the scan refused (what was
        // charged held, released once the heap is freed).
        let got = self.pending.capacity().saturating_mul(size);
        let asked = capacity.saturating_mul(size);
        if got > asked && !reads.charge(got - asked) {
            self.held = self.held.saturating_add(asked);
            return false;
        }
        reads.release(self.held);
        self.held = got;
        true
    }

    /// The key the quad `raw` would have in the answer at the least: its own values with
    /// the pattern's constants in place.
    fn least_key(pattern: &QuadPattern, permutation: Permutation, raw: &EncodedQuad) -> Key {
        let quad = EncodedQuad::from_components([
            pattern.subject.unwrap_or(raw.subject).raw(),
            pattern.predicate.unwrap_or(raw.predicate).raw(),
            pattern.object.unwrap_or(raw.object).raw(),
            raw.graph.raw(),
        ]);
        permutation.to_key(&quad)
    }

    /// Moves the next input quad's copies into the heap; `false` at the end of the input.
    fn pull(&mut self) -> bool {
        let Some(raw) = self.input.next() else {
            return false;
        };
        if !raw.graph.is_default_graph() {
            if self.named {
                if !self.room(1) {
                    return self.refuse();
                }
                self.pending.push(Reverse(self.permutation.to_key(&raw)));
            }
            return true;
        }
        if !self.classes.is_canonical(&raw) {
            return true;
        }
        let copies = self.classes.copies(&raw, &self.pattern);
        if !self.room(copies) {
            return self.refuse();
        }
        let (permutation, visibility, meter) = (self.permutation, self.visibility, self.meter);
        let touching = self.classes.touches(&raw);
        let all = visibility.shows_all(touching);
        let (pending, credit) = (&mut self.pending, &mut self.credit);
        let mut stopped = false;
        let mut left = copies as u64;
        self.classes.expand(&raw, &self.pattern, &mut |quad| {
            // Each copy a unit, counted a run at a time before it is made (SW013).
            if !credit.take(meter, left) {
                stopped = true;
                return std::ops::ControlFlow::Break(());
            }
            left = left.saturating_sub(1);
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::ExpandCopies, 1);
            if all || visibility.shows_with(&quad, touching, meter) {
                pending.push(Reverse(permutation.to_key(&quad)));
            }
            std::ops::ControlFlow::Continue(())
        });
        !(M::COUNTED && stopped)
    }
}

impl<I: Iterator<Item = EncodedQuad>, M: Metering> Expand<'_, I, M> {
    /// Ends the scan after a refused growth: nothing more is given (the refusal is the
    /// reader's to see), the heap freed before its charge is released.
    fn refuse(&mut self) -> bool {
        self.refused = true;
        self.pending = BinaryHeap::new();
        if let Some(reads) = &self.reads {
            reads.release(std::mem::take(&mut self.held));
        }
        false
    }
}

impl<I: Iterator<Item = EncodedQuad>, M: Metering> Drop for Expand<'_, I, M> {
    /// The heap freed, then its charge released.
    fn drop(&mut self) {
        self.pending = BinaryHeap::new();
        if let Some(reads) = &self.reads {
            reads.release(std::mem::take(&mut self.held));
        }
    }
}

impl<I: Iterator<Item = EncodedQuad>, M: Metering> Iterator for Expand<'_, I, M> {
    type Item = EncodedQuad;

    fn next(&mut self) -> Option<EncodedQuad> {
        if self.refused {
            return None;
        }
        loop {
            // A counted read's step, a copy handed on or an input quad pulled: a unit, and
            // the end once its meter has stopped (the heap's rest is never walked).
            let left = self.pending.len() as u64 + 1;
            if !self.credit.take(self.meter, left) {
                return None;
            }
            // What the heap holds may go out once no later input can sort before it.
            if let Some(Reverse(first)) = self.pending.peek() {
                let (pattern, permutation) = (&self.pattern, self.permutation);
                let safe = match self.input.peek() {
                    None => true,
                    Some(next) => *first <= Self::least_key(pattern, permutation, next),
                };
                if safe {
                    let Reverse(key) = self.pending.pop().expect("peeked");
                    return Some(self.permutation.key_to_quad(&key));
                }
            }
            if !self.pull() && (self.refused || self.pending.is_empty()) {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(s: u64, p: u64, o: u64) -> EncodedQuad {
        EncodedQuad::from_components([s, p, o, TermId::DEFAULT_GRAPH.raw()])
    }

    fn classes(groups: &[&[u64]]) -> Classes {
        let mut classes = Classes::default();
        for group in groups {
            for &alias in &group[1..] {
                classes.aliases.insert(alias, group[0]);
            }
            classes
                .members
                .insert(group[0], group.to_vec().into_boxed_slice());
        }
        classes
    }

    #[test]
    fn expansion_keeps_the_scan_order() {
        let classes = Arc::new(classes(&[&[10, 30], &[20, 25, 40]]));
        // Stored over representatives, in SPOG order; `30 p 1` is an alias's fact.
        let stored = vec![
            quad(10, 5, 20),
            quad(10, 6, 1),
            quad(20, 5, 10),
            quad(30, 5, 1),
        ];
        let version = Version::empty();
        let expanded: Vec<EncodedQuad> = Expand::new(
            stored.into_iter(),
            Arc::clone(&classes),
            QuadPattern::all(),
            Permutation::Spog,
            true,
            ReadModel::Materialised,
            &version,
        )
        .collect();
        let keys: Vec<Key> = expanded
            .iter()
            .map(|q| Permutation::Spog.to_key(q))
            .collect();
        assert!(keys.is_sorted(), "{keys:?}");
        // 10 5 20: 2 × 3 copies; 10 6 1: 2; 20 5 10: 3 × 2; the alias fact: none.
        assert_eq!(expanded.len(), 6 + 2 + 6);
        // A constant stands for itself.
        let pattern = QuadPattern {
            subject: Some(TermId::from_raw(30)),
            ..QuadPattern::all()
        };
        let stored = vec![quad(10, 5, 20), quad(10, 6, 1)];
        let normalised = classes.normalised(&pattern);
        assert_eq!(normalised.subject, Some(TermId::from_raw(10)));
        let expanded: Vec<EncodedQuad> = Expand::new(
            stored.into_iter(),
            Arc::clone(&classes),
            pattern,
            Permutation::Spog,
            true,
            ReadModel::Materialised,
            &version,
        )
        .collect();
        assert_eq!(expanded.len(), 3 + 1);
        assert!(expanded.iter().all(|q| q.subject.raw() == 30));
        assert_eq!(classes.multiplicity(&quad(10, 5, 20), &pattern), Some(3));
    }

    /// One class of 2^22 identities in all three places stands for 2^66 quads, more than a
    /// `u64` holds: the multiplicity says so, neither wrapped (it wrapped to 0, so a snapshot
    /// holding such a class read as empty: PR #70 fix check 1 F2) nor capped (a capped count
    /// was published as an exact `COUNT`: fix check 2 F3).
    #[test]
    fn multiplicity_past_u64_is_none() {
        let r = 1;
        let mut classes = Classes::default();
        classes.members.insert(r, (r..r + (1 << 22)).collect());
        let all = QuadPattern::all();
        assert_eq!(classes.multiplicity(&quad(r, r, r), &all), None);
        let bound = QuadPattern {
            subject: Some(TermId::from_raw(r)),
            ..all
        };
        assert_eq!(classes.multiplicity(&quad(r, r, r), &bound), Some(1 << 44));
    }
}
