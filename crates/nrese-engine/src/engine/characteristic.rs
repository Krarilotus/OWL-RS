//! Characteristic sets of the default graph (Neumann and Moerkotte, ICDE 2011), for the
//! planner's estimates of star joins (XC4).
//!
//! A subject's characteristic set is the set of predicates it has statements with. Kept
//! per distinct set: how many subjects have exactly it, and how many statements each of
//! its predicates has among them. A star `?s p1 ?a . ?s p2 ?b` then has
//! `Σ_{S ⊇ {p1, p2}} |S| · (occ_S(p1) / |S|) · (occ_S(p2) / |S|)` solutions: only the
//! subjects that have both predicates count, where independence of the two patterns
//! assumes every subject might. On data with kinds of subjects (products, offers,
//! persons), that is the difference between an estimate of thousands and one of zero.
//!
//! Built by one scan of the default graph in subject order. Data whose subjects have too
//! many different sets (more than [`MAX_SETS`]) gets none: the planner then estimates as
//! before.
//!
//! **Characteristic pairs** (Gubichev and Neumann, *Exploiting the query structure for
//! efficient join ordering in SPARQL queries*, EDBT 2014) estimate chains: for each
//! predicate `p`, how many statements `s p o` link a subject of set `S1` to an object of
//! set `S2` (an object that is a subject itself; `NONE` otherwise). The chain
//! `?x p ?y . ?y q ?z` then counts only the links whose object has `q`
//! ([`CharacteristicSets::pair`]), where independence assumes every object might. A second
//! scan finds them ([`CharacteristicSets::with_pairs`]), within [`MAX_PAIR_SUBJECTS`] and
//! [`MAX_PAIRS`].

use std::collections::HashMap;

use crate::quad::EncodedQuad;

/// Distinct characteristic sets beyond which none are kept.
pub const MAX_SETS: usize = 1 << 17;

/// Subjects beyond which no characteristic pairs are kept (their set index is held, 12
/// bytes each, while the pairs are counted).
pub const MAX_PAIR_SUBJECTS: usize = 1 << 23;

/// Distinct (set, predicate, set) pairs beyond which none are kept.
pub const MAX_PAIRS: usize = 1 << 20;

/// The set of an object that is no subject (a literal, or a node without statements).
const NONE: u32 = u32::MAX;

/// The characteristic sets of a graph.
#[derive(Debug, Default)]
pub struct CharacteristicSets {
    sets: Vec<Set>,
    /// Per predicate, the sets that have it.
    by_predicate: HashMap<u64, Vec<u32>>,
    subjects: u64,
    /// Each subject's set, sorted by subject; while the pairs are being counted.
    subject_sets: Vec<(u64, u32)>,
    /// Per predicate, (subject set, object set, statements); `None` if not counted.
    pairs: Option<Links>,
}

/// Characteristic pairs by predicate: (subject set, object set, statements).
type Links = HashMap<u64, Vec<(u32, u32, u64)>>;

/// Statements per (predicate, subject set, object set), while pairs are counted.
type PairCounts = HashMap<(u64, u32, u32), u64>;

#[derive(Debug)]
struct Set {
    /// Its predicates (raw ids, sorted), each with its statements among the set's subjects.
    predicates: Box<[(u64, u64)]>,
    subjects: u64,
}

impl CharacteristicSets {
    /// The sets of `quads`, which come sorted by subject and then predicate (as `Gspo`
    /// scans them); `None` past [`MAX_SETS`] distinct sets.
    #[cfg(test)]
    pub(crate) fn build(quads: impl Iterator<Item = EncodedQuad>) -> Option<Self> {
        Self::merge(vec![Partial::of(quads)?])
    }

