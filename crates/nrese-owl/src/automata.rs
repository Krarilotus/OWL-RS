//! Automata over roles for universal restrictions on non-simple roles (Horrocks and
//! Sattler, AIJ 2004): built by the normaliser (`Normaliser::nfa`), clausified a state per
//! fresh name and a clause per transition, for each filler.
//!
//! [`Nfa::minimal`] makes one smaller: ε-free, deterministic, minimal, trimmed. The
//! automaton of a transitive role over a large hierarchy of transitive subroles is spliced
//! from theirs, a few states each with ε-moves between them; minimal, it is a handful of
//! states (ore_ont_1066: 2,865 clauses per universal before). Its transitions carry the
//! union of the axioms of the paths they stand for: justifications that remain sound but
//! may name more inclusions than needed, so it is for callers that don't read proofs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::ObjProp;

/// An automaton over roles: states, the start, the end, transitions (`None`: ε), each
/// with the role-inclusion axioms it comes from (none for the role's own edge).
#[derive(Debug, Clone)]
pub(crate) struct Nfa {
    pub(crate) states: u32,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) edges: Vec<(u32, Option<ObjProp>, u32, Vec<usize>)>,
}

/// Past this many states a determinised automaton isn't kept.
const MAX_STATES: usize = 4096;

impl Nfa {
    /// The automaton of the inverse role: every path reversed.
    pub(crate) fn inverse(&self) -> Nfa {
        Nfa {
            states: self.states,
            start: self.end,
            end: self.start,
            edges: self
                .edges
                .iter()
                .map(|(a, label, b, axioms)| (*b, label.map(ObjProp::inverse), *a, axioms.clone()))
                .collect(),
        }
    }

    /// The minimal deterministic automaton of the same language (itself where that isn't
    /// smaller), with the end reached by ε from each accepting state.
    pub(crate) fn minimal(&self) -> Nfa {
        let n = self.states as usize;
        // ε-closures, with the axioms of the ε-moves on the way (all of them, unioned).
        let mut eps: Vec<Vec<(u32, &Vec<usize>)>> = vec![Vec::new(); n];
        let mut moves: Vec<Vec<(ObjProp, u32, &Vec<usize>)>> = vec![Vec::new(); n];
        for (a, label, b, axioms) in &self.edges {
            match label {
                None => eps[*a as usize].push((*b, axioms)),
                Some(l) => moves[*a as usize].push((*l, *b, axioms)),
            }
        }
        // ε-closures on demand (only targets of moves and the start need one), each with
        // the axioms of its ε-moves.
        let mut closures: Vec<Option<(Vec<u32>, Vec<usize>)>> = vec![None; n];
        let mut mark = vec![false; n];
        let mut closure_of = |s: u32, closures: &mut Vec<Option<(Vec<u32>, Vec<usize>)>>| {
            if closures[s as usize].is_none() {
                let mut states = vec![s];
                let mut axioms = Vec::new();
                mark[s as usize] = true;
                let mut at = 0;
                while at < states.len() {
                    let x = states[at];
                    at += 1;
                    for &(y, ax) in &eps[x as usize] {
                        axioms.extend_from_slice(ax);
                        if !std::mem::replace(&mut mark[y as usize], true) {
                            states.push(y);
                        }
                    }
                }
                for &x in &states {
                    mark[x as usize] = false;
                }
                states.sort_unstable();
                axioms.sort_unstable();
                axioms.dedup();
                closures[s as usize] = Some((states, axioms));
            }
        };
        // Subsets.
        type Set = Vec<u32>;
        let mut seen = vec![false; n];
        let mut close = |seeds: &BTreeSet<u32>| -> (Set, BTreeSet<usize>) {
            let mut states = Vec::new();
            let mut axioms = BTreeSet::new();
            for &s in seeds {
                closure_of(s, &mut closures);
                let (st, ax) = closures[s as usize]
                    .as_ref()
                    .unwrap_or_else(|| unreachable!());
                for &x in st {
                    if !std::mem::replace(&mut seen[x as usize], true) {
                        states.push(x);
                    }
                }
                axioms.extend(ax.iter().copied());
            }
            for &x in &states {
                seen[x as usize] = false;
            }
            states.sort_unstable();
            (states, axioms)
        };
        let (first, _) = close(&BTreeSet::from([self.start]));
        let mut ids: HashMap<Set, u32> = HashMap::from([(first.clone(), 0)]);
        let mut sets = vec![first];
        // Per DFA state: label ↦ (target, axioms).
        let mut delta: Vec<BTreeMap<ObjProp, (u32, BTreeSet<usize>)>> = Vec::new();
        let mut k = 0;
        while k < sets.len() {
            if sets.len() > MAX_STATES.min(4 * n.max(1)) {
                return self.clone();
            }
            let mut by_label: BTreeMap<ObjProp, (BTreeSet<u32>, BTreeSet<usize>)> = BTreeMap::new();
            for &p in &sets[k] {
                for &(l, q, ax) in &moves[p as usize] {
                    let e = by_label.entry(l).or_default();
                    e.0.insert(q);
                    e.1.extend(ax.iter().copied());
                }
            }
            let mut row = BTreeMap::new();
            for (l, (seeds, mut axioms)) in by_label {
                let (target, closed) = close(&seeds);
                axioms.extend(closed);
                let len = ids.len() as u32;
                let id = *ids.entry(target.clone()).or_insert_with(|| {
                    sets.push(target);
                    len
                });
                row.insert(l, (id, axioms));
            }
            delta.push(row);
            k += 1;
        }
        let accepting: Vec<bool> = sets
            .iter()
            .map(|s| s.binary_search(&self.end).is_ok())
            .collect();
        // Trim: only states that reach an accepting one.
        let m = sets.len();
        let mut live = accepting.clone();
        let mut changed = true;
        while changed {
            changed = false;
            for q in 0..m {
                if !live[q] && delta[q].values().any(|(t, _)| live[*t as usize]) {
                    live[q] = true;
                    changed = true;
                }
            }
        }
        if !live[0] {
            // The empty language: the start alone, never reaching the end.
            return Nfa {
                states: 2,
                start: 0,
                end: 1,
                edges: Vec::new(),
            };
        }
        // Moore's refinement over the live states (a missing or dead move is one class).
        let mut class: Vec<u32> = (0..m).map(|q| u32::from(accepting[q])).collect();
        let mut count = 0;
        loop {
            let mut ids: HashMap<(u32, Vec<(ObjProp, u32)>), u32> = HashMap::new();
            let mut next = vec![u32::MAX; m];
            for q in (0..m).filter(|&q| live[q]) {
                let row: Vec<(ObjProp, u32)> = delta[q]
                    .iter()
                    .filter(|(_, (t, _))| live[*t as usize])
                    .map(|(l, (t, _))| (*l, class[*t as usize]))
                    .collect();
                let len = ids.len() as u32;
                next[q] = *ids.entry((class[q], row)).or_insert(len);
            }
            let refined = ids.len();
            class = next;
            if refined == count {
                break;
            }
            count = refined;
        }
        if count + 1 >= n {
            return self.clone();
        }
        // The quotient, the start's class first, and a fresh end.
        let mut order: HashMap<u32, u32> = HashMap::from([(class[0], 0)]);
        for q in (0..m).filter(|&q| live[q]) {
            let len = order.len() as u32;
            order.entry(class[q]).or_insert(len);
        }
        let end = order.len() as u32;
        let mut edges: BTreeMap<(u32, Option<ObjProp>, u32), BTreeSet<usize>> = BTreeMap::new();
        for q in (0..m).filter(|&q| live[q]) {
            let from = order[&class[q]];
            if accepting[q] {
                edges.entry((from, None, end)).or_default();
            }
            for (l, (t, axioms)) in &delta[q] {
                if live[*t as usize] {
                    let to = order[&class[*t as usize]];
                    edges
                        .entry((from, Some(*l), to))
                        .or_default()
                        .extend(axioms.iter().copied());
                }
            }
        }
        Nfa {
            states: end + 1,
            start: 0,
            end,
            edges: edges
                .into_iter()
                .map(|((a, l, b), axioms)| (a, l, b, axioms.into_iter().collect()))
                .collect(),
        }
    }

