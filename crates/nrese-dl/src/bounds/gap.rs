//! The gap between the bounds (docs/design/owl2-dl.md §8, the query path's step 1): for
//! an atomic query, L's answers are certain, U1's contain every certain one (if the
//! ontology is consistent), and where the two agree the answer is exact. Where they
//! don't, the gap `U1 \ L` holds the candidates the DL engine has to check.
//!
//! Only answers count: facts over named terms (never U1's Skolem constants or fresh
//! classes), with a class of the ontology (or `owl:Thing`), one of its properties, or
//! `owl:sameAs` as predicate, and an individual as subject (and as object of
//! `owl:sameAs`): never a literal, which OWL 2 RL's closure can equate with another
//! (`prp-key`, `prp-fp` over data values) where the Direct Semantics has no assertion.

use std::collections::BTreeMap;

use hashbrown::{HashMap, HashSet};
use nrese_owl::{Term, TermKind, Terms};

use super::program::Program;

/// A fact: subject, predicate and object ids.
pub type Triple = [Term; 3];

/// An atomic query: the instances of a class, the pairs of a property, or one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtomicQuery {
    /// `C(?x)`.
    Instances(Term),
    /// `R(?x, ?y)` (an object or data property, or `owl:sameAs`).
    Pairs(Term),
    /// `C(a)` as `(a rdf:type C)`, or `R(a, b)`.
    Fact(Triple),
}

/// The answer the bounds give to an atomic query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// L and U1 agree: these are the certain answers (the facts that answer it).
    Exact(Vec<Triple>),
    /// They don't: `certain` are L's answers, `candidates` the rest of U1's, each to be
    /// checked by an exact service; `open` the subjects whose values of a data property
    /// U1 doesn't enumerate ([`Bounds::is_open`]), whose values an exact service has to
    /// find.
    Gap {
        certain: Vec<Triple>,
        candidates: Vec<Triple>,
        open: Vec<Term>,
    },
}

/// One predicate's bounds: a class (`rdf:type` facts) or a property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateGap {
    /// The class, for class memberships; else the property.
    pub predicate: Term,
    pub class: bool,
    pub lower: usize,
    pub upper: usize,
    /// `|U1 \ L|` on the predicate.
    pub gap: usize,
    /// `|L \ U1|`: must be 0 (L ⊆ U1).
    pub lower_only: usize,
    /// Subjects with values U1 doesn't enumerate ([`Bounds::is_open`]).
    pub open: usize,
}

/// The gap over every predicate, and how much the bounds settle alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GapReport {
    pub predicates: Vec<PredicateGap>,
    pub lower: usize,
    pub upper: usize,
    pub gap: usize,
    pub lower_only: usize,
    /// Predicate queries (`C(?x)`, `R(?x, ?y)`) whose bounds agree.
    pub exact_queries: usize,
    /// Subjects with values U1 doesn't enumerate, over every data property.
    pub open: usize,
    /// Terms a clash was derived at in U1.
    pub clashes: usize,
    /// Whether U1 proves the ontology consistent ([`Bounds::consistent`]).
    pub consistent: bool,
}

/// A predicate key: `(rdf:type, class)` for class memberships, `(property, 0)` else.
type Key = (Term, Term);

/// The two closures, cut down to their answers and grouped by predicate.
#[derive(Debug, Default)]
pub struct Bounds {
    rdf_type: Term,
    lower: HashMap<Key, Vec<Triple>>,
    upper: HashMap<Key, HashSet<Triple>>,
    clashes: Vec<Term>,
    /// Per data property, the subjects U1 gives a Skolem value: one the datatypes would
    /// fix, which U1 doesn't enumerate.
    open: HashMap<Term, HashSet<Term>>,
    /// [`Program::proves_consistency`].
    proves: bool,
}

impl Bounds {
    /// The bounds from L's closure and U1's (each the input with what was derived;
    /// `program` is the U1 the upper one was computed with; `terms` tells literals).
    pub fn new(
        program: &Program,
        terms: &dyn Terms,
        lower: impl IntoIterator<Item = Triple>,
        upper: impl IntoIterator<Item = Triple>,
    ) -> Self {
        let names = &program.names;
        let mut classes: HashSet<Term> = program.signature.classes.iter().copied().collect();
        classes.insert(names.thing);
        let mut properties: HashSet<Term> = program
            .signature
            .object_properties
            .iter()
            .chain(&program.signature.data_properties)
            .copied()
            .collect();
        properties.insert(names.same_as);
        let data: HashSet<Term> = program.signature.data_properties.iter().copied().collect();
        let literal = |t: Term| terms.kind(t) == TermKind::Literal;
        let key = |[s, p, o]: Triple| -> Option<Key> {
            if program.is_internal(s) || program.is_internal(o) {
                return None;
            }
            if literal(s) || (p == names.same_as && literal(o)) {
                return None;
            }
            if p == names.rdf_type {
                return classes.contains(&o).then_some((p, o));
            }
            properties.contains(&p).then_some((p, 0))
        };
        let mut bounds = Self {
            rdf_type: names.rdf_type,
            proves: program.proves_consistency(),
            ..Self::default()
        };
        for t in lower {
            if let Some(k) = key(t) {
                bounds.lower.entry(k).or_default().push(t);
            }
        }
        for list in bounds.lower.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        for t in upper {
            if t[1] == names.clash && t[2] == names.clash {
                bounds.clashes.push(t[0]);
            } else if data.contains(&t[1])
                && program.is_internal(t[2])
                && !program.is_internal(t[0])
                && !literal(t[0])
            {
                bounds.open.entry(t[1]).or_default().insert(t[0]);
            } else if let Some(k) = key(t) {
                bounds.upper.entry(k).or_default().insert(t);
            }
        }
        bounds.clashes.sort_unstable();
        bounds.clashes.dedup();
        bounds
    }