    /// The sets of the parts of a graph, each scanned by subject (the parts of [`Partial`]
    /// in subject order): `None` past [`MAX_SETS`] distinct sets.
    pub(crate) fn merge(parts: Vec<Partial>) -> Option<Self> {
        // Each distinct set by its index, in the order the parts found them.
        let mut index: HashMap<Box<[u64]>, u32> = HashMap::new();
        let mut sets = Self::default();
        let mut occurrences: Vec<Vec<u64>> = Vec::new();
        let mut counts: Vec<u64> = Vec::new();
        let mut keys: Vec<Box<[u64]>> = Vec::new();
        let keep_subjects = parts.iter().map(|p| p.subject_sets.len()).sum::<usize>()
            < MAX_PAIR_SUBJECTS
            && parts.iter().all(|p| p.kept_subjects);
        let mut subject_sets = Vec::new();
        for part in parts {
            let mut local_to_global = Vec::with_capacity(part.found.len());
            for (key, subjects, occurs) in part.found {
                let at = match index.get(&key) {
                    Some(&at) => at,
                    None => {
                        if keys.len() >= MAX_SETS {
                            return None;
                        }
                        let at = keys.len() as u32;
                        index.insert(key.clone(), at);
                        occurrences.push(vec![0; key.len()]);
                        counts.push(0);
                        keys.push(key);
                        at
                    }
                };
                counts[at as usize] += subjects;
                for (total, n) in occurrences[at as usize].iter_mut().zip(occurs) {
                    *total += n;
                }
                local_to_global.push(at);
            }
            if keep_subjects {
                subject_sets.extend(
                    part.subject_sets
                        .into_iter()
                        .map(|(s, local)| (s, local_to_global[local as usize])),
                );
            }
        }
        for (at, ((predicates, subjects), occurs)) in
            keys.into_iter().zip(counts).zip(occurrences).enumerate()
        {
            for &p in predicates.iter() {
                sets.by_predicate.entry(p).or_default().push(at as u32);
            }
            sets.subjects += subjects;
            sets.sets.push(Set {
                predicates: predicates.iter().copied().zip(occurs).collect(),
                subjects,
            });
        }
        sets.subject_sets = subject_sets;
        Some(sets)
    }

    /// These sets with their characteristic pairs counted from `quads` (the same graph
    /// again, in any order); without pairs if there are too many subjects or pairs.
    #[cfg(test)]
    pub(crate) fn with_pairs(self, quads: impl Iterator<Item = EncodedQuad> + Send) -> Self {
        self.with_pairs_of(vec![quads])
    }

    /// [`Self::with_pairs`] over parts of the graph, counted on every core and summed.
    pub(crate) fn with_pairs_of<I>(mut self, parts: Vec<I>) -> Self
    where
        I: Iterator<Item = EncodedQuad> + Send,
    {
        use rayon::prelude::*;
        let subject_sets = std::mem::take(&mut self.subject_sets);
        if subject_sets.is_empty() {
            return self;
        }
        let set_of = |id: u64| {
            subject_sets
                .binary_search_by_key(&id, |&(s, _)| s)
                .map_or(NONE, |at| subject_sets[at].1)
        };
        let counted: Option<Vec<PairCounts>> = parts
            .into_par_iter()
            .map(|quads| {
                let mut counts: PairCounts = HashMap::new();
                let mut last: Option<(u64, u32)> = None;
                for quad in quads {
                    let s = quad.subject.raw();
                    let from = match last {
                        Some((subject, set)) if subject == s => set,
                        _ => {
                            let set = set_of(s);
                            last = Some((s, set));
                            set
                        }
                    };
                    let key = (quad.predicate.raw(), from, set_of(quad.object.raw()));
                    if !counts.contains_key(&key) && counts.len() >= MAX_PAIRS {
                        return None;
                    }
                    *counts.entry(key).or_insert(0) += 1;
                }
                Some(counts)
            })
            .collect();
        let Some(counted) = counted else {
            return self;
        };
        let mut counts: PairCounts = HashMap::new();
        for part in counted {
            for (key, n) in part {
                if !counts.contains_key(&key) && counts.len() >= MAX_PAIRS {
                    return self;
                }
                *counts.entry(key).or_insert(0) += n;
            }
        }
        let mut pairs: Links = HashMap::new();
        for ((p, from, to), n) in counts {
            pairs.entry(p).or_default().push((from, to, n));
        }
        for links in pairs.values_mut() {
            links.sort_unstable();
        }
        self.pairs = Some(pairs);
        self
    }

