//! Equality by representatives (work package W4, stage A).
//!
//! OWL 2 RL's equality rules (`eq-rep-s/p/o`) make every fact true of every identity of
//! its terms: a class of k identities turns one fact into up to k³ copies, and the store,
//! the joins and the answers carry all of them. Here each `owl:sameAs` class has one
//! representative (its smallest id), every fact is rewritten to representatives, and the
//! rules run on the rewritten facts without the replacement rules.
//!
//! The batch executor does it within its semi-naive rounds (egglog's rebuild; Zhang et
//! al., POPL 2023): a round's new `sameAs` between two representatives merges their
//! classes, and only the facts that mention the representative that lost its place are
//! rewritten, into the next round's delta ([`super::batch`]). So equality costs one
//! materialisation plus the facts its merges touch, however long a cascade of merges is;
//! before 6 October 2026 every merge re-materialised the whole closure, until a pass
//! merged nothing.
//!
//! The replicated closure is the representative closure expanded: a fact holds iff the
//! fact of its terms' representatives is in the representative closure
//! ([`EqualityClasses::expand`]). The property test checks exactly that, and that the
//! same consistency rules fire.

use hashbrown::HashMap;

use super::batch::{self, Schema};
use super::ir::Rule;
use super::ir::{Triple, Violation};
use super::lists::ListVocabulary;

/// The `owl:sameAs` classes of a closure: each term's representative, and each
/// representative's members (only for classes of two or more).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EqualityClasses {
    /// Every member of a class other than its representative, with the representative.
    representative: HashMap<u64, u64>,
    /// Each representative's members, sorted (the representative first).
    members: HashMap<u64, Vec<u64>>,
}

impl EqualityClasses {
    /// The representative of `term` (the term itself outside every class).
    pub fn representative(&self, term: u64) -> u64 {
        self.representative.get(&term).copied().unwrap_or(term)
    }

    /// The identities of `term`'s class, sorted, if it has two or more.
    pub fn class_of(&self, term: u64) -> Option<&[u64]> {
        self.members
            .get(&self.representative(term))
            .map(Vec::as_slice)
    }

    /// The identities of `term`'s class (`[term]` outside every class), sorted.
    pub fn members(&self, term: u64) -> Vec<u64> {
        self.class_of(term)
            .map_or_else(|| vec![term], <[u64]>::to_vec)
    }

    /// Whether `term` is the representative of its class (or in no class).
    pub fn is_representative(&self, term: u64) -> bool {
        !self.representative.contains_key(&term)
    }

    /// The classes of two or more identities: representative and members.
    pub fn classes(&self) -> impl Iterator<Item = (u64, &[u64])> {
        self.members.iter().map(|(&r, m)| (r, m.as_slice()))
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// `fact` with every term replaced by its representative.
    pub fn rewrite(&self, [s, p, o]: Triple) -> Triple {
        [
            self.representative(s),
            self.representative(p),
            self.representative(o),
        ]
    }

    /// Every fact a representative fact stands for.
    pub fn expand(&self, [s, p, o]: Triple) -> Vec<Triple> {
        let (ms, mp, mo) = (self.members(s), self.members(p), self.members(o));
        let mut out = Vec::with_capacity(ms.len() * mp.len() * mo.len());
        for &s in &ms {
            for &p in &mp {
                for &o in &mo {
                    out.push([s, p, o]);
                }
            }
        }
        out
    }

    /// Merges the classes of `a` and `b`; returns the representative that lost its place
    /// (the larger of the two), or `None` if they were one class. The members of that
    /// class move under the smaller representative.
    pub fn union(&mut self, a: u64, b: u64) -> Option<u64> {
        let (ra, rb) = (self.representative(a), self.representative(b));
        if ra == rb {
            return None;
        }
        let (keep, lose) = (ra.min(rb), ra.max(rb));
        let moved = self.members.remove(&lose).unwrap_or_else(|| vec![lose]);
        for &member in &moved {
            self.representative.insert(member, keep);
        }
        let members = self.members.entry(keep).or_insert_with(|| vec![keep]);
        members.extend(moved);
        members.sort_unstable();
        Some(lose)
    }

    /// Merges the classes of each pair; returns the representatives that lost their place.
    pub fn union_all(&mut self, pairs: &[(u64, u64)]) -> Vec<u64> {
        pairs
            .iter()
            .filter_map(|&(a, b)| self.union(a, b))
            .collect()
    }

    /// Merges the classes of each pair; returns whether any class changed.
    #[cfg(test)]
    fn merge(&mut self, pairs: &[(u64, u64)]) -> bool {
        !self.union_all(pairs).is_empty()
    }
}

/// A closure over representatives.
#[derive(Debug, Default)]
pub struct RepresentativeClosure {
    /// Every fact of the closure over representatives, the rewritten input included,
    /// sorted, without duplicates.
    pub facts: Vec<Triple>,
    pub classes: EqualityClasses,
    /// Violations over representatives, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    /// Rounds of the closure, and in how many of them classes merged.
    pub rounds: usize,
    pub merges: usize,
    /// The batch materialisations run: one (merges are rebuilt within its rounds).
    pub passes: usize,
}

/// The `owl:sameAs` id of rules that do equality reasoning (`eq-rep-s`), if they do.
pub fn same_as(rules: &[Rule]) -> Option<u64> {
    let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
    match rule.body.first()?.0[1] {
        super::ir::Term::Const(id) => Some(id),
        super::ir::Term::Var(_) => None,
    }
}

/// The rules without the replacement rules, which rewriting stands in for.
pub fn without_replacement(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !batch::is_replacement_rule(r))
        .cloned()
        .collect()
}

/// The closure of `input` under `rules` over representatives of `owl:sameAs` classes.
/// Rules without equality reasoning give the plain closure (no classes).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> RepresentativeClosure {
    materialise_until(input, rules, lists, schema, super::eval::NEVER).expect("never stopped")
}

/// [`materialise`], polling `stop` in every round.
pub fn materialise_until(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<RepresentativeClosure, super::delta::Interrupted> {
    let result = batch::materialise_representatives_until(
        batch::Input::Facts(input.to_vec()),
        rules,
        lists,
        schema,
        batch::Listing::Representatives,
        stop,
    )?;
    Ok(RepresentativeClosure {
        facts: result.derived,
        classes: result.classes,
        violations: result.violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        merges: result.merges,
        passes: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_merge_expand_and_rewrite() {
        let mut classes = EqualityClasses::default();
        assert!(classes.merge(&[(5, 3), (7, 9)]));
        assert!(!classes.merge(&[(3, 5)]));
        assert!(classes.merge(&[(9, 5)]));
        assert_eq!(classes.members(7), &[3, 5, 7, 9]);
        assert_eq!(classes.representative(9), 3);
        assert_eq!(classes.members(42), vec![42]);
        assert!(classes.is_representative(3) && !classes.is_representative(5));
        assert_eq!(classes.rewrite([9, 1, 42]), [3, 1, 42]);
        let expanded: Vec<Triple> = classes.expand([3, 1, 42]);
        assert_eq!(expanded.len(), 4);
        // The representative that lost its place is reported.
        assert_eq!(classes.union(2, 7), Some(3));
        assert_eq!(classes.members(9), &[2, 3, 5, 7, 9]);
        assert_eq!(classes.union(9, 2), None);
    }
}
