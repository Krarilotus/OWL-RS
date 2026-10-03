//! The canonical taxonomy (docs/design/owl2-dl.md §11), as `benches/reasoning/dl/
//! canonical.py` and the reference runner write it, so that NRESE's taxonomies compare
//! with the references' by hash:
//!
//! - `= rep member` per class, the representative being the smallest IRI of its
//!   equivalence class, `owl:Nothing` for unsatisfiable classes and `owl:Thing` for
//!   classes equivalent to it;
//! - `< sub super` per direct subsumption between representatives (`owl:Thing` for a
//!   class with no other superclass);
//! - sorted, one per line.
//!
//! An inconsistent ontology comes out as `canonical.py` makes it of its closure: every
//! class below `owl:Nothing`.

use std::collections::{BTreeSet, HashMap};

use nrese_owl::Term;

use super::classify::Classification;

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

impl Classification {
    /// The closure lines of the EL classifier's example: `sub<TAB>super` per subsumption,
    /// `C<TAB>owl:Nothing` per unsatisfiable class, `owl:Thing<TAB>C` per class equivalent
    /// to `owl:Thing` (the input of `canonical.py`).
    pub fn closure(&self, name: &dyn Fn(Term) -> String) -> String {
        let mut out = String::new();
        for &(a, b) in &self.subsumptions {
            out.push_str(&format!("{}\t{}\n", name(a), name(b)));
        }
        for &c in &self.unsatisfiable {
            out.push_str(&format!("{}\towl:Nothing\n", name(c)));
        }
        for &c in &self.top {
            out.push_str(&format!("owl:Thing\t{}\n", name(c)));
        }
        out
    }

    /// The canonical taxonomy text; `name` gives a class's IRI (without brackets).
    pub fn canonical(&self, name: &dyn Fn(Term) -> String) -> String {
        let n = self.classes.len();
        let names: Vec<String> = self.classes.iter().map(|&c| name(c)).collect();
        let index: HashMap<Term, usize> = self
            .classes
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i))
            .collect();
        let mut supers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        for &(a, b) in &self.subsumptions {
            if let (Some(&a), Some(&b)) = (index.get(&a), index.get(&b)) {
                supers[a].insert(b);
            }
        }
        let unsat: BTreeSet<usize> = self
            .unsatisfiable
            .iter()
            .filter_map(|c| index.get(c).copied())
            .collect();
        let top: BTreeSet<usize> = self
            .top
            .iter()
            .filter_map(|c| index.get(c).copied())
            .collect();
        // Representatives.
        let rep: Vec<&str> = (0..n)
            .map(|c| {
                if unsat.contains(&c) {
                    return NOTHING;
                }
                if top.contains(&c) {
                    return THING;
                }
                supers[c]
                    .iter()
                    .filter(|&&d| supers[d].contains(&c))
                    .map(|&d| names[d].as_str())
                    .chain([names[c].as_str()])
                    .min()
                    .unwrap_or(names[c].as_str())
            })
            .collect();
        let mut lines: BTreeSet<String> = BTreeSet::new();
        lines.insert(format!("= {THING} {THING}"));
        lines.insert(format!("= {NOTHING} {NOTHING}"));
        for c in 0..n {
            lines.insert(format!("= {} {}", rep[c], names[c]));
        }
        // Each representative's representative superclasses, `owl:Thing` included.
        let mut member: HashMap<&str, usize> = HashMap::new();
        for c in 0..n {
            if rep[c] == names[c] {
                member.insert(rep[c], c);
            }
        }
        let above = |c: usize| -> BTreeSet<&str> {
            let mut set: BTreeSet<&str> = supers[c].iter().map(|&d| rep[d]).collect();
            set.insert(THING);
            set
        };
        for (&r, &c) in &member {
            if r == THING || r == NOTHING {
                continue;
            }
            let mut ups = above(c);
            ups.remove(r);
            ups.remove(NOTHING);
            let mut below_others: BTreeSet<&str> = BTreeSet::new();
            for &b in &ups {
                if let Some(&m) = member.get(b) {
                    for a in above(m) {
                        if a != b {
                            below_others.insert(a);
                        }
                    }
                }
            }
            let direct: Vec<&str> = ups
                .iter()
                .copied()
                .filter(|a| !below_others.contains(a))
                .collect();
            if direct.is_empty() {
                lines.insert(format!("< {r} {THING}"));
            }
            for a in direct {
                lines.insert(format!("< {r} {a}"));
            }
        }
        let mut out = lines.into_iter().collect::<Vec<_>>().join("\n");
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canonical_form_of_a_small_taxonomy() {
        // 1 ≡ 2 ⊑ 3, 4 unsatisfiable, 5 ≡ ⊤.
        let c = Classification {
            classes: vec![1, 2, 3, 4, 5],
            subsumptions: vec![(1, 2), (1, 3), (1, 5), (2, 1), (2, 3), (2, 5), (3, 5)],
            unsatisfiable: vec![4],
            top: vec![5],
            consistent: true,
        };
        let name = |t: Term| format!("http://e/{t}");
        let text = c.canonical(&name);
        let expected = [
            "< http://e/1 http://e/3",
            "< http://e/3 http://www.w3.org/2002/07/owl#Thing",
            "= http://e/1 http://e/1",
            "= http://e/1 http://e/2",
            "= http://e/3 http://e/3",
            "= http://www.w3.org/2002/07/owl#Nothing http://e/4",
            "= http://www.w3.org/2002/07/owl#Nothing http://www.w3.org/2002/07/owl#Nothing",
            "= http://www.w3.org/2002/07/owl#Thing http://e/5",
            "= http://www.w3.org/2002/07/owl#Thing http://www.w3.org/2002/07/owl#Thing",
        ];
        assert_eq!(text, expected.join("\n") + "\n");
    }
}