    /// Writes the sets and pairs (not the subjects' sets, which only counting pairs needs).
    pub(crate) fn write<W: std::io::Write>(
        &self,
        w: &mut crate::term::derived::Writer<W>,
    ) -> std::io::Result<()> {
        w.u32(FILE_VERSION)?;
        w.u64(self.subjects)?;
        w.u64(self.sets.len() as u64)?;
        for set in &self.sets {
            w.u64(set.subjects)?;
            w.u64(set.predicates.len() as u64)?;
            for &(p, n) in set.predicates.iter() {
                w.u64(p)?;
                w.u64(n)?;
            }
        }
        match &self.pairs {
            None => w.u64(u64::MAX)?,
            Some(pairs) => {
                w.u64(pairs.len() as u64)?;
                let mut predicates: Vec<&u64> = pairs.keys().collect();
                predicates.sort_unstable();
                for p in predicates {
                    let links = &pairs[p];
                    w.u64(*p)?;
                    w.u64(links.len() as u64)?;
                    for &(from, to, n) in links {
                        w.u32(from)?;
                        w.u32(to)?;
                        w.u64(n)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads what [`Self::write`] wrote; `None` if it is malformed or of another version.
    pub(crate) fn read(r: &mut crate::term::derived::Reader<'_>) -> Option<Self> {
        if r.u32()? != FILE_VERSION {
            return None;
        }
        let mut sets = Self {
            subjects: r.u64()?,
            ..Self::default()
        };
        let count = r.len(16)?;
        for at in 0..count {
            let subjects = r.u64()?;
            let n = r.len(16)?;
            let mut predicates = Vec::with_capacity(n);
            for _ in 0..n {
                let p = r.u64()?;
                predicates.push((p, r.u64()?));
                sets.by_predicate.entry(p).or_default().push(at as u32);
            }
            sets.sets.push(Set {
                predicates: predicates.into_boxed_slice(),
                subjects,
            });
        }
        let pair_predicates = r.u64()?;
        if pair_predicates != u64::MAX {
            let mut pairs: Links = HashMap::new();
            for _ in 0..pair_predicates {
                let p = r.u64()?;
                let n = r.len(16)?;
                let mut links = Vec::with_capacity(n);
                for _ in 0..n {
                    links.push((r.u32()?, r.u32()?, r.u64()?));
                }
                pairs.insert(p, links);
            }
            sets.pairs = Some(pairs);
        }
        Some(sets)
    }

    /// Whether characteristic pairs were counted.
    pub fn has_pairs(&self) -> bool {
        self.pairs.is_some()
    }

    /// The estimated solutions of the chain `?x p ?y` with `?x` also having the
    /// predicates `from` and `?y` having `to` (every other object free): over the links
    /// `p` makes from a set with `from` to a set with `to`, the links times each other
    /// predicate's statements per subject on either side. With `to` empty, links to
    /// objects that are no subject count too. `None` without pairs.
    pub fn pair(&self, from: &[u64], p: u64, to: &[u64]) -> Option<f64> {
        let pairs = self.pairs.as_ref()?;
        let Some(links) = pairs.get(&p) else {
            return Some(0.0);
        };
        // Statements per subject of each predicate, over a set that has them all.
        let per_subject = |set: u32, predicates: &[u64]| -> Option<f64> {
            if predicates.is_empty() {
                return Some(1.0);
            }
            let set = self.sets.get(set as usize)?;
            let subjects = set.subjects as f64;
            predicates.iter().try_fold(1.0, |rows, q| {
                let at = set.predicates.binary_search_by_key(q, |&(r, _)| r).ok()?;
                Some(rows * set.predicates[at].1 as f64 / subjects)
            })
        };
        Some(
            links
                .iter()
                .filter_map(|&(s1, s2, n)| {
                    let left = per_subject(s1, from)?;
                    let right = match (to.is_empty(), s2) {
                        (true, _) => 1.0,
                        (false, NONE) => return None,
                        (false, s2) => per_subject(s2, to)?,
                    };
                    Some(n as f64 * left * right)
                })
                .sum(),
        )
    }

    /// The estimated solutions of a star on one subject with these predicates (raw ids;
    /// one may repeat, for two patterns with it), every object free: over the sets that
    /// have them all, the subjects times each predicate's statements per subject.
    pub fn star(&self, predicates: &[u64]) -> f64 {
        let Some(rarest) = predicates
            .iter()
            .map(|p| self.by_predicate.get(p).map_or(&[][..], Vec::as_slice))
            .min_by_key(|sets| sets.len())
        else {
            return self.subjects as f64;
        };
        rarest
            .iter()
            .map(|&i| &self.sets[i as usize])
            .filter_map(|set| {
                let subjects = set.subjects as f64;
                predicates.iter().try_fold(subjects, |rows, p| {
                    let at = set.predicates.binary_search_by_key(p, |&(q, _)| q).ok()?;
                    Some(rows * set.predicates[at].1 as f64 / subjects)
                })
            })
            .sum()
    }

    /// The number of distinct sets.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The number of subjects.
    pub fn subjects(&self) -> u64 {
        self.subjects
    }
}

/// The version of [`CharacteristicSets::write`]'s bytes.
const FILE_VERSION: u32 = 1;

/// The sets of one part of a graph scanned by subject ([`CharacteristicSets::merge`]): in
/// the order found, each with its subjects and its predicates' statements, and each
/// subject's set (its index here), while there aren't too many subjects to keep.
pub(crate) struct Partial {
    found: Vec<(Box<[u64]>, u64, Vec<u64>)>,
    subject_sets: Vec<(u64, u32)>,
    kept_subjects: bool,
}

impl Partial {
    /// The sets of `quads`, sorted by subject and then predicate; `None` past
    /// [`MAX_SETS`] distinct sets.
    pub(crate) fn of(quads: impl Iterator<Item = EncodedQuad>) -> Option<Self> {
        let mut index: HashMap<Box<[u64]>, u32> = HashMap::new();
        let mut part = Partial {
            found: Vec::new(),
            subject_sets: Vec::new(),
            kept_subjects: true,
        };
        let mut current: Vec<(u64, u64)> = Vec::new();
        let mut subject = None;
        let mut flush = |part: &mut Partial,
                         current: &mut Vec<(u64, u64)>,
                         subject: Option<u64>|
         -> Option<()> {
            if current.is_empty() {
                return Some(());
            }
            let key: Box<[u64]> = current.iter().map(|&(p, _)| p).collect();
            let at = match index.get(&key) {
                Some(&at) => at,
                None => {
                    if part.found.len() >= MAX_SETS {
                        return None;
                    }
                    let at = part.found.len() as u32;
                    part.found.push((key.clone(), 0, vec![0; current.len()]));
                    index.insert(key, at);
                    at
                }
            };
            let entry = &mut part.found[at as usize];
            entry.1 += 1;
            for (total, &(_, n)) in entry.2.iter_mut().zip(current.iter()) {
                *total += n;
            }
            if part.kept_subjects {
                if part.subject_sets.len() >= MAX_PAIR_SUBJECTS {
                    part.kept_subjects = false;
                    part.subject_sets = Vec::new();
                } else if let Some(s) = subject {
                    part.subject_sets.push((s, at));
                }
            }
            current.clear();
            Some(())
        };
        for quad in quads {
            let (s, p) = (quad.subject.raw(), quad.predicate.raw());
            if subject != Some(s) {
                flush(&mut part, &mut current, subject)?;
                subject = Some(s);
            }
            match current.last_mut() {
                Some((last, n)) if *last == p => *n += 1,
                _ => current.push((p, 1)),
            }
        }
        flush(&mut part, &mut current, subject)?;
        Some(part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::TermId;

    fn quad(s: u64, p: u64, o: u64) -> EncodedQuad {
        let id = |raw| TermId::from_raw(raw);
        EncodedQuad::new(id(s), id(p), id(o), TermId::DEFAULT_GRAPH)
    }

    #[test]
    fn stars_count_only_subjects_with_every_predicate() {
        // 10 products with a label and a price (two labels each), 20 persons with a
        // label and a name.
        let (label, price, name) = (100, 101, 102);
        let mut quads = Vec::new();
        for s in 0..10 {
            quads.push(quad(s, label, 1000));
            quads.push(quad(s, label, 1001));
            quads.push(quad(s, price, 2000));
        }
        for s in 10..30 {
            quads.push(quad(s, label, 1000));
            quads.push(quad(s, name, 3000));
        }
        quads.sort_by_key(|q| (q.subject.raw(), q.predicate.raw(), q.object.raw()));
        let sets = CharacteristicSets::build(quads.into_iter()).unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets.subjects(), 30);
        assert_eq!(sets.star(&[price, name]), 0.0, "no subject has both");
        assert_eq!(
            sets.star(&[label, price]),
            20.0,
            "10 products, 2 labels each"
        );
        assert_eq!(sets.star(&[label]), 40.0, "every label statement");
        assert_eq!(
            sets.star(&[label, label]),
            60.0,
            "4 label pairs per product, 1 per person"
        );
        assert_eq!(sets.star(&[999]), 0.0, "an unknown predicate");
    }

    #[test]
    fn pairs_count_only_links_to_objects_with_the_predicates() {
        // 10 people each know 2 people; 5 of the 10 work somewhere; 3 companies (no
        // `knows`) are known by person 0.
        let (knows, works, name) = (100, 101, 102);
        let mut quads = Vec::new();
        for s in 0..10 {
            quads.push(quad(s, knows, (s + 1) % 10));
            quads.push(quad(s, knows, (s + 2) % 10));
            quads.push(quad(s, name, 1000 + s));
            if s % 2 == 0 {
                quads.push(quad(s, works, 50));
            }
        }
        for c in 50..53 {
            quads.push(quad(c, name, 2000 + c));
            quads.push(quad(0, knows, c));
        }
        quads.sort_by_key(|q| (q.subject.raw(), q.predicate.raw(), q.object.raw()));
        let sets = CharacteristicSets::build(quads.clone().into_iter())
            .unwrap()
            .with_pairs(quads.into_iter());
        assert!(sets.has_pairs());
        // Every `knows` link: 20 between people and 3 to companies.
        assert_eq!(sets.pair(&[], knows, &[]), Some(23.0));
        // Links to someone who works: each person is known by 2, 5 people work.
        assert_eq!(sets.pair(&[], knows, &[works]), Some(10.0));
        // Links to anyone with a name: people and companies.
        assert_eq!(sets.pair(&[], knows, &[name]), Some(23.0));
        // From someone who works to someone who works: 0 knows 1 and 2, 2 knows 3 and 4...
        // every even person knows one odd and one even one.
        assert_eq!(sets.pair(&[works], knows, &[works]), Some(5.0));
        // Unknown predicates link nothing.
        assert_eq!(sets.pair(&[], 999, &[]), Some(0.0));
        // Independence would say 23 links × 5/13 workers among the subjects: about 9.
        let unpaired = CharacteristicSets::build(std::iter::empty()).unwrap();
        assert_eq!(unpaired.pair(&[], knows, &[]), None);
    }

    #[test]
    fn too_many_sets_give_none() {
        // Every subject its own set: one predicate of its own.
        let quads = (0..MAX_SETS as u64 + 1).map(|s| quad(s, 10_000_000 + s, 1));
        assert!(CharacteristicSets::build(quads).is_none());
    }
}
