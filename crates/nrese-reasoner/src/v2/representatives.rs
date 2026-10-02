//! Equality by representatives (work package W4, stage A).
//!
//! OWL 2 RL's equality rules (`eq-rep-s/p/o`) make every fact true of every identity of
//! its terms: a class of k identities turns one fact into up to k³ copies, and the store,
//! the joins and the answers carry all of them. Here each `owl:sameAs` class has one
//! representative (its smallest id), every fact is rewritten to representatives, and the
//! rules run on the rewritten facts without the replacement rules. When the rules derive
//! a new `sameAs` between two representatives, their classes merge, the facts are
//! rewritten again, and the closure continues.
//!
//! The replicated closure is the representative closure expanded: a fact holds iff the
//! fact of its terms' representatives is in the representative closure
//! ([`EqualityClasses::expand`]). The property test checks exactly that, and that the
//! same consistency rules fire.

use std::collections::HashMap;

use rayon::prelude::*;

use super::batch::{self, Materialisation, Schema};
use super::ir::Rule;
use super::lists::ListVocabulary;
use super::naive::{Triple, Violation};

/// The `owl:sameAs` classes of a closure: each term's representative, and each
/// representative's members (only for classes of two or more).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EqualityClasses {
    representative: HashMap<u64, u64>,
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
        self.representative(term) == term
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

    /// Merges the classes of each pair; returns whether any class changed.
    fn merge(&mut self, pairs: &[(u64, u64)]) -> bool {
        let mut parent: HashMap<u64, u64> = HashMap::new();
        fn find(parent: &mut HashMap<u64, u64>, x: u64) -> u64 {
            let mut root = x;
            while let Some(&p) = parent.get(&root) {
                if p == root {
                    break;
                }
                root = p;
            }
            let mut node = x;
            while let Some(&p) = parent.get(&node) {
                if p == root {
                    break;
                }
                parent.insert(node, root);
                node = p;
            }
            root
        }
        let union = |parent: &mut HashMap<u64, u64>, a: u64, b: u64| -> bool {
            let (ra, rb) = (find(parent, a), find(parent, b));
            parent.entry(ra).or_insert(ra);
            parent.entry(rb).or_insert(rb);
            if ra == rb {
                return false;
            }
            // The smaller id stays the root, so a root is its class's smallest id.
            parent.insert(ra.max(rb), ra.min(rb));
            true
        };
        for (&r, members) in &self.members {
            for &x in members {
                union(&mut parent, r, x);
            }
        }
        let mut changed = false;
        for &(a, b) in pairs {
            changed |= union(&mut parent, a, b);
        }
        if !changed {
            return false;
        }
        let nodes: Vec<u64> = parent.keys().copied().collect();
        let mut members: HashMap<u64, Vec<u64>> = HashMap::new();
        for node in nodes {
            let root = find(&mut parent, node);
            members.entry(root).or_default().push(node);
        }
        members.retain(|_, m| m.len() > 1);
        let mut representative = HashMap::new();
        for (r, m) in &mut members {
            m.sort_unstable();
            // The smallest id represents the class.
            debug_assert_eq!(m[0], *r);
            for &x in m.iter() {
                representative.insert(x, *r);
            }
        }
        self.members = members;
        self.representative = representative;
        true
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
    /// Rounds of the last closure, and how many times classes merged.
    pub rounds: usize,
    pub merges: usize,
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
fn without_replacement(rules: &[Rule]) -> Vec<Rule> {
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

/// [`materialise`], polling `stop` in every round of every closure.
pub fn materialise_until(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<RepresentativeClosure, super::delta::Interrupted> {
    let Some(same_as) = same_as(rules) else {
        let result = batch::materialise_owned_until(input.to_vec(), rules, lists, schema, stop)?;
        let mut facts: Vec<Triple> = input.iter().copied().chain(result.derived).collect();
        facts.par_sort_unstable();
        facts.dedup();
        return Ok(RepresentativeClosure {
            facts,
            violations: result.violations,
            diagnostics: result.diagnostics,
            rounds: result.rounds,
            ..RepresentativeClosure::default()
        });
    };
    let rules = without_replacement(rules);
    let mut classes = EqualityClasses::default();
    let mut facts: Vec<Triple> = input.to_vec();
    let mut merges = 0;
    loop {
        // Classes from the sameAs facts known so far.
        let pairs: Vec<(u64, u64)> = facts
            .iter()
            .filter(|t| t[1] == same_as && t[0] != t[2])
            .map(|t| (t[0], t[2]))
            .collect();
        if classes.merge(&pairs) {
            merges += 1;
        }
        let mut rewritten: Vec<Triple> = facts.par_iter().map(|&t| classes.rewrite(t)).collect();
        // The sameAs facts that made a class are now `r sameAs r`: expanded, every pair of
        // its members, as eq-sym and eq-trans derive them.
        rewritten.par_sort_unstable();
        rewritten.dedup();
        let Materialisation {
            derived,
            violations,
            diagnostics,
            rounds,
            ..
        } = batch::materialise_owned_until(rewritten.clone(), &rules, lists, schema, stop)?;
        let mut closure = rewritten;
        closure.extend(derived);
        closure.par_sort_unstable();
        closure.dedup();
        let new_equalities = closure.iter().any(|t| t[1] == same_as && t[0] != t[2]);
        if !new_equalities {
            return Ok(RepresentativeClosure {
                facts: closure,
                classes,
                violations,
                diagnostics,
                rounds,
                merges,
            });
        }
        facts = closure;
    }
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
    }
}
