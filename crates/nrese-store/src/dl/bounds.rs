//! The bounds of the store's latest revision (docs/design/owl2-dl.md §8): the lower bound
//! L is the inferred stack (the OWL 2 RL closure); the upper bound U1 ([`super::upper`])
//! is kept here, beside the engine's stacks, and maintained per commit.
//!
//! - **On commit** ([`prepare`], [`install`]): the commit's change to U1 is computed
//!   inside the commit (by the delta executor, or a rebuild where the schema changed) and
//!   applied once the commit is done; U1 after the change also answers the commit's
//!   consistency check where it can (no clash and every `⊥` checked: consistent, with no
//!   DL engine run).
//! - **On query** ([`view`]): the revision's read view, built once per revision: a
//!   snapshot with U1's facts in its inferred stack, never published, and which
//!   predicates and classes U1 has facts on beyond L (the gap's signature). A revision U1
//!   doesn't describe (a write past the pipeline, a start) gets U1 built afresh.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use nrese_engine::{EncodedQuad, EncodedTriple, ReadModel, Snapshot, TermId, Transaction};
use nrese_owl::Ontology;
use nrese_reasoner::eval::Stop;
use nrese_reasoner::ir::Triple;

use super::source;
use super::upper::{Change, GaveUp, Upper};
use crate::StoreService;

/// U1 at a revision.
struct State {
    revision: u64,
    upper: Result<Upper, GaveUp>,
    /// How U1 got to this revision: `delta` (the delta executor), `rebuilt` (compiled
    /// and evaluated afresh on commit), `read` (afresh for a query), `kept` (U1 gave up
    /// and nothing it is compiled from changed).
    last: &'static str,
    /// The TBox's taxonomy for L's memberships ([`super::lower`]): computed on the first
    /// read, kept as long as U1's compilation (both change only with the schema).
    taxonomy: std::sync::OnceLock<Arc<nrese_dl::classify::Taxonomy>>,
    /// The ontology is in OWL 2 RL: the RL rules decide it (theorem PR1 of OWL 2
    /// Profiles: complete for its assertions and its consistency), and U1 isn't compiled.
    rules: bool,
}

/// The route the ontology's profile gives (`nrese_owl::profile`): an OWL 2 RL ontology
/// to the RL rules alone.
fn routed_to_rules(ontology: &Ontology) -> bool {
    nrese_owl::profile::of_ontology(ontology).rl
}

/// U1's place on the RL route: not compiled.
fn not_compiled() -> Result<Upper, GaveUp> {
    Err(GaveUp(
        "not compiled: the ontology is in OWL 2 RL, which the RL rules decide".to_owned(),
    ))
}

/// The store's bounds: U1 for the revision it describes, and the read view of the latest
/// revision a query asked for.
#[derive(Default)]
pub(crate) struct Bounds {
    state: Mutex<Option<State>>,
    view: Mutex<Option<Arc<View>>>,
}

impl std::fmt::Debug for Bounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bounds").finish_non_exhaustive()
    }
}

/// What a query reads of the bounds at a revision.
pub(crate) struct View {
    pub revision: u64,
    /// L: the inferred stack with the memberships the TBox's taxonomy adds
    /// ([`super::lower`]); the snapshot itself where it adds none.
    pub lower: Snapshot,
    /// Those memberships.
    pub lower_facts: usize,
    /// L ∪ U1 as a snapshot (`None`: U1 isn't available).
    pub upper: Option<Snapshot>,
    /// Why U1 can't bound the answers (not built, gave up, or axioms it doesn't cover).
    pub unavailable: Option<String>,
    /// Classes U1 has memberships in beyond L, and the other predicates it has facts on
    /// beyond L (Skolem constants included): the gap's signature.
    pub gap_classes: HashSet<u64>,
    pub gap_predicates: HashSet<u64>,
    /// Those of them with a fact beyond L that names no Skolem constant (a membership of
    /// a named individual; a property value between two): the others' facts beyond L
    /// all have a Skolem constant, which is never an answer.
    pub named_gap_classes: HashSet<u64>,
    pub named_gap_predicates: HashSet<u64>,
    /// U1's own terms among its facts: never answers.
    pub internal: HashSet<u64>,
    /// U1's facts beyond L.
    pub facts: usize,
    /// U1 has no clash and checks every `⊥`: the data is consistent.
    pub proves_consistency: bool,
    /// The ontology is in OWL 2 RL: L is complete for every predicate, and consistent
    /// (the RL rules decide it).
    pub rules: bool,
}

