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
use crate::quad::{AccessPlan, EncodedQuad, GraphSelector, Key, Permutation, QuadPattern};
use crate::term::TermId;
use crate::visit::{Account, Free, Stopped};

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
        // One account for all of the build's work: its scans' searches and steps, its unions'
        // moves and its finish, so at most `POLL_UNITS` units of it follow any yes (fix check
        // r2 G1: a scan's ledger and the unions' credit apart allowed both).
        let mut account = OnMeter::new(meter);
        let plan = AccessPlan::for_pattern(&pattern);
        for stack in Stack::ALL {
            let index = version.stack(stack);
            let mut scan = index.scan_plan_with(&plan, &mut account).ok()?;
            while let Some(quad) = scan.step_with(&mut account).ok()? {
                let (a, b) = (quad.subject.raw(), quad.object.raw());
                if a == b {
                    continue;
                }
                // A union moves the smaller side's members, each counted before it moves, a
                // chunk at a time (REAS-3a sol-loop r1 S1, fix check r1 R1); a pair already in
                // one class moves none (sol-loop r2 S6). A stopped union leaves the kernel half
                // made: dropped with this build.
                union.union_asking(a, b, |n| account.take(n))?;
            }
        }
        // Finishing the classes (REAS-3a review r1 R1, fix check r1 R1): each class the kernel
        // lists, and each identity copied out of it, sorted, checked for a literal and indexed,
        // is counted before it is handled, a chunk of at most `POLL_UNITS` units at a time, so
        // a stop is seen within a run of work however large a class is (a counted sort:
        // `sort_counted`). The maps are made at their final size before the first insert,
        // counted as the identities they will hold, so no insert grows them (their
        // initialisation is held for the owner: fix check r1 F2).
        let chunk = nrese_exec::POLL_UNITS as usize;
        let mut listed = union.classes();
        let mut copies: Vec<Vec<u64>> = Vec::with_capacity(listed.len());
        let mut identities_in_all = 0;
        loop {
            if !account.take(1) {
                return None;
            }
            let Some((_, members)) = listed.next() else {
                break;
            };
            if members.len() < 2 {
                continue;
            }
            let mut identities = Vec::with_capacity(members.len());
            for part in members.chunks(chunk) {
                if !account.take(part.len() as u64) {
                    return None;
                }
                #[cfg(any(test, all(feature = "witness", debug_assertions)))]
                crate::witness::add(crate::witness::Site::ClassFinish, part.len() as u64);
                identities.extend_from_slice(part);
            }
            identities_in_all += identities.len();
            copies.push(identities);
        }
        if !account.take(identities_in_all as u64) {
            return None;
        }
        let mut classes = Self {
            aliases: HashMap::with_capacity(identities_in_all - copies.len()),
            members: HashMap::with_capacity(copies.len()),
            literals: false,
        };
        for mut identities in copies {
            if !M::COUNTED {
                identities.sort_unstable();
            } else if !sort_counted(&mut identities, |n| account.take(n)) {
                return None;
            }
            // The smallest id represents the class, as the reasoner chooses.
            let representative = identities[0];
            classes.literals |= is_literal(TermId::from_raw(representative));
            for part in identities[1..].chunks(chunk) {
                if !account.take(part.len() as u64) {
                    return None;
                }
                #[cfg(any(test, all(feature = "witness", debug_assertions)))]
                crate::witness::add(crate::witness::Site::ClassFinish, part.len() as u64);
                for &identity in part {
                    classes.literals |= is_literal(TermId::from_raw(identity));
                    classes.aliases.insert(identity, representative);
                }
            }
            classes
                .members
                .insert(representative, identities.into_boxed_slice());
        }
        Some(classes)
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
    /// Counts `n` units before they are done: false once stopped.
    fn units(self, n: u64) -> bool;
    /// Whether it has stopped, counting nothing.
    fn stopped(self) -> bool;
    /// `index`'s held count for `plan`; `None` once stopped.
    fn count_plan(
        self,
        index: &crate::index::IndexVersion,
        plan: &crate::quad::AccessPlan,
    ) -> Option<u64>;
}

