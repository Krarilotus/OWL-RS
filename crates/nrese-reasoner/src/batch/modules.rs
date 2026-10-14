//! The modules that stand for rule families: equality (by copying, and by
//! representatives) and transitive closure.

use super::*;

/// The equality module, batch form (design §4.4): replaces `eq-rep-s/p/o`. The generic
/// rules copy a fact to each `sameAs` partner of its subject, predicate or object, and
/// every copy is copied again in the next round: O(k²) derivations per fact for a class
/// of k members. Here each new fact is expanded once to every combination of its terms'
/// classes (a term's class is the term plus its `sameAs` partners; the relation is kept
/// closed by `eq-sym` and the transitive module). A new `sameAs` pair re-expands the facts
/// that mention either side, so classes that grow later are covered.
pub(crate) struct Equality {
    pub(super) same_as: u64,
    /// The module's output of the last round: already expanded, so skipped once.
    pub(super) produced: HashSet<Triple>,
    /// Scans for a term's partners so far (three per expanded fact).
    pub(crate) member_scans: u64,
    /// The terms with a partner, once a round's delta was large enough to collect them
    /// all; kept current from each later round's `sameAs` facts.
    pub(super) partners: Option<TermSet>,
}

/// The `owl:sameAs` id of `rules`, if they reason with equality (`eq-rep-s`).
pub(crate) fn same_as_of(rules: &[Rule]) -> Option<u64> {
    Equality::for_rules(rules).map(|equality| equality.same_as)
}

/// The rules the equality module replaces.
pub(crate) fn is_replacement_rule(rule: &Rule) -> bool {
    matches!(rule.name.as_str(), "eq-rep-s" | "eq-rep-p" | "eq-rep-o")
}

impl Equality {
    /// The module for `rules`, if they include `eq-rep-s` (whose first atom names
    /// `owl:sameAs`).
    pub(crate) fn for_rules(rules: &[Rule]) -> Option<Self> {
        let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
        match rule.body.first()?.0[1] {
            crate::ir::Term::Const(same_as) => Some(Self {
                same_as,
                produced: HashSet::new(),
                member_scans: 0,
                partners: None,
            }),
            crate::ir::Term::Var(_) => None,
        }
    }

    /// The class of `x`: itself and its `sameAs` partners, sorted.
    pub(super) fn members<S: Source + ?Sized>(&self, store: &S, x: u64) -> Vec<u64> {
        let mut members = vec![x];
        store.scan([Some(x), Some(self.same_as), None], Seg::All, &mut |t| {
            members.push(t[2]);
        });
        members.sort_unstable();
        members.dedup();
        members
    }

    /// The expansions of the delta of `store` (see the type's docs), not yet in it.
    pub(crate) fn run<S: Source + ?Sized>(&mut self, store: &S) -> Vec<Triple> {
        let equalities = store.estimate([None, Some(self.same_as), None], Seg::All);
        if equalities == 0 {
            self.produced.clear();
            return Vec::new();
        }
        let same_as = self.same_as;
        // A fact mentioning no term with a partner expands to itself only (#12 of the
        // investigation of 6 October 2026). Which terms have one: for a small delta (a
        // commit's, beside a store's many equalities) each term of it is looked up; for
        // a large one (the batch executor's rounds) they are collected once from all
        // `sameAs` facts and then kept current from each round's.
        if self.partners.is_none()
            && store.estimate([None, None, None], Seg::Delta) * 8 >= equalities
        {
            let mut all = TermSet::default();
            store.scan([None, Some(same_as), None], Seg::All, &mut |[s, _, o]| {
                if s != o {
                    all.insert(s);
                    all.insert(o);
                }
            });
            self.partners = Some(all);
        } else if let Some(partners) = &mut self.partners {
            store.scan([None, Some(same_as), None], Seg::Delta, &mut |[s, _, o]| {
                if s != o {
                    partners.insert(s);
                    partners.insert(o);
                }
            });
        }
        let mut looked_up: HashMap<u64, bool> = HashMap::new();
        let mut has_partner = |term: u64| match &self.partners {
            Some(partners) => partners.contains(term),
            None => *looked_up.entry(term).or_insert_with(|| {
                // An upper bound: zero means no equality mentions the term.
                store.estimate([Some(term), Some(same_as), None], Seg::All) > 0
                    || store.estimate([None, Some(same_as), Some(term)], Seg::All) > 0
            }),
        };
        let mut seeds: Vec<Triple> = Vec::new();
        let mut merged: Vec<u64> = Vec::new();
        let produced = &self.produced;
        store.scan([None, None, None], Seg::Delta, &mut |t| {
            if t[1] == same_as {
                if t[0] != t[2] {
                    merged.push(t[0]);
                }
            } else if t.iter().any(|&term| has_partner(term)) && !produced.contains(&t) {
                seeds.push(t);
            }
        });
        // Facts mentioning a term whose class grew.
        merged.sort_unstable();
        merged.dedup();
        for &a in &merged {
            let mut push = |t: Triple| {
                if t[1] != same_as {
                    seeds.push(t);
                }
            };
            store.scan([Some(a), None, None], Seg::All, &mut push);
            store.scan([None, None, Some(a)], Seg::All, &mut push);
            store.scan([None, Some(a), None], Seg::All, &mut push);
        }
        seeds.par_sort_unstable();
        seeds.dedup();
        self.member_scans += 3 * seeds.len() as u64;
        let out: Vec<Triple> = seeds
            .par_iter()
            .flat_map_iter(|&[s, p, o]| {
                let (ms, mp, mo) = (
                    self.members(store, s),
                    self.members(store, p),
                    self.members(store, o),
                );
                let mut facts = Vec::new();
                for &s in &ms {
                    for &p in &mp {
                        for &o in &mo {
                            let fact = [s, p, o];
                            if !store.contains(fact) {
                                facts.push(fact);
                            }
                        }
                    }
                }
                facts
            })
            .collect();
        self.produced = out.iter().copied().collect();
        out
    }
}