fn triple(q: EncodedTriple) -> Triple {
    [q.subject.raw(), q.predicate.raw(), q.object.raw()]
}

fn quad_triple(q: EncodedQuad) -> Triple {
    [q.subject.raw(), q.predicate.raw(), q.object.raw()]
}

/// What a commit prepared for U1, applied by [`install`] once the commit is done.
pub(crate) enum Prepared {
    /// The delta executor's change to U1.
    Delta(Box<Change>),
    /// U1 compiled and evaluated afresh (the schema changed, or U1 described another
    /// revision), or why that gave up.
    Rebuilt(Box<Result<Upper, GaveUp>>),
    /// U1 had given up, and nothing it was compiled from changed.
    Unavailable,
    /// The ontology is (still) in OWL 2 RL: the RL rules decide it, no U1.
    Rules,
}

/// A commit's preparation: what to install, and what U1 after the commit says about
/// consistency.
pub(crate) struct Preparation {
    base: u64,
    prepared: Prepared,
    /// U1 after the commit proves the data consistent (or, on the RL route, the RL
    /// rules do: [`Preparation::decided_by`]).
    pub proves_consistency: bool,
    /// `upper-bound`, or `rules` on the RL route.
    pub decided_by: &'static str,
    /// The ontology the commit leaves, where the preparation had to read it (for the
    /// consistency check, which then needn't read it again).
    pub ontology: Option<Ontology>,
}

/// Prepares U1 for the commit `tx` (after the RL reasoning applied its changes): the
/// change by the delta executor where only assertions over U1's signature changed, else
/// U1 afresh. Cost: the change's, or the closure's on a rebuild.
pub(crate) fn prepare(store: &StoreService, tx: &Transaction<'_>, stop: Stop<'_>) -> Preparation {
    let base = tx.base().revision();
    let pending = tx.pending_snapshot();
    let deadline = Instant::now() + store.config().dl.timeout;
    let stop = move || stop() || Instant::now() >= deadline;
    let state = store
        .dl()
        .bounds
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let current = state.as_ref().filter(|s| s.revision == base);
    let asserted: Vec<Triple> = tx.inserted().chain(tx.deleted()).map(quad_triple).collect();
    if let Some(State { rules: true, .. }) = current
        && !asserted.iter().any(|t| changes_schema(tx, *t))
    {
        return Preparation {
            base,
            prepared: Prepared::Rules,
            proves_consistency: true,
            decided_by: "rules",
            ontology: None,
        };
    }
    if let Some(State { upper, .. }) = current {
        match upper {
            // A U1 with equality classes is evaluated afresh (`Upper`'s module docs).
            Ok(upper)
                if upper.classes().is_empty()
                    && asserted.iter().all(|t| upper.is_assertion(*t)) =>
            {
                let before = tx.base();
                let inserted: Vec<Triple> = tx
                    .inserted()
                    .map(quad_triple)
                    .chain(tx.inferred_inserted().map(triple))
                    .filter(|t| !in_view(before, *t))
                    .collect();
                let deleted: Vec<Triple> = tx
                    .deleted()
                    .map(quad_triple)
                    .chain(tx.inferred_deleted().map(triple))
                    .collect();
                match upper.change(&pending, &inserted, &deleted, &stop) {
                    Ok(change) if !change.merges() => {
                        return Preparation {
                            base,
                            decided_by: "upper-bound",
                            proves_consistency: upper.program.proves_consistency()
                                && upper.clashes_after(&change) == 0,
                            prepared: Prepared::Delta(Box::new(change)),
                            ontology: None,
                        };
                    }
                    // The commit equates terms: U1 afresh, by representatives (below).
                    Ok(_) => {}
                    Err(gave_up) => {
                        return Preparation {
                            base,
                            prepared: Prepared::Rebuilt(Box::new(Err(gave_up))),
                            decided_by: "upper-bound",
                            proves_consistency: false,
                            ontology: None,
                        };
                    }
                }
            }
            Err(_) if !asserted.iter().any(|t| changes_schema(tx, *t)) => {
                return Preparation {
                    base,
                    prepared: Prepared::Unavailable,
                    decided_by: "upper-bound",
                    proves_consistency: false,
                    ontology: None,
                };
            }
            _ => {}
        }
    }
    drop(state);
    source::intern_vocabulary(tx);
    let ontology = source::read_pending(tx);
    if routed_to_rules(&ontology) && ontology.diagnostics.iter().all(|d| !d.is_fatal()) {
        // The RL gate that ran before has decided the commit's consistency.
        return Preparation {
            base,
            prepared: Prepared::Rules,
            proves_consistency: true,
            decided_by: "rules",
            ontology: None,
        };
    }
    let normalised = nrese_owl::normalise(&ontology);
    let upper = Upper::build(
        &ontology,
        &normalised,
        &pending,
        &|t| Some(tx.intern(t)),
        &stop,
    );
    Preparation {
        base,
        decided_by: "upper-bound",
        proves_consistency: upper.as_ref().is_ok_and(Upper::proves_consistency),
        prepared: Prepared::Rebuilt(Box::new(upper)),
        ontology: Some(ontology),
    }
}