/// Sorts `ids` (distinct identities), `take(n)` called before each `n` units of its work, `n`
/// at most `POLL_UNITS` (REAS-3a fix check r1 R1): runs of [`SORT_RUN`] ids each sorted alone,
/// counted as `len · (⌊log2 len⌋ + 1)` comparisons before it, then merged pairwise into a
/// second list, each id a merge moves a unit, counted a chunk at a time. False once `take`
/// answers false: `ids` is then in no particular order.
fn sort_counted(ids: &mut Vec<u64>, mut take: impl FnMut(u64) -> bool) -> bool {
    let comparisons = |len: usize| (len * (usize::BITS - len.leading_zeros()) as usize) as u64;
    for run in ids.chunks_mut(SORT_RUN) {
        if !take(comparisons(run.len())) {
            return false;
        }
        #[cfg(not(any(test, all(feature = "witness", debug_assertions))))]
        run.sort_unstable();
        #[cfg(any(test, all(feature = "witness", debug_assertions)))]
        run.sort_unstable_by(|a, b| {
            crate::witness::add(crate::witness::Site::SortWork, 1);
            a.cmp(b)
        });
    }
    let len = ids.len();
    if len <= SORT_RUN {
        return true;
    }
    let chunk = nrese_exec::POLL_UNITS as usize;
    let mut from = std::mem::take(ids);
    let mut into: Vec<u64> = Vec::with_capacity(len);
    let mut width = SORT_RUN;
    while width < len {
        into.clear();
        for lo in (0..len).step_by(2 * width) {
            let (mid, hi) = ((lo + width).min(len), (lo + 2 * width).min(len));
            let (mut i, mut j) = (lo, mid);
            while i < mid || j < hi {
                let part = chunk.min(mid - i + hi - j);
                if !take(part as u64) {
                    *ids = from;
                    return false;
                }
                #[cfg(any(test, all(feature = "witness", debug_assertions)))]
                crate::witness::add(crate::witness::Site::SortWork, part as u64);
                for _ in 0..part {
                    if j == hi || (i < mid && from[i] < from[j]) {
                        into.push(from[i]);
                        i += 1;
                    } else {
                        into.push(from[j]);
                        j += 1;
                    }
                }
            }
        }
        std::mem::swap(&mut from, &mut into);
        width *= 2;
    }
    *ids = from;
    true
}

/// The ids [`sort_counted`] sorts alone before merging: their sort is at most about
/// `SORT_RUN · log2 SORT_RUN` comparisons, a fraction of `POLL_UNITS`.
const SORT_RUN: usize = 256;

/// A meter's count taken a portion at a time and asked for a run at a time: one call of the
/// visitor per run of at most `POLL_UNITS` units, a decrement per portion (REAS-2's r8 form:
/// a call per unit cost +0.6 % on replicate, the run form noise). Nothing for an unmetered
/// read.
#[derive(Default)]
struct Credit(u64);

impl Credit {
    /// Takes `n` units at once (a chunk of units, or a step that can't be split), asking
    /// `meter` for a run when fewer are left: `POLL_UNITS`, or `n` where that is more (counted
    /// in runs of at most `POLL_UNITS`, the step done whole once counted); what was left of the
    /// last run is dropped, so between two asks there is at most one run of work. False once
    /// the meter has stopped.
    #[inline]
    pub(crate) fn take_many<M: Metering>(&mut self, meter: M, n: u64) -> bool {
        if !M::COUNTED {
            return true;
        }
        if self.0 < n {
            let run = n.max(u64::from(nrese_exec::POLL_UNITS));
            if !meter.units(run) {
                return false;
            }
            self.0 = run;
        }
        self.0 -= n;
        true
    }
}

/// A [`Credit`] on its meter as an [`Account`]: the one account of an equality read for all
/// its work, its scans' steps, its probes and its own (fix check r2 G1 and its siblings: a
/// class build, an expanded count, an expansion; [`Read`]). Its portions may be up to
/// `POLL_UNITS` (a union's chunk, a sort's run), each taken whole, after a yes at most
/// `POLL_UNITS` of them ([`Credit::take_many`]). Nothing for an unmetered read.
pub(crate) struct OnMeter<M: Metering> {
    credit: Credit,
    meter: M,
}

impl<M: Metering> OnMeter<M> {
    /// An account on `meter` with nothing asked for yet.
    pub(crate) fn new(meter: M) -> Self {
        Self {
            credit: Credit::default(),
            meter,
        }
    }
}

impl<M: Metering> Account for OnMeter<M> {
    const COUNTED: bool = M::COUNTED;

    #[inline(always)]
    fn take(&mut self, n: u64) -> bool {
        self.credit.take_many(self.meter, n)
    }
}

/// Counting nothing: the plain reads.
#[derive(Clone, Copy)]
pub(crate) struct Unmetered;

impl Metering for Unmetered {
    const COUNTED: bool = false;