    /// Whether `word` takes the start to the end (for tests).
    #[cfg(test)]
    pub(crate) fn accepts(&self, word: &[ObjProp]) -> bool {
        let closure = |set: BTreeSet<u32>| {
            let mut out = set.clone();
            let mut stack: Vec<u32> = set.into_iter().collect();
            while let Some(x) = stack.pop() {
                for (a, l, b, _) in &self.edges {
                    if *a == x && l.is_none() && out.insert(*b) {
                        stack.push(*b);
                    }
                }
            }
            out
        };
        let mut at = closure(BTreeSet::from([self.start]));
        for w in word {
            let next: BTreeSet<u32> = self
                .edges
                .iter()
                .filter(|(a, l, _, _)| at.contains(a) && *l == Some(*w))
                .map(|(_, _, b, _)| *b)
                .collect();
            at = closure(next);
        }
        at.contains(&self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transitive role over two transitive subroles, spliced: the same words, fewer
    /// states.
    #[test]
    fn minimal_automata_accept_the_same_words() {
        let (r, s, t) = (ObjProp::Named(1), ObjProp::Named(2), ObjProp::Named(3));
        // r's: 0 -r-> 1, 1 -ε-> 0; s and t spliced at 2-3 and 4-5, each transitive.
        let nfa = Nfa {
            states: 6,
            start: 0,
            end: 1,
            edges: vec![
                (0, Some(r), 1, vec![]),
                (1, None, 0, vec![10]),
                (0, None, 2, vec![11]),
                (2, Some(s), 3, vec![11]),
                (3, None, 2, vec![12]),
                (3, None, 1, vec![11]),
                (0, None, 4, vec![13]),
                (4, Some(t), 5, vec![13]),
                (5, None, 4, vec![14]),
                (5, None, 1, vec![13]),
            ],
        };
        let min = nfa.minimal();
        assert!(min.states < nfa.states, "{min:?}");
        let alphabet = [r, s, t, r.inverse()];
        let mut words: Vec<Vec<ObjProp>> = vec![Vec::new()];
        for _ in 0..4 {
            let longer: Vec<Vec<ObjProp>> = words
                .iter()
                .flat_map(|w| {
                    alphabet.iter().map(move |&l| {
                        let mut w = w.clone();
                        w.push(l);
                        w
                    })
                })
                .collect();
            words.extend(longer);
        }
        for w in &words {
            assert_eq!(min.accepts(w), nfa.accepts(w), "{w:?}");
        }
        // Each transition keeps the axioms of what it stands for.
        assert!(
            min.edges
                .iter()
                .any(|(_, l, _, ax)| *l == Some(s) && ax.contains(&11))
        );
        // The inverse of the minimal is the minimal's language reversed.
        for w in &words {
            let reversed: Vec<ObjProp> = w.iter().rev().map(|l| l.inverse()).collect();
            assert_eq!(min.inverse().accepts(&reversed), nfa.accepts(w));
        }
    }
}