/// Whether a changed statement may change the ontology's schema (and so U1's rules),
/// judged without U1's signature: a blank node, OWL's, RDF's or RDFS's vocabulary as
/// predicate (but `rdf:type` with another class, and `owl:sameAs`).
fn changes_schema(tx: &Transaction<'_>, [s, p, o]: Triple) -> bool {
    use nrese_engine::TermKind;
    let kind = |t: u64| TermId::from_raw(t).kind();
    if kind(s) == TermKind::BlankNode || kind(o) == TermKind::BlankNode {
        return true;
    }
    let reserved = |t: u64| match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => {
            let iri = n.as_str();
            iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                || iri.starts_with("http://www.w3.org/2002/07/owl#")
        }
        _ => false,
    };
    let text = |t: u64| match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => n.into_string(),
        _ => String::new(),
    };
    match text(p).as_str() {
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" => reserved(o),
        "http://www.w3.org/2002/07/owl#sameAs" => false,
        _ => reserved(p),
    }
}

fn in_view(view: &Snapshot, [s, p, o]: Triple) -> bool {
    view.quads_for_pattern_in(
        ReadModel::Materialised,
        &nrese_engine::QuadPattern {
            subject: Some(TermId::from_raw(s)),
            predicate: Some(TermId::from_raw(p)),
            object: Some(TermId::from_raw(o)),
            graph: nrese_engine::GraphSelector::Any,
        },
    )
    .next()
    .is_some()
}

/// Installs a commit's preparation for its new `revision`.
pub(crate) fn install(store: &StoreService, preparation: Preparation, revision: u64) {
    let mut state = store
        .dl()
        .bounds
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    match preparation.prepared {
        Prepared::Rebuilt(upper) => {
            *state = Some(State {
                revision,
                upper: *upper,
                last: "rebuilt",
                taxonomy: std::sync::OnceLock::new(),
                rules: false,
            });
        }
        Prepared::Rules => {
            *state = Some(State {
                revision,
                upper: not_compiled(),
                last: "rules",
                taxonomy: std::sync::OnceLock::new(),
                rules: true,
            });
        }
        Prepared::Delta(change) => match state.as_mut() {
            Some(s) if s.revision == preparation.base => {
                if let Ok(upper) = &mut s.upper {
                    upper.apply(*change);
                }
                s.revision = revision;
                s.last = "delta";
            }
            // A query rebuilt U1 for the new revision meanwhile: it stands.
            Some(s) if s.revision == revision => {}
            _ => *state = None,
        },
        Prepared::Unavailable => {
            if let Some(s) = state.as_mut()
                && s.revision == preparation.base
            {
                s.revision = revision;
                s.last = "kept";
            }
        }
    }
}

