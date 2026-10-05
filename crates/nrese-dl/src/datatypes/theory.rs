//! The datatype theory (docs/design/owl2-dl.md, "The datatype theory"): whether data
//! variables, each constrained by literals and data ranges of either polarity and some
//! pairwise unequal, can take values, solved per connected component of the
//! inequalities.
//!
//! - A variable's values are the intersection of its ranges and the complements of its
//!   negated ones (`ValueSet`s, exact or bounded both ways: `ranges.rs`).
//! - A component needs distinct values where variables are unequal. A variable with at
//!   least as many values as the component has variables always finds one; the others
//!   (few values, listed) are searched, smallest first. Before the search, a set of
//!   pairwise unequal variables (`min_cardinality`) whose values together are fewer is a
//!   clash at once.
//! - **A clash carries the dependency sets of the constraints it used**: for one
//!   variable, a minimal subset of its constraints (by deletion); for a component, those
//!   of the searched variables and the inequalities between them. Backjumping over a
//!   choice only a facet combination refutes stays sound.
//! - Where a set is only bounded (`Eval::approximate`), a clash found with its superset is
//!   a clash, and a solution that relied on it is reported as approximate.

use std::collections::{HashMap, HashSet};

use nrese_xsd::owl::{Value, ValueSet};

use super::ranges::{Eval, Ranges};
use crate::tableau::depset::{DepSetId, DepSets};

/// A data variable of the theory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DataVar(pub u32);

/// The answer for a component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The variables can take values; `approximate`: only as far as approximated ranges
    /// tell (why), so not an answer for a model.
    Sat { approximate: Option<String> },
    /// No values: the union of what the clash depends on.
    Clash(DepSetId),
}

#[derive(Debug, Clone)]
enum Of {
    Range(nrese_owl::RangeId),
    Value(Value),
}

#[derive(Debug, Clone)]
struct Constraint {
    of: Of,
    positive: bool,
    dep: DepSetId,
}

/// A component's inequalities over its own indexes.
struct Local {
    edges: Vec<(usize, usize, DepSetId)>,
    adjacent: Vec<HashSet<usize>>,
}

impl Local {
    /// A set of pairwise unequal variables, greedily by degree.
    fn clique(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.adjacent.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.adjacent[i].len()));
        let mut clique: Vec<usize> = Vec::new();
        for i in order {
            if clique.iter().all(|c| self.adjacent[i].contains(c)) {
                clique.push(i);
            }
        }
        clique
    }
}

/// The most variable assignments one component's search may try.
const SEARCH_BUDGET: u64 = 200_000;

/// Constraints over data variables.
#[derive(Debug, Clone, Default)]
pub struct DatatypeTheory {
    constraints: Vec<Vec<Constraint>>,
    /// Per variable: the variable it was merged into (itself if none), and the merge's
    /// dependency set.
    merged: Vec<(u32, DepSetId)>,
    unequal: Vec<(u32, u32, DepSetId)>,
}

/// A variable's values: the superset decides clashes, the subset certainty.
struct Values {
    over: ValueSet,
    under: ValueSet,
    approximate: Option<String>,
}

impl DatatypeTheory {
    pub fn clear(&mut self) {
        self.constraints.clear();
        self.merged.clear();
        self.unequal.clear();
    }

    pub fn var(&mut self) -> DataVar {
        let id = self.constraints.len() as u32;
        self.constraints.push(Vec::new());
        self.merged.push((id, DepSetId::EMPTY));
        DataVar(id)
    }

    pub fn len(&self) -> usize {
        self.constraints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.constraints.is_empty()
    }

    /// `var` is the value (a literal's).
    pub fn add_literal(&mut self, var: DataVar, value: Value, dep: DepSetId) {
        self.constraints[var.0 as usize].push(Constraint {
            of: Of::Value(value),
            positive: true,
            dep,
        });
    }

    /// `var` is in `range` (`positive`) or not.
    pub fn add_range(
        &mut self,
        var: DataVar,
        range: nrese_owl::RangeId,
        positive: bool,
        dep: DepSetId,
    ) {
        self.constraints[var.0 as usize].push(Constraint {
            of: Of::Range(range),
            positive,
            dep,
        });
    }

    pub fn add_not_equal(&mut self, a: DataVar, b: DataVar, dep: DepSetId) {
        self.unequal.push((a.0, b.0, dep));
    }

    /// `a` and `b` are one value, by `dep`.
    pub fn merge(&mut self, a: DataVar, b: DataVar, dep: DepSetId, deps: &mut DepSets) {
        let (ra, da) = self.find(a.0, deps);
        let (rb, db) = self.find(b.0, deps);
        if ra != rb {
            let d = deps.union(dep, da);
            let d = deps.union(d, db);
            self.merged[rb as usize] = (ra, d);
        }
    }