    #[inline(always)]
    fn units(self, _: u64) -> bool {
        true
    }

    #[inline(always)]
    fn stopped(self) -> bool {
        false
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

impl Metering for crate::visit::Visitor<'_> {
    const COUNTED: bool = true;

    #[inline]
    fn units(self, n: u64) -> bool {
        crate::visit::units(self, n)
    }

    fn stopped(self) -> bool {
        crate::visit::stopped(self)
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

    /// Whether the asserted stack holds `copy`, counted on `account` where it counts.
    fn asserted_holds<A: Account>(
        &self,
        copy: &EncodedQuad,
        account: &mut A,
    ) -> Result<bool, Stopped> {
        match A::COUNTED {
            true => self.version.asserted.contains_with(copy, account),
            false => Ok(self.version.asserted.contains(copy)),
        }
    }

    fn asserted_anywhere<A: Account>(
        &self,
        copy: &EncodedQuad,
        account: &mut A,
    ) -> Result<bool, Stopped> {
        if !self.named_graphs {
            return self.asserted_holds(copy, account);
        }
        let pattern = QuadPattern {
            subject: Some(copy.subject),
            predicate: Some(copy.predicate),
            object: Some(copy.object),
            graph: GraphSelector::Any,
        };
        let plan = AccessPlan::for_pattern(&pattern);
        let mut scan = self.version.asserted.scan_plan_with(&plan, account)?;
        Ok(scan.step_with(account)?.is_some())
    }

    /// Whether `copy` (in the default graph), a copy of a stored quad that `touching`
    /// classes or not, is shown.
    pub(crate) fn shows(&self, copy: &EncodedQuad, touching: bool) -> bool {
        self.shows_with(copy, touching, &mut Free) == Ok(true)
    }

    /// [`Self::shows`], its probes counted on `account`, the read's one account (REAS-3,
    /// sweep SW015; fix check r2 G1's siblings): `Err` once it has stopped.
    pub(crate) fn shows_with<A: Account>(
        &self,
        copy: &EncodedQuad,
        touching: bool,
        account: &mut A,
    ) -> Result<bool, Stopped> {
        if self.inferred_only {
            return Ok(!self.asserted_anywhere(copy, account)?);
        }
        // Without classes the stored quad is the only copy: asserted in the default graph,
        // or inferred (never asserted anywhere).
        if !touching || !self.named_graphs {
            return Ok(true);
        }
        Ok(self.asserted_holds(copy, account)? || !self.asserted_anywhere(copy, account)?)
    }

    /// Whether every copy of a stored quad is shown without asking each.
    pub(crate) fn shows_all(&self, touching: bool) -> bool {
        !self.inferred_only && (!touching || !self.named_graphs)
    }
}

/// A read's quads one at a time, each step's work counted on the account it is given before it
/// is done: `Err` once that account has stopped. The parts of a counted equality read (its
/// stored scans, their merge, the expansion and its named scan) are steps, so the read's one
/// account ([`Read`]) counts all of their work (fix check r2 G1's siblings: an account of each
/// part's own let each allow a run after the same yes).
pub(crate) trait Step {
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped>;
}

impl Step for crate::index::QuadScan<'_> {
    #[inline]
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        self.step_with(account)
    }
}

/// A part a read may not have (a stack the model leaves out).
impl<S: Step> Step for Option<S> {
    #[inline]
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        match self {
            Some(part) => part.step(account),
            None => Ok(None),
        }
    }
}

/// A plain read's input: its quads, counting nothing.
pub(crate) struct Plain<I>(pub(crate) I);

impl<I: Iterator<Item = EncodedQuad>> Step for Plain<I> {
    #[inline]
    fn step(&mut self, _: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        Ok(self.0.next())
    }
}

/// A [`Step`] read one quad ahead.
pub(crate) struct Ahead<S> {
    step: S,
    head: Option<Option<EncodedQuad>>,
}

impl<S: Step> Ahead<S> {
    fn new(step: S) -> Self {
        Self { step, head: None }
    }

    /// The next quad, not taken.
    fn peek(&mut self, account: &mut impl Account) -> Result<Option<&EncodedQuad>, Stopped> {
        if self.head.is_none() {
            self.head = Some(self.step.step(account)?);
        }
        Ok(self.head.as_ref().and_then(Option::as_ref))
    }

    fn next(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        match self.head.take() {
            Some(head) => Ok(head),
            None => self.step.step(account),
        }
    }
}