/// The read view of the store's latest revision, and its snapshot: U1 built afresh if it
/// doesn't describe that revision (O(the closure)), the view once per revision.
pub(crate) fn view(store: &StoreService) -> (Snapshot, Arc<View>) {
    let bounds = &store.dl().bounds;
    let mut state = bounds.state.lock().unwrap_or_else(|p| p.into_inner());
    let snapshot = store.engine().snapshot();
    let revision = snapshot.revision();
    if let Some(view) = &*bounds.view.lock().unwrap_or_else(|p| p.into_inner())
        && view.revision == revision
    {
        return (snapshot, Arc::clone(view));
    }
    if state.as_ref().is_none_or(|s| s.revision != revision) {
        let tx = store.engine().speculative();
        let replica = store.dl().replica();
        if !replica {
            source::intern_vocabulary(&tx);
        }
        let ontology = source::read_snapshot(&snapshot);
        let rules =
            routed_to_rules(&ontology) && ontology.diagnostics.iter().all(|d| !d.is_fatal());
        let upper = match rules {
            true => not_compiled(),
            false => {
                let normalised = nrese_owl::normalise(&ontology);
                let deadline = Instant::now() + store.config().dl.timeout;
                let stop = move || Instant::now() >= deadline;
                let resolve = |t: nrese_rdf::TermRef<'_>| source::resolve(replica, &tx, t);
                Upper::build(&ontology, &normalised, &snapshot, &resolve, &stop)
            }
        };
        *state = Some(State {
            revision,
            upper,
            last: match rules {
                true => "rules",
                false => "read",
            },
            taxonomy: std::sync::OnceLock::new(),
            rules,
        });
    }
    let s = state.as_ref().expect("built above");
    let taxonomy = match &s.upper {
        Ok(_) if !s.rules => Some(Arc::clone(s.taxonomy.get_or_init(|| {
            let ontology = super::query::ontology_at(store, &snapshot);
            Arc::new(super::lower::tbox_taxonomy(store, &ontology))
        }))),
        _ => None,
    };
    let view = Arc::new(build_view(&snapshot, s, taxonomy.as_deref()));
    *bounds.view.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&view));
    (snapshot, view)
}

fn build_view(
    snapshot: &Snapshot,
    state: &State,
    taxonomy: Option<&nrese_dl::classify::Taxonomy>,
) -> View {
    if state.rules {
        return View {
            revision: state.revision,
            lower: snapshot.clone(),
            lower_facts: 0,
            upper: None,
            unavailable: None,
            gap_classes: HashSet::new(),
            gap_predicates: HashSet::new(),
            named_gap_classes: HashSet::new(),
            named_gap_predicates: HashSet::new(),
            internal: HashSet::new(),
            facts: 0,
            proves_consistency: true,
            rules: true,
        };
    }
    let upper = match &state.upper {
        Ok(upper) => upper,
        Err(GaveUp(why)) => {
            return View {
                revision: state.revision,
                lower: snapshot.clone(),
                lower_facts: 0,
                upper: None,
                unavailable: Some(format!("the upper bound isn't available: {why}")),
                gap_classes: HashSet::new(),
                gap_predicates: HashSet::new(),
                named_gap_classes: HashSet::new(),
                named_gap_predicates: HashSet::new(),
                internal: HashSet::new(),
                facts: 0,
                proves_consistency: false,
                rules: false,
            };
        }
    };
    let rdf_type = upper.program.names.rdf_type;
    let clash = upper.program.names.clash;
    let lower: HashSet<Triple> = taxonomy
        .map(|t| super::lower::memberships(snapshot, t, rdf_type))
        .unwrap_or_default()
        .into_iter()
        .collect();
    let encode = |[s, p, o]: Triple| {
        EncodedTriple::new(
            TermId::from_raw(s),
            TermId::from_raw(p),
            TermId::from_raw(o),
        )
        .in_default_graph()
    };
    let lower_quads: Vec<EncodedQuad> = lower.iter().map(|&t| encode(t)).collect();
    let mut view = View {
        revision: state.revision,
        lower: match lower_quads.is_empty() {
            true => snapshot.clone(),
            false => snapshot.with_inferred_added(&lower_quads),
        },
        lower_facts: lower.len(),
        upper: None,
        unavailable: (!upper.program.incomplete.is_empty()).then(|| {
            format!(
                "the upper bound doesn't cover {} axiom(s) (first: {})",
                upper.program.incomplete.len(),
                upper.program.incomplete[0].1
            )
        }),
        gap_classes: HashSet::new(),
        gap_predicates: HashSet::new(),
        named_gap_classes: HashSet::new(),
        named_gap_predicates: HashSet::new(),
        internal: HashSet::new(),
        facts: upper.stack.len(),
        proves_consistency: upper.proves_consistency(),
        rules: false,
    };
    // A fact over a representative stands for its members: named where one of them is.
    let named_classes: HashSet<u64> = upper
        .classes()
        .classes()
        .filter(|(_, members)| members.iter().any(|&m| !upper.is_internal(m)))
        .map(|(representative, _)| representative)
        .collect();
    let named = |t: u64| !upper.is_internal(t) || named_classes.contains(&t);
    // L's memberships beside it, over representatives as the expanded reads want them.
    let classes = upper.classes();
    let mut quads: Vec<EncodedQuad> = match classes.is_empty() {
        true => lower_quads,
        false => lower
            .iter()
            .map(|&[s, p, o]| encode([classes.representative(s), p, classes.representative(o)]))
            .collect(),
    };
    for [s, p, o] in upper.stack.iter() {
        // L's memberships from the taxonomy are certain: no gap.
        if lower.contains(&[s, p, o]) {
            continue;
        }
        for t in [s, p, o] {
            if upper.is_internal(t) {
                view.internal.insert(t);
            }
        }
        if p == clash {
            continue;
        }
        if p == rdf_type {
            if !upper.is_internal(o) {
                view.gap_classes.insert(o);
                if named(s) {
                    view.named_gap_classes.insert(o);
                }
            }
        } else if !upper.is_internal(p) {
            view.gap_predicates.insert(p);
            if named(s) && named(o) {
                view.named_gap_predicates.insert(p);
            }
        }
        quads.push(encode([s, p, o]));
    }
    let upper_view = snapshot.with_inferred_added(&quads);
    // Read expanded to every identity of U1's classes (the stack keeps representatives).
    view.upper = Some(match upper.classes().is_empty() {
        true => upper_view,
        false => upper_view.with_equality(Some(TermId::from_raw(upper.same_as()))),
    });
    view
}