    /// Where U1 derived a clash (`⊥` fired): the ontology may be inconsistent.
    pub fn clashes(&self) -> &[Term] {
        &self.clashes
    }

    /// Whether U1 proves the ontology consistent: no clash, and U1 checks every `⊥`
    /// (PAGOdA, Theorem 5.5 (i); [`Program::proves_consistency`]). False says nothing.
    pub fn consistent(&self) -> bool {
        self.clashes.is_empty() && self.proves
    }

    /// Whether U1 leaves the values of data property `p` at `subject` open: it derived a
    /// Skolem value there, which stands for values the datatypes would fix (a range of
    /// 128 bytes that are also unsigned ints, with cardinality 128, entails each), so
    /// it holds every `p`-value of `subject`.
    pub fn is_open(&self, subject: Term, p: Term) -> bool {
        self.open.get(&p).is_some_and(|s| s.contains(&subject))
    }

    /// Whether U1 holds `t`: as a fact, or by an open value.
    fn covers(&self, k: &Key, t: &Triple) -> bool {
        self.upper.get(k).is_some_and(|u| u.contains(t)) || self.is_open(t[0], t[1])
    }

    /// L's answers U1 lacks: must be none.
    pub fn lower_not_in_upper(&self) -> Vec<Triple> {
        let mut out: Vec<Triple> = self
            .lower
            .iter()
            .flat_map(|(k, list)| list.iter().filter(move |t| !self.covers(k, t)).copied())
            .collect();
        out.sort_unstable();
        out
    }

    fn key_of(&self, query: AtomicQuery) -> Key {
        match query {
            AtomicQuery::Instances(class) => (self.rdf_type, class),
            AtomicQuery::Pairs(p) => (p, 0),
            AtomicQuery::Fact([_, p, o]) if p == self.rdf_type => (p, o),
            AtomicQuery::Fact([_, p, _]) => (p, 0),
        }
    }

    /// The bounds' answer to `query`: exact, or L's answers and the candidates.
    pub fn answer(&self, query: AtomicQuery) -> Answer {
        let key = self.key_of(query);
        let matches = |t: &Triple| match query {
            AtomicQuery::Fact(f) => *t == f,
            _ => true,
        };
        let certain: Vec<Triple> = self
            .lower
            .get(&key)
            .map(|l| l.iter().filter(|t| matches(t)).copied().collect())
            .unwrap_or_default();
        let lower: HashSet<Triple> = certain.iter().copied().collect();
        let mut candidates: Vec<Triple> = self
            .upper
            .get(&key)
            .map(|u| {
                u.iter()
                    .filter(|t| matches(t) && !lower.contains(*t))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        let mut open: Vec<Term> = match query {
            AtomicQuery::Pairs(p) => self
                .open
                .get(&p)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default(),
            AtomicQuery::Fact([s, p, _]) if self.is_open(s, p) => vec![s],
            _ => Vec::new(),
        };
        if candidates.is_empty() && open.is_empty() {
            return Answer::Exact(certain);
        }
        candidates.sort_unstable();
        open.sort_unstable();
        Answer::Gap {
            certain,
            candidates,
            open,
        }
    }

    /// The gap per predicate, sorted by its size (largest first).
    pub fn report(&self) -> GapReport {
        let mut keys: BTreeMap<Key, ()> = BTreeMap::new();
        keys.extend(self.lower.keys().map(|&k| (k, ())));
        keys.extend(self.upper.keys().map(|&k| (k, ())));
        keys.extend(self.open.keys().map(|&p| ((p, 0), ())));
        let mut report = GapReport {
            clashes: self.clashes.len(),
            consistent: self.consistent(),
            ..GapReport::default()
        };
        for (key, ()) in keys {
            let empty = HashSet::new();
            let upper = self.upper.get(&key).unwrap_or(&empty);
            let lower = self.lower.get(&key).map_or(&[][..], Vec::as_slice);
            let lower_only = lower.iter().filter(|t| !self.covers(&key, t)).count();
            // L's facts U1 holds as facts (an open value holds the others).
            let both = lower.iter().filter(|t| upper.contains(*t)).count();
            let gap = upper.len() - both;
            let class = key.0 == self.rdf_type;
            let open = if class {
                0
            } else {
                self.open.get(&key.0).map_or(0, HashSet::len)
            };
            report.predicates.push(PredicateGap {
                predicate: if class { key.1 } else { key.0 },
                class,
                lower: lower.len(),
                upper: upper.len(),
                gap,
                lower_only,
                open,
            });
            report.lower += lower.len();
            report.upper += upper.len();
            report.gap += gap;
            report.lower_only += lower_only;
            report.open += open;
            report.exact_queries += usize::from(gap == 0 && open == 0);
        }
        report
            .predicates
            .sort_by(|a, b| b.gap.cmp(&a.gap).then(a.predicate.cmp(&b.predicate)));
        report
    }
}