/// Two steps, each sorted by `permutation`, merged in that order: the snapshot's `SortedMerge`
/// for a read whose parts share its one account.
pub(crate) struct Merged<L, R> {
    left: Ahead<L>,
    right: Ahead<R>,
    permutation: Permutation,
}

impl<L: Step, R: Step> Merged<L, R> {
    pub(crate) fn new(left: L, right: R, permutation: Permutation) -> Self {
        Self {
            left: Ahead::new(left),
            right: Ahead::new(right),
            permutation,
        }
    }
}

impl<L: Step, R: Step> Step for Merged<L, R> {
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        let permutation = self.permutation;
        let left = self
            .left
            .peek(account)?
            .map(|quad| permutation.to_key(quad));
        let right = self
            .right
            .peek(account)?
            .map(|quad| permutation.to_key(quad));
        match (left, right) {
            (Some(left), Some(right)) if right < left => self.right.next(account),
            (Some(_), _) => self.left.next(account),
            (None, _) => self.right.next(account),
        }
    }
}

/// A counted read's quads: its parts' ([`Step`]), all of their work on its one account,
/// ending where they end or stop (the stop is the visitor's to tell).
pub(crate) struct Read<S, A> {
    pub(crate) step: S,
    pub(crate) account: A,
}

impl<S: Step, A: Account> Iterator for Read<S, A> {
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        self.step.step(&mut self.account).ok().flatten()
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
/// A [`Step`]: a counted read's expansion counts each copy it makes before it makes it, each
/// copy it hands on, its input's steps and its visibility probes on the read's one account
/// (REAS-3, sweeps SW013, SW014; fix check r2 G1's siblings). The plain reads' is an iterator
/// over a plain input, counting nothing.
pub(crate) struct Expand<'a, S: Step> {
    input: Ahead<S>,
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
}

impl<'a, I: Iterator<Item = EncodedQuad>> Expand<'a, Plain<I>> {
    pub(crate) fn new(
        input: I,
        classes: Arc<Classes>,
        pattern: QuadPattern,
        permutation: Permutation,
        named: bool,
        model: ReadModel,
        version: &'a Version,
    ) -> Self {
        Expand::of(
            Plain(input),
            classes,
            pattern,
            permutation,
            named,
            model,
            version,
        )
    }
}

