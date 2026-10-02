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

use std::collections::HashMap;

use crate::quad::EncodedQuad;

/// Distinct characteristic sets beyond which none are kept.
pub const MAX_SETS: usize = 1 << 17;

/// The characteristic sets of a graph.
#[derive(Debug, Default)]
pub struct CharacteristicSets {
    sets: Vec<Set>,
    /// Per predicate, the sets that have it.
    by_predicate: HashMap<u64, Vec<u32>>,
    subjects: u64,
}

#[derive(Debug)]
struct Set {
    /// Its predicates (raw ids, sorted), each with its statements among the set's subjects.
    predicates: Box<[(u64, u64)]>,
    subjects: u64,
}

impl CharacteristicSets {
    /// The sets of `quads`, which come sorted by subject and then predicate (as `Gspo`
    /// scans them); `None` past [`MAX_SETS`] distinct sets.
    pub(crate) fn build(quads: impl Iterator<Item = EncodedQuad>) -> Option<Self> {
        let mut found: HashMap<Box<[u64]>, (u64, Vec<u64>)> = HashMap::new();
        let mut current: Vec<(u64, u64)> = Vec::new();
        let mut subject = None;
        let mut flush = |current: &mut Vec<(u64, u64)>| -> Option<()> {
            if current.is_empty() {
                return Some(());
            }
            let key: Box<[u64]> = current.iter().map(|&(p, _)| p).collect();
            if !found.contains_key(&key) && found.len() >= MAX_SETS {
                return None;
            }
            let entry = found
                .entry(key)
                .or_insert_with(|| (0, vec![0; current.len()]));
            entry.0 += 1;
            for (total, &(_, n)) in entry.1.iter_mut().zip(current.iter()) {
                *total += n;
            }
            current.clear();
            Some(())
        };
        for quad in quads {
            let (s, p) = (quad.subject.raw(), quad.predicate.raw());
            if subject != Some(s) {
                flush(&mut current)?;
                subject = Some(s);
            }
            match current.last_mut() {
                Some((last, n)) if *last == p => *n += 1,
                _ => current.push((p, 1)),
            }
        }
        flush(&mut current)?;
        let mut sets = Self::default();
        for (predicates, (subjects, occurrences)) in found {
            let index = sets.sets.len() as u32;
            for &p in predicates.iter() {
                sets.by_predicate.entry(p).or_default().push(index);
            }
            sets.subjects += subjects;
            sets.sets.push(Set {
                predicates: predicates.iter().copied().zip(occurrences).collect(),
                subjects,
            });
        }
        Some(sets)
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
    fn too_many_sets_give_none() {
        // Every subject its own set: one predicate of its own.
        let quads = (0..MAX_SETS as u64 + 1).map(|s| quad(s, 10_000_000 + s, 1));
        assert!(CharacteristicSets::build(quads).is_none());
    }
}