    /// The variable `v` stands for, with what the merges on the way depend on.
    fn find(&self, mut v: u32, deps: &mut DepSets) -> (u32, DepSetId) {
        let mut dep = DepSetId::EMPTY;
        while self.merged[v as usize].0 != v {
            let (to, d) = self.merged[v as usize];
            dep = deps.union(dep, d);
            v = to;
        }
        (v, dep)
    }

    /// The connected components of the inequalities, over the variables merges leave.
    pub fn components(&self, deps: &mut DepSets) -> Vec<Vec<DataVar>> {
        let n = self.constraints.len();
        let mut parent: Vec<u32> = (0..n as u32).map(|v| self.find(v, deps).0).collect();
        fn root(parent: &mut [u32], mut v: u32) -> u32 {
            while parent[v as usize] != v {
                let p = parent[v as usize];
                parent[v as usize] = parent[p as usize];
                v = p;
            }
            v
        }
        for &(a, b, _) in &self.unequal {
            let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
            if ra != rb {
                parent[ra.max(rb) as usize] = ra.min(rb);
            }
        }
        let mut groups: std::collections::BTreeMap<u32, Vec<DataVar>> = Default::default();
        for v in 0..n as u32 {
            if self.find(v, deps).0 == v {
                groups
                    .entry(root(&mut parent, v))
                    .or_default()
                    .push(DataVar(v));
            }
        }
        groups.into_values().collect()
    }

    /// The fewest distinct values `component` needs: the size of a set of pairwise
    /// unequal variables in it (found greedily, so a lower bound of the largest).
    pub fn min_cardinality(&self, component: &[DataVar], deps: &mut DepSets) -> u64 {
        let local = self.local(component, deps);
        local.clique().len() as u64
    }

    /// The inequalities within `component`, between representatives, over the
    /// component's indexes.
    fn local(&self, component: &[DataVar], deps: &mut DepSets) -> Local {
        let index: HashMap<u32, usize> = component
            .iter()
            .enumerate()
            .map(|(i, v)| (v.0, i))
            .collect();
        let mut local = Local {
            edges: Vec::new(),
            adjacent: vec![HashSet::new(); component.len()],
        };
        for &(a, b, d) in &self.unequal {
            let (ra, da) = self.find(a, deps);
            let (rb, db) = self.find(b, deps);
            if let (Some(&i), Some(&j)) = (index.get(&ra), index.get(&rb)) {
                let d = deps.union(d, da);
                local.edges.push((i, j, deps.union(d, db)));
                local.adjacent[i].insert(j);
                local.adjacent[j].insert(i);
            }
        }
        local
    }

    /// Per representative, the variables merged into it with the merges' dependencies.
    fn members(&self, deps: &mut DepSets) -> HashMap<u32, Vec<(u32, DepSetId)>> {
        let mut out: HashMap<u32, Vec<(u32, DepSetId)>> = HashMap::new();
        for w in 0..self.constraints.len() as u32 {
            let (r, d) = self.find(w, deps);
            out.entry(r).or_default().push((w, d));
        }
        out
    }

    /// The constraints of a representative: its own and its merged variables', with the
    /// merges' dependencies.
    fn constraints_of(&self, members: &[(u32, DepSetId)], deps: &mut DepSets) -> Vec<Constraint> {
        let mut out = Vec::new();
        for &(w, d) in members {
            for c in &self.constraints[w as usize] {
                let mut c = c.clone();
                c.dep = deps.union(c.dep, d);
                out.push(c);
            }
        }
        out
    }

    fn values(constraints: &[Constraint], ranges: &Ranges) -> Values {
        let mut v = Values {
            over: ValueSet::all(),
            under: ValueSet::all(),
            approximate: None,
        };
        for c in constraints {
            let eval: Eval = match &c.of {
                Of::Value(value) => {
                    let s = ValueSet::single(value);
                    Eval {
                        over: s.clone(),
                        under: s,
                        approximate: None,
                    }
                }
                Of::Range(r) => ranges.eval(*r).clone(),
            };
            let eval = if c.positive { eval } else { eval.not() };
            v.over = v.over.intersection(&eval.over);
            v.under = v.under.intersection(&eval.under);
            if v.approximate.is_none() {
                v.approximate = eval.approximate;
            }
        }
        v
    }

    /// A minimal subset of `constraints` whose values are still empty, by deletion: the
    /// union of their dependencies.
    fn empty_core(constraints: &[Constraint], ranges: &Ranges, deps: &mut DepSets) -> DepSetId {
        let mut kept: Vec<Constraint> = constraints.to_vec();
        if kept.len() <= 24 {
            let mut i = 0;
            while i < kept.len() {
                let mut without = kept.clone();
                without.remove(i);
                if Self::values(&without, ranges).over.is_empty() {
                    kept = without;
                } else {
                    i += 1;
                }
            }
        }
        kept.iter()
            .fold(DepSetId::EMPTY, |acc, c| deps.union(acc, c.dep))
    }