impl<'a, S: Step> Expand<'a, S> {
    /// [`Expand::new`] over any input: a counted read's, stepped with its account.
    pub(crate) fn of(
        input: S,
        classes: Arc<Classes>,
        pattern: QuadPattern,
        permutation: Permutation,
        named: bool,
        model: ReadModel,
        version: &'a Version,
    ) -> Self {
        Self {
            input: Ahead::new(input),
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
    fn pull(&mut self, account: &mut impl Account) -> Result<bool, Stopped> {
        let Some(raw) = self.input.next(account)? else {
            return Ok(false);
        };
        if !raw.graph.is_default_graph() {
            if self.named {
                if !self.room(1) {
                    return Ok(self.refuse());
                }
                self.pending.push(Reverse(self.permutation.to_key(&raw)));
            }
            return Ok(true);
        }
        if !self.classes.is_canonical(&raw) {
            return Ok(true);
        }
        let copies = self.classes.copies(&raw, &self.pattern);
        if !self.room(copies) {
            return Ok(self.refuse());
        }
        let (permutation, visibility) = (self.permutation, self.visibility);
        let touching = self.classes.touches(&raw);
        let all = visibility.shows_all(touching);
        let pending = &mut self.pending;
        let mut stopped = false;
        self.classes.expand(&raw, &self.pattern, &mut |quad| {
            // Each copy a unit, counted before it is made (SW013), its probe after it.
            if !account.take(1) {
                stopped = true;
                return std::ops::ControlFlow::Break(());
            }
            #[cfg(any(test, all(feature = "witness", debug_assertions)))]
            crate::witness::add(crate::witness::Site::ExpandCopies, 1);
            let shown = match all {
                true => Ok(true),
                false => visibility.shows_with(&quad, touching, account),
            };
            match shown {
                Ok(true) => pending.push(Reverse(permutation.to_key(&quad))),
                Ok(false) => {}
                Err(Stopped) => {
                    stopped = true;
                    return std::ops::ControlFlow::Break(());
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        match stopped {
            true => Err(Stopped),
            false => Ok(true),
        }
    }

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

impl<S: Step> Drop for Expand<'_, S> {
    /// The heap freed, then its charge released.
    fn drop(&mut self) {
        self.pending = BinaryHeap::new();
        if let Some(reads) = &self.reads {
            reads.release(std::mem::take(&mut self.held));
        }
    }
}

impl<S: Step> Step for Expand<'_, S> {
    fn step(&mut self, account: &mut impl Account) -> Result<Option<EncodedQuad>, Stopped> {
        if self.refused {
            return Ok(None);
        }
        loop {
            // A step, a copy handed on or an input quad pulled: a unit.
            if !account.take(1) {
                return Err(Stopped);
            }
            // What the heap holds may go out once no later input can sort before it.
            if let Some(Reverse(first)) = self.pending.peek() {
                let (pattern, permutation) = (&self.pattern, self.permutation);
                let safe = match self.input.peek(account)? {
                    None => true,
                    Some(next) => *first <= Self::least_key(pattern, permutation, next),
                };
                if safe {
                    let Reverse(key) = self.pending.pop().expect("peeked");
                    return Ok(Some(self.permutation.key_to_quad(&key)));
                }
            }
            if !self.pull(account)? && (self.refused || self.pending.is_empty()) {
                return Ok(None);
            }
        }
    }
}

impl<I: Iterator<Item = EncodedQuad>> Iterator for Expand<'_, Plain<I>> {
    type Item = EncodedQuad;

    #[inline]
    fn next(&mut self) -> Option<EncodedQuad> {
        self.step(&mut Free).unwrap_or(None)
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

    /// Spec (`sort_counted`): it sorts distinct ids as `sort_unstable` does, asking before at
    /// most `POLL_UNITS` units at a time, and a false answer ends it with false. Lengths around
    /// its run size and past several merge passes, the ids a permutation of `0..len`.
    #[test]
    fn a_counted_sort_sorts_and_stops_when_told() {
        for len in [
            0,
            1,
            2,
            SORT_RUN - 1,
            SORT_RUN,
            SORT_RUN + 1,
            5 * SORT_RUN + 17,
            20_000,
        ] {
            let ids: Vec<u64> = (0..len as u64).map(|i| (i * 7919) % len as u64).collect();
            let mut counted = ids.clone();
            let largest = std::cell::Cell::new(0);
            assert!(sort_counted(&mut counted, |n| {
                largest.set(largest.get().max(n));
                true
            }));
            let mut plain = ids.clone();
            plain.sort_unstable();
            assert_eq!(counted, plain, "{len} ids");
            assert!(
                largest.get() <= u64::from(nrese_exec::POLL_UNITS),
                "{len} ids"
            );
        }
        let mut ids: Vec<u64> = (0..20_000).rev().collect();
        let asked = std::cell::Cell::new(0);
        assert!(!sort_counted(&mut ids, |_| {
            asked.set(asked.get() + 1);
            asked.get() < 10
        }));
        assert_eq!(asked.get(), 10, "no ask after the false one");
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

    /// REAS-3a review r1 R1. Spec (`crate::visit`: after its visitor's last yes a counted read
    /// does at most the units that yes allowed, `POLL_UNITS`, plus a step that can't be split;
    /// finishing a class is split): a counted read's cold class build finishes its classes
    /// within its visitor's interval. One asserted chain of three intervals of identities, one
    /// class built one identity at a time, and one statement about its representative: between
    /// two asks of the visitor, and after the last, the read does at most an interval of work
    /// (`work`: fix check r2 G2, the bound itself). Finishing the class after one count of it
    /// (its members copied, checked for literals and indexed), it handled three times the class
    /// unseen; with its scan and its unions counted on two credits, each within its own run,
    /// both runs' work followed a yes (fix check r2 G1).
    #[test]
    fn a_counted_class_build_finishes_its_class_within_its_visitor_s_interval() {
        let interval = u64::from(nrese_exec::POLL_UNITS);
        let n = 3 * interval as usize;
        let node = |name: &str| nrese_rdf::NamedNode::new_unchecked(format!("urn:{name}"));
        let engine = crate::Engine::new(crate::EngineConfig {
            background_maintenance: false,
            ..crate::EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let same_as = tx.intern(node("sameAs").as_ref().into());
        for i in 1..n {
            tx.insert(
                nrese_rdf::Quad::new(
                    node(&format!("s{}", i - 1)),
                    node("sameAs"),
                    node(&format!("s{i}")),
                    nrese_rdf::GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.insert(
            nrese_rdf::Quad::new(
                node("s0"),
                node("p"),
                node("o"),
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        let snapshot = engine.snapshot();
        let p = snapshot.lookup(node("p").as_ref().into()).unwrap();
        let pattern = QuadPattern {
            predicate: Some(p),
            ..QuadPattern::all()
        };
        let last = std::cell::Cell::new(work());
        let largest = std::cell::Cell::new(0);
        let gap = || {
            let now = work();
            largest.set(largest.get().max(now - last.replace(now)));
        };
        let counted = snapshot
            .counted()
            .count(crate::ReadModel::Materialised, &pattern, &|_| {
                gap();
                true
            });
        gap();
        assert_eq!(
            counted,
            Ok(crate::Count::Exact(n as u64)),
            "the statement's copy for each identity"
        );
        let largest = largest.get();
        assert!(
            largest <= interval,
            "{largest} units of work between two asks of the visitor: at most {interval}"
        );
    }

    /// The work a counted class build does, counted where it is done: scan steps and runs, their
    /// searches' comparisons and their merges' work, copies, finished identities, sort work, and
    /// the union kernel's member moves and the slots its enumeration walks (fix check r1 F3, r2
    /// G2: not what the build told its visitor).
    fn work() -> u64 {
        use crate::witness::{self, Site};
        [
            Site::ScanSteps,
            Site::RunProbes,
            Site::ExpandCopies,
            Site::ClassFinish,
            Site::SortWork,
            Site::SearchWork,
            Site::MergeWork,
        ]
        .into_iter()
        .map(witness::get)
        .sum::<u64>()
            + nrese_exec::classes::witness::moved()
            + nrese_exec::classes::witness::examined()
    }

    /// A snapshot under equality whose default graph holds `same_as` statements between the
    /// names given (interned in the order given first), and `subject p o`; and its pattern
    /// `? p ?`.
    fn under_equality(
        names: &[String],
        same_as: &[(usize, usize)],
        subject: usize,
    ) -> (crate::Snapshot, QuadPattern) {
        let node = |name: &str| nrese_rdf::NamedNode::new_unchecked(format!("urn:{name}"));
        let engine = crate::Engine::new(crate::EngineConfig {
            background_maintenance: false,
            ..crate::EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let predicate = tx.intern(node("sameAs").as_ref().into());
        for name in names {
            tx.intern(node(name).as_ref().into());
        }
        let quad = |s: &str, p: &str, o: &str| {
            nrese_rdf::Quad::new(
                node(s),
                node(p),
                node(o),
                nrese_rdf::GraphName::DefaultGraph,
            )
        };
        for &(a, b) in same_as {
            tx.insert(quad(&names[a], "sameAs", &names[b]).as_ref());
        }
        tx.insert(quad(&names[subject], "p", "o").as_ref());
        tx.commit().unwrap();
        engine.set_equality(Some(predicate));
        let snapshot = engine.snapshot();
        let p = snapshot.lookup(node("p").as_ref().into()).unwrap();
        let pattern = QuadPattern {
            predicate: Some(p),
            ..QuadPattern::all()
        };
        (snapshot, pattern)
    }

    /// Counts `pattern` on `snapshot` (materialised, its classes cold) with a visitor that
    /// answers yes `yeses` times, then no: the answer, and the most work done between a yes
    /// and the visitor's next call (or the read's end).
    fn stopped_after(
        snapshot: &crate::Snapshot,
        pattern: &QuadPattern,
        yeses: usize,
    ) -> (Result<crate::Count, crate::Stopped>, u64) {
        let calls = std::cell::Cell::new(0);
        let last = std::cell::Cell::new(work());
        let largest = std::cell::Cell::new(0);
        let gap = || {
            let now = work();
            largest.set(largest.get().max(now - last.replace(now)));
        };
        let answer = snapshot
            .counted()
            .count(crate::ReadModel::Materialised, pattern, &|_| {
                gap();
                calls.set(calls.get() + 1);
                calls.get() <= yeses
            });
        gap();
        (answer, largest.get())
    }

    /// Spec (`crate::visit`: after its visitor's last yes a counted read does at most a run of
    /// work, then stops at its next call; review r1 R1, fix check r1 F3): a counted read's cold
    /// class build, stopped after its visitor's k-th yes for every k until it finishes, answers
    /// `Stopped` and does at most an interval of work after any yes (fix check r2 G2: the
    /// bound itself, `POLL_UNITS`). Then, not stopped, it
    /// answers every copy. Work is counted where it is done (`work`). Map initialisation, held
    /// for the owner (fix check r1 F2), is not counted.
    fn a_cold_class_build_stops_within_an_interval_of_any_yes(
        names: &[String],
        same_as: &[(usize, usize)],
        subject: usize,
        copies: u64,
    ) {
        let interval = u64::from(nrese_exec::POLL_UNITS);
        // A stopped build keeps nothing, so each run builds the classes cold again.
        let (snapshot, pattern) = under_equality(names, same_as, subject);
        // (stopped after, units of work after a yes) for each run past the bound.
        let mut over = Vec::new();
        for yeses in 0.. {
            let (answer, largest) = stopped_after(&snapshot, &pattern, yeses);
            if largest > interval {
                over.push((yeses, largest));
            }
            match answer {
                Err(crate::Stopped) => continue,
                Ok(count) => {
                    assert_eq!(count, crate::Count::Exact(copies), "every copy");
                    break;
                }
            }
        }
        assert!(
            over.is_empty(),
            "units of work after a yes, past {interval} (stopped after, units): {over:?}"
        );
    }

    /// REAS-3a fix check r1 R1 (the union and the sort) and F3. Two chains of three intervals
    /// of identities each, then one statement joining them, last in every order of the scan
    /// (its ends are interned last): that union moves a whole chain, three intervals of members,
    /// and the joined class's sort sorts six.
    #[test]
    fn a_large_union_and_sort_stop_within_an_interval_of_any_yes() {
        let half = 3 * nrese_exec::POLL_UNITS as usize;
        let names: Vec<String> = (0..half - 1)
            .map(|i| format!("a{i}"))
            .chain((0..half - 1).map(|i| format!("b{i}")))
            .chain(["a-end".to_owned(), "b-end".to_owned()])
            .collect();
        let (a, b) = (|i: usize| i, |i: usize| half - 1 + i);
        let (a_end, b_end) = (2 * half - 2, 2 * half - 1);
        let mut same_as: Vec<(usize, usize)> = (1..half - 1)
            .flat_map(|i| [(a(i - 1), a(i)), (b(i - 1), b(i))])
            .collect();
        same_as.extend([(a(half - 2), a_end), (b(half - 2), b_end), (a_end, b_end)]);
        a_cold_class_build_stops_within_an_interval_of_any_yes(
            &names,
            &same_as,
            a(0),
            2 * half as u64,
        );
    }

    /// REAS-3a fix check r2 G3. Three intervals of classes of two, then each joined to a hub
    /// interned last (so its statements come last in the scan): the union kernel's table of roots
    /// held three intervals of classes and keeps one. Enumerating the classes walked the table's
    /// slots, three intervals of them, before the finish's first ask.
    #[test]
    fn classes_joined_into_one_enumerate_within_an_interval_of_any_yes() {
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let names: Vec<String> = (0..n)
            .flat_map(|i| [format!("c{i}"), format!("d{i}")])
            .chain(["hub".to_owned()])
            .collect();
        let hub = 2 * n;
        let same_as: Vec<(usize, usize)> = (0..n)
            .flat_map(|i| [(2 * i, 2 * i + 1), (hub, 2 * i)])
            .collect();
        a_cold_class_build_stops_within_an_interval_of_any_yes(
            &names,
            &same_as,
            0,
            2 * n as u64 + 1,
        );
    }

    /// REAS-3a fix check r1 F3, the many-small-classes shape: three intervals of classes of two.
    #[test]
    fn many_small_classes_stop_within_an_interval_of_any_yes() {
        let n = 3 * nrese_exec::POLL_UNITS as usize;
        let names: Vec<String> = (0..n)
            .flat_map(|i| [format!("c{i}"), format!("d{i}")])
            .collect();
        let same_as: Vec<(usize, usize)> = (0..n).map(|i| (2 * i, 2 * i + 1)).collect();
        a_cold_class_build_stops_within_an_interval_of_any_yes(&names, &same_as, 0, 2);
    }

    /// The most work `read` does between two asks of its visitor, which always answers yes, or
    /// after the last until it returns.
    fn largest_gap(read: impl FnOnce(crate::visit::Visitor<'_>)) -> u64 {
        let last = std::cell::Cell::new(work());
        let largest = std::cell::Cell::new(0);
        let gap = || {
            let now = work();
            largest.set(largest.get().max(now - last.replace(now)));
        };
        read(&|_| {
            gap();
            true
        });
        gap();
        largest.get()
    }

    fn node(name: &str) -> nrese_rdf::NamedNode {
        nrese_rdf::NamedNode::new_unchecked(format!("urn:{name}"))
    }

    fn statement(s: &str, p: &str, o: &str) -> nrese_rdf::Quad {
        nrese_rdf::Quad::new(
            node(s),
            node(p),
            node(o),
            nrese_rdf::GraphName::DefaultGraph,
        )
    }

    /// REAS-3a fix check r2, G1's sibling in an expansion (`Expand`). Spec (`crate::visit`: at
    /// most an interval of work after any yes): a counted sorted read under equality does at most
    /// an interval of work between two asks of its visitor, its stored scan's steps and its
    /// copies together, and answers as the plain read does. Every quad, read by SPOG: a class of
    /// three first in the order (`rep`, then two aliases interned after it) and six intervals of
    /// statements `rep p o_i`: every alias's copy waits for the input's end, so the expansion
    /// asks for full runs. Its scan's ledger and its own credit each allowed a run after the
    /// same yes.
    #[test]
    fn an_expanded_read_steps_and_copies_within_an_interval_of_any_yes() {
        let interval = u64::from(nrese_exec::POLL_UNITS);
        let n = 6 * interval as usize;
        let engine = crate::Engine::new(crate::EngineConfig {
            background_maintenance: false,
            ..crate::EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let rep = tx.intern(node("rep").as_ref().into());
        let aliases = ["alias1", "alias2"].map(|alias| tx.intern(node(alias).as_ref().into()));
        let same_as = tx.intern(node("sameAs").as_ref().into());
        for alias in aliases {
            tx.insert_inferred(crate::EncodedTriple {
                subject: alias,
                predicate: same_as,
                object: rep,
            });
        }
        for i in 0..n {
            tx.insert(statement("rep", "p", &format!("o{i}")).as_ref());
        }
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        let snapshot = engine.snapshot();
        let (pattern, model) = (QuadPattern::all(), ReadModel::Materialised);
        let plain: Vec<_> = snapshot
            .scan_sorted_in(model, &pattern, Permutation::Spog)
            .expect("SPOG answers it")
            .collect();
        assert!(plain.len() >= 3 * n, "each statement's three copies");
        let mut read: Vec<EncodedQuad> = Vec::new();
        let largest = largest_gap(|visit| {
            read = snapshot
                .counted()
                .scan_sorted(model, &pattern, Permutation::Spog, visit)
                .expect("SPOG answers it")
                .collect::<Result<_, _>>()
                .expect("never stopped");
        });
        assert_eq!(read, plain, "the plain read's quads, in its order");
        assert!(
            largest <= interval,
            "{largest} units of work between two asks of the visitor: at most {interval}"
        );
    }

    /// REAS-3a fix check r2, G1's sibling in a sorted read without equality. Spec
    /// (`crate::visit`): a counted sorted read of both stacks does at most an interval of work
    /// between two asks of its visitor, and answers as the plain read does. Every quad, read by
    /// SPOG: three intervals of statements in each stack, their objects alternating, so the merge steps both scans in
    /// turn. Each scan's ledger allowed a run after the same yes.
    #[test]
    fn a_sorted_read_of_both_stacks_steps_within_an_interval_of_any_yes() {
        let interval = u64::from(nrese_exec::POLL_UNITS);
        let n = 3 * interval as usize;
        let engine = crate::Engine::new(crate::EngineConfig {
            background_maintenance: false,
            ..crate::EngineConfig::default()
        })
        .unwrap();
        let mut tx = engine.transaction();
        let (s, p) = (
            tx.intern(node("s").as_ref().into()),
            tx.intern(node("p").as_ref().into()),
        );
        for i in 0..2 * n {
            let o = tx.intern(node(&format!("o{i}")).as_ref().into());
            match i % 2 {
                0 => {
                    tx.insert(statement("s", "p", &format!("o{i}")).as_ref());
                }
                _ => {
                    tx.insert_inferred(crate::EncodedTriple {
                        subject: s,
                        predicate: p,
                        object: o,
                    });
                }
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let (pattern, model) = (QuadPattern::all(), ReadModel::Materialised);
        let plain: Vec<_> = snapshot
            .scan_sorted_in(model, &pattern, Permutation::Spog)
            .expect("SPOG answers it")
            .collect();
        assert_eq!(plain.len(), 2 * n, "both stacks' statements");
        let mut read: Vec<EncodedQuad> = Vec::new();
        let largest = largest_gap(|visit| {
            read = snapshot
                .counted()
                .scan_sorted(model, &pattern, Permutation::Spog, visit)
                .expect("SPOG answers it")
                .collect::<Result<_, _>>()
                .expect("never stopped");
        });
        assert_eq!(read, plain, "the plain read's quads, in its order");
        assert!(
            largest <= interval,
            "{largest} units of work between two asks of the visitor: at most {interval}"
        );
    }
}