/// A set of terms with a bitmap in front: most terms are in no such set, and a bit test
/// answers for them without hashing (a term per position of every fact read).
#[derive(Default)]
pub(super) struct TermSet {
    pub(super) bits: Vec<u64>,
    pub(super) terms: HashSet<u64>,
}

impl TermSet {
    /// Bits in the bitmap (8 KiB).
    const BITS: usize = 1 << 16;

    pub(super) fn slot(term: u64) -> usize {
        (term.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as usize
    }

    pub(super) fn insert(&mut self, term: u64) {
        if self.bits.is_empty() {
            self.bits = vec![0; Self::BITS / 64];
        }
        let slot = Self::slot(term);
        self.bits[slot / 64] |= 1 << (slot % 64);
        self.terms.insert(term);
    }

    pub(super) fn contains(&self, term: u64) -> bool {
        if self.bits.is_empty() {
            return false;
        }
        let slot = Self::slot(term);
        self.bits[slot / 64] & (1 << (slot % 64)) != 0 && self.terms.contains(&term)
    }

    /// Whether a term of `fact` is in the set.
    pub(super) fn mentioned(&self, [s, p, o]: Triple) -> bool {
        self.contains(s) || self.contains(p) || self.contains(o)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Equality by representatives within the rounds ([`super::representatives`]): the
/// classes so far, and the terms that lost their place as representative. A fact that
/// mentions one is stale: its rewrite is in the store too, and the rules, grounding and
/// the listing read only current facts ([`Current<R>`]).
pub(super) struct Representatives {
    pub(super) same_as: u64,
    pub(super) classes: EqualityClasses,
    /// Former representatives.
    pub(super) stale: TermSet,
    /// The representatives of classes of two or more.
    pub(super) shared: TermSet,
    pub(super) listing: Listing,
    pub(super) merges: usize,
}

impl Representatives {
    /// Merges the classes `pairs` equate; returns the terms that lost their place.
    pub(super) fn merge(&mut self, pairs: &[(u64, u64)]) -> HashSet<u64> {
        let lost: HashSet<u64> = self.classes.union_all(pairs).into_iter().collect();
        if !lost.is_empty() {
            self.merges += 1;
            for &term in &lost {
                self.stale.insert(term);
            }
            for (representative, _) in self.classes.classes() {
                if !self.shared.terms.contains(&representative) {
                    self.shared.insert(representative);
                }
            }
        }
        lost
    }

    /// The facts the merge that took the places of `lost` makes stale, rewritten.
    pub(super) fn rewrites<R: Representation>(
        &self,
        store: &WorkingStore<R>,
        lost: &HashSet<u64>,
    ) -> Vec<Triple> {
        let mut facts = store.mentioning(lost);
        facts
            .par_iter_mut()
            .for_each(|fact| *fact = self.classes.rewrite(*fact));
        facts
    }
}

/// `rule` with every constant replaced by its representative.
pub(super) fn rewrite_rule(rule: &Rule, classes: &EqualityClasses) -> Rule {
    let term = |t: Term| match t {
        Term::Const(c) => Term::Const(classes.representative(c)),
        variable => variable,
    };
    let atom = |a: &crate::ir::Atom| crate::ir::Atom(a.0.map(term));
    Rule {
        name: rule.name.clone(),
        body: rule.body.iter().map(atom).collect(),
        guards: rule.guards.clone(),
        head: match &rule.head {
            Head::Facts(atoms) => Head::Facts(atoms.iter().map(atom).collect()),
            Head::Inconsistent => Head::Inconsistent,
        },
    }
}

/// Whether a constant of `rule` is in `terms`.
pub(super) fn names_any(rule: &Rule, terms: &HashSet<u64>) -> bool {
    let atoms = match &rule.head {
        Head::Facts(atoms) => rule.body.iter().chain(atoms),
        Head::Inconsistent => rule.body.iter().chain(&[]),
    };
    atoms
        .flat_map(|atom| atom.0)
        .any(|t| matches!(t, Term::Const(c) if terms.contains(&c)))
}

/// The transitive module, batch form: for each transitive predicate, its closure by SCC
/// condensation where the relation isn't closed yet (the first round, or after a merge of
/// `sameAs` classes rewrote facts) or other rules added many pairs at once; else the
/// transitivity rule's semi-naive joins over the pairs other rules added, the module's
/// own left out ([`Transitive::joined`]). A closure computed again in every round that
/// added a few pairs was 11.9 of CSPA linux's 16.8 s: its 5.5 M ValueFlow pairs, 18 times
/// (13 October 2026).
#[derive(Default)]
pub(super) struct Transitive {
    pub(super) predicates: std::collections::BTreeMap<u64, Closing>,
    /// New<R> facts each predicate's last recomputation produced.
    pub(super) produced: HashMap<u64, usize>,
    /// The pairs read in the last [`Transitive::run`].
    pub(super) read: u64,
}

/// Where a transitive predicate's closure stands.
pub(super) struct Closing {
    /// Whether other rules added pairs since the last closure or joins.
    dirty: bool,
    /// Whether the relation is closed but for the pairs added since.
    closed: bool,
    /// Whether the inherited narrow graph kernel can represent this relation.
    kernel: bool,
    /// Its transitivity rule, for the joins.
    rule: Rule,
}

impl Transitive {
    pub(super) fn register(&mut self, p: u64) {
        self.predicates.entry(p).or_insert_with(|| Closing {
            dirty: true,
            closed: false,
            kernel: true,
            rule: crate::eval::transitivity(p),
        });
    }

    /// Module families are disjoint from schema families, regardless of term IDs.
    pub(super) fn family(p: u64) -> Family {
        Family::Transitive(p)
    }

    /// Whether `p` is a transitive predicate whose relation is closed (its transitivity
    /// may be joined next).
    pub(super) fn closed(&self, p: u64) -> bool {
        self.predicates
            .get(&p)
            .is_some_and(|closing| closing.closed || !closing.kernel)
    }

    /// Every predicate needs recomputing: a merge of `sameAs` classes rewrote facts, so
    /// what the module produced says nothing about what the rules added.
    pub(super) fn recompute_all(&mut self) {
        for closing in self.predicates.values_mut() {
            closing.dirty = true;
            closing.closed = false;
        }
    }

    /// The predicates whose transitivity is joined this round instead of closed again,
    /// with their rules: closed, and the joins cheaper than a closure. The joins read the
    /// delta without the module's output (its own family's part, [`Seg::DeltaNotBy`])
    /// against all facts, at either atom: the relation and that output are closed, so
    /// every new pair has an added one in it. They cost the delta's pairs' degrees (each
    /// pair's object's successors, its subject's predecessors); a closure costs about the
    /// relation's size. A clique built up from scratch would cost the joins its size
    /// cubed; a few pairs added to a hierarchy whose classes meet a hundred others each
    /// more than closing its 20,000 pairs again (`a_deep_hierarchy_derives_each_
    /// inherited_type_once`).
    /// Check the graph representation before dropping object orders or scheduling jobs.
    pub(super) fn prepare<R: Representation>(&mut self, store: &WorkingStore<R>) {
        for (&p, closing) in &mut self.predicates {
            if closing.kernel
                && let Some(relation) = store.relation(p)
                && relation.len_all() > closure_capacity() / 2
            {
                closing.kernel = closure_fits(&relation.pairs(), closure_capacity());
            }
        }
    }

    pub(super) fn joined<R: Representation>(
        &mut self,
        store: &WorkingStore<R>,
    ) -> Vec<(u64, &Rule)> {
        self.predicates
            .iter_mut()
            .filter_map(|(&p, closing)| {
                let relation = store.relation(p)?;
                let joins = !closing.kernel
                    || closing.closed
                        && closing.dirty
                        && relation.delta_degrees_below(relation.len_all());
                joins.then(|| {
                    closing.dirty = false;
                    (p, &closing.rule)
                })
            })
            .collect()
    }

    /// The closure facts of every predicate that needs it, not yet in `store`, each
    /// predicate's in its module family ([`Self::family`]); `None` if `stop` fired.
    pub(super) fn run<R: Representation>(
        &mut self,
        store: &WorkingStore<R>,
        representatives: Option<&Representatives>,
        stop: crate::eval::Stop<'_>,
    ) -> Option<Vec<Candidates<R>>> {
        self.produced.clear();
        self.read = 0;
        let mut out = Vec::new();
        for (&p, closing) in &mut self.predicates {
            if !closing.kernel || !std::mem::take(&mut closing.dirty) {
                continue;
            }
            let Some(relation) = store.relation(p) else {
                continue;
            };
            let mut pairs: Vec<(u64, u64)> = relation
                .pairs()
                .into_iter()
                .map(|q| (R::first(q), R::second(q)))
                .collect();
            self.read += pairs.len() as u64;
            if let Some(representatives) = representatives.filter(|r| !r.stale.is_empty()) {
                // Over current facts only (every term a representative), which closes them.
                let stale = &representatives.stale;
                if stale.contains(p) {
                    continue;
                }
                pairs.retain(|&(s, o)| !stale.contains(s) && !stale.contains(o));
            }
            let facts: Vec<Triple> = nrese_exec::graph::transitive_closure_until(&pairs, stop)?
                .into_iter()
                .filter(|&(s, o)| !relation.contains(s, o))
                .map(|(s, o)| [s, p, o])
                .collect();
            closing.closed = true;
            self.produced.insert(p, facts.len());
            out.push((Some(Self::family(p)), segments::<R>(facts)));
        }
        Some(out)
    }

    /// After a round: a predicate needs recomputing or joins if other rules added facts
    /// over it.
    pub(super) fn observe<R: Representation>(&mut self, store: &WorkingStore<R>) {
        for (p, closing) in &mut self.predicates {
            let added = store.relation(*p).map_or(0, Relation::<R>::delta_len);
            closing.dirty |= added > self.produced.get(p).copied().unwrap_or(0);
        }
    }
}

// The graph kernel has narrow node ranks and CSR offsets. Unsupported layouts run the
// same transitivity rule's joins; this is algorithm selection, never a failure/limit.
fn closure_capacity() -> usize {
    #[cfg(test)]
    {
        CLOSURE_CAPACITY.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        u32::MAX as usize
    }
}

fn closure_fits<R: Representation>(pairs: &[R], capacity: usize) -> bool {
    if pairs.len() > capacity {
        return false;
    }
    if pairs.len() <= capacity / 2 {
        return true;
    }
    let mut nodes: Vec<u64> = pairs
        .iter()
        .flat_map(|&p| [R::first(p), R::second(p)])
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes.len() <= capacity
}

#[cfg(test)]
thread_local! {
    pub(super) static CLOSURE_CAPACITY: std::cell::Cell<usize> = const {
        std::cell::Cell::new(u32::MAX as usize)
    };
}

#[cfg(test)]
mod graph_boundary_tests {
    use super::*;

    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            CLOSURE_CAPACITY.with(|cap| cap.set(self.0));
        }
    }

    #[test]
    fn node_ranks_and_csr_offsets_independently_select_exact_rules() {
        let cap = 7;
        // Eight nodes, seven chain edges: only node ranks exceed capacity.
        let nodes: Vec<[u64; 2]> = (0..7).map(|i| [i, i + 1]).collect();
        // Three nodes, eight edges: only the terminal CSR offset exceeds capacity.
        // The missing reachable edge makes a no-op fallback fail the oracle check.
        let edges: Vec<[u64; 2]> = (0..3)
            .flat_map(|s| (0..3).map(move |o| [s, o]))
            .filter(|pair| *pair != [0, 2])
            .collect();
        assert!(!closure_fits(&nodes, cap));
        assert!(!closure_fits(&edges, cap));
        assert!(closure_fits(&edges[..7], cap));
        let schema = Schema::owl(&mut crate::vocabulary::LocalVocabulary::default());
        let p = u64::MAX;
        for pairs in [nodes, edges] {
            let facts: Vec<Triple> = pairs.iter().map(|&[s, o]| [s, p, o]).collect();
            let rules = [crate::eval::transitivity(p)];
            let oracle = crate::naive::materialise(&facts, &rules, None);
            assert!(!facts.contains(&[0, p, 2]));
            assert!(oracle.derived.contains(&[0, p, 2]));
            let _reset = Reset(CLOSURE_CAPACITY.with(|capacity| capacity.replace(cap)));
            for capacity in [0, usize::MAX] {
                let result = materialise_test_capacity(
                    Input::Facts(facts.clone()),
                    &rules,
                    None,
                    &schema,
                    None,
                    Output::Facts,
                    NEVER,
                    capacity,
                )
                .unwrap();
                assert_eq!(
                    result
                        .derived
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>(),
                    oracle.derived
                );
                assert!(
                    result.counters.closure_pairs.iter().all(|&n| n == 0),
                    "the narrow graph kernel was never entered"
                );
                assert_eq!(result.counters.lookups_without_order, 0);
            }
        }
    }
}