    /// Whether the variables of `component` can take values.
    pub fn check(&self, component: &[DataVar], ranges: &Ranges, deps: &mut DepSets) -> Verdict {
        let n = component.len() as u64;
        let mut approximate: Option<String> = None;
        let members = self.members(deps);
        let mut vars: Vec<(Vec<Constraint>, Values)> = Vec::with_capacity(component.len());
        for &DataVar(v) in component {
            let constraints =
                self.constraints_of(members.get(&v).map_or(&[][..], Vec::as_slice), deps);
            let values = Self::values(&constraints, ranges);
            if values.over.is_empty() {
                return Verdict::Clash(Self::empty_core(&constraints, ranges, deps));
            }
            vars.push((constraints, values));
        }
        let local = self.local(component, deps);
        for &(a, b, d) in &local.edges {
            if a == b {
                // Merged into one and unequal.
                return Verdict::Clash(d);
            }
        }
        // A variable is certain if its subset has a value for everyone, uncertain
        // otherwise where it is approximated.
        for (_, values) in &vars {
            if values.approximate.is_some() && !values.under.count().at_least(n.max(1)) {
                approximate = approximate.or_else(|| values.approximate.clone());
            }
        }
        if n <= 1 {
            return Verdict::Sat { approximate };
        }
        // The dependencies of a set of variables' constraints and inequalities.
        let core = |within: &dyn Fn(usize) -> bool, deps: &mut DepSets| {
            let mut d = DepSetId::EMPTY;
            for (i, (constraints, _)) in vars.iter().enumerate() {
                if within(i) {
                    for c in constraints {
                        d = deps.union(d, c.dep);
                    }
                }
            }
            for &(a, b, e) in &local.edges {
                if within(a) && within(b) {
                    d = deps.union(d, e);
                }
            }
            d
        };
        // Pairwise unequal variables with fewer values among them than they are.
        let clique = local.clique();
        if clique.len() > 1 {
            let union = clique
                .iter()
                .fold(ValueSet::empty(), |u, &i| u.union(&vars[i].1.over));
            if union.count().below(clique.len() as u64) {
                let set: HashSet<usize> = clique.into_iter().collect();
                return Verdict::Clash(core(&|i| set.contains(&i), deps));
            }
        }
        // The variables that may have fewer values than the component has variables,
        // with their values listed.
        let mut small: Vec<(usize, Vec<Value>)> = Vec::new();
        for (i, (_, values)) in vars.iter().enumerate() {
            if values.over.count().at_least(n) {
                continue;
            }
            match values.over.values(n) {
                Some(list) => small.push((i, list)),
                None => {
                    approximate.get_or_insert_with(|| {
                        "a variable with an uncounted number of values".to_owned()
                    });
                }
            }
        }
        if small.is_empty() {
            return Verdict::Sat { approximate };
        }
        small.sort_by_key(|(_, list)| list.len());
        let position: HashMap<usize, usize> = small
            .iter()
            .enumerate()
            .map(|(k, &(i, _))| (i, k))
            .collect();
        let neighbours: Vec<Vec<usize>> = small
            .iter()
            .map(|&(i, _)| {
                local.adjacent[i]
                    .iter()
                    .filter_map(|j| position.get(j).copied())
                    .collect()
            })
            .collect();
        let mut chosen: Vec<Option<usize>> = vec![None; small.len()];
        let mut budget = SEARCH_BUDGET;
        match Self::search(0, &small, &neighbours, &mut chosen, &mut budget) {
            Some(true) => Verdict::Sat { approximate },
            None => Verdict::Sat {
                approximate: Some("the search for distinct values ran out of steps".into()),
            },
            Some(false) => Verdict::Clash(core(&|i| position.contains_key(&i), deps)),
        }
    }

    /// Values for `small[at..]` unequal to their neighbours' chosen ones: whether there
    /// are (`None`: the budget ran out).
    fn search(
        at: usize,
        small: &[(usize, Vec<Value>)],
        neighbours: &[Vec<usize>],
        chosen: &mut [Option<usize>],
        budget: &mut u64,
    ) -> Option<bool> {
        if at == small.len() {
            return Some(true);
        }
        for (i, value) in small[at].1.iter().enumerate() {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let taken = neighbours[at]
                .iter()
                .any(|&o| chosen[o].is_some_and(|j| small[o].1[j] == *value));
            if taken {
                continue;
            }
            chosen[at] = Some(i);
            match Self::search(at + 1, small, neighbours, chosen, budget) {
                Some(false) => {}
                other => return other,
            }
            chosen[at] = None;
        }
        Some(false)
    }
}