/// The bounds at the store's latest revision, for reports and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundsReport {
    pub revision: u64,
    /// Why U1 can't bound answers (`None`: it can).
    pub unavailable: Option<String>,
    /// U1's facts beyond L.
    pub upper_facts: usize,
    /// L's memberships beyond the RL closure, from the TBox's taxonomy.
    pub lower_facts: usize,
    /// Classes and other predicates with facts in U1 beyond L.
    pub gap_classes: usize,
    pub gap_predicates: usize,
    /// How U1 got to this revision: `delta`, `rebuilt`, `read` or `kept`.
    pub last: &'static str,
    /// The literals U1 reads by value: those of predicates its rules compare.
    pub literals_by_value: usize,
    /// U1's `owl:sameAs` classes (kept by representatives) and their largest.
    pub equality_classes: usize,
    pub largest_class: usize,
}

/// The bounds at the latest revision (U1 built afresh if it doesn't describe it).
pub(crate) fn report(store: &StoreService) -> BoundsReport {
    let (_, view) = self::view(store);
    let state = store
        .dl()
        .bounds
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let current = state.as_ref().filter(|s| s.revision == view.revision);
    let last = current.map_or("read", |s| s.last);
    let upper = current.and_then(|s| s.upper.as_ref().ok());
    let literals_by_value = upper.map_or(0, Upper::literals_by_value);
    let (equality_classes, largest_class) = upper.map_or((0, 0), |u| {
        let sizes = u.classes().classes().map(|(_, members)| members.len());
        sizes.fold((0, 0), |(n, max), size| (n + 1, max.max(size)))
    });
    BoundsReport {
        revision: view.revision,
        unavailable: view.unavailable.clone(),
        upper_facts: view.facts,
        lower_facts: view.lower_facts,
        gap_classes: view.gap_classes.len(),
        gap_predicates: view.gap_predicates.len(),
        last,
        literals_by_value,
        equality_classes,
        largest_class,
    }
}

/// U1's facts beyond L at the latest revision, as N-Triples terms, sorted: as
/// maintained, or with `afresh` evaluated anew (not installed). `None` if U1 isn't
/// available. For the differential tests of the maintenance.
pub(crate) fn upper_facts(store: &StoreService, afresh: bool) -> Option<Vec<[String; 3]>> {
    let snapshot = store.engine().snapshot();
    let decode = |t: u64| {
        snapshot
            .decode(TermId::from_raw(t))
            .map_or_else(|| format!("#{t}"), |term| term.to_string())
    };
    let facts: Vec<Triple> = if afresh {
        let tx = store.engine().speculative();
        let replica = store.dl().replica();
        if !replica {
            source::intern_vocabulary(&tx);
        }
        let ontology = source::read_snapshot(&snapshot);
        let normalised = nrese_owl::normalise(&ontology);
        let upper = Upper::build(
            &ontology,
            &normalised,
            &snapshot,
            &|t| source::resolve(replica, &tx, t),
            nrese_reasoner::eval::NEVER,
        )
        .ok()?;
        upper.stack.iter().collect()
    } else {
        let _ = view(store);
        let state = store
            .dl()
            .bounds
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state.as_ref()?.upper.as_ref().ok()?.stack.iter().collect()
    };
    let mut out: Vec<[String; 3]> = facts.into_iter().map(|t| t.map(decode)).collect();
    out.sort();
    Some(out)
}
