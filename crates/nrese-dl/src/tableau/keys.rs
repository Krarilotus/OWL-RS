//! Keys in the hypertableau: `nrese-owl`'s DL-safe rules for `HasKey(C (P₁…Pₘ)
//! (D₁…Dₙ))`, applied to named individuals only (OWL 2 Direct Semantics: `x`, `y` and
//! each `zᵢ` named):
//!
//! `C(x) ∧ C(y) ∧ ⋀ Pᵢ(x, zᵢ) ∧ Pᵢ(y, zᵢ) ∧ ⋀ Dⱼ(x, vⱼ) ∧ Dⱼ(y, wⱼ) → x ≈ y ∨ ⋁ vⱼ ≉ wⱼ`
//!
//! The data values meet in one variable in the rule; here, where they are two concrete
//! nodes, the rule says the individuals are equal unless the values differ (HermiT's
//! encoding): a choice the datatype theory then decides. A class that isn't a name is
//! the normalisation's fresh name `Q` with `C ⊑ Q`, which holds wherever `C` does in the
//! model the graph stands for. A non-simple property is read through names, one per named
//! individual, that hold where a path along the property reaches it: the two individuals
//! need one name of the property's group in common (`KeyRule::reach`).

use std::collections::{BTreeSet, HashMap};

use super::depset::DepSetId;
use super::engine::{Engine, Lit, Step};
use super::graph::Annot;
use super::program::{ConceptId, KeyRule, RoleExpr};

/// A data value of `x` and one of `y` for one data property, each with its edge's
/// dependencies.
type ValuePair = ((u32, DepSetId), (u32, DepSetId));

impl Engine<'_> {
    /// Applies the first key instance whose head doesn't hold; whether one was.
    pub fn apply_keys(&mut self) -> Step<bool> {
        if self.p.keys.is_empty() {
            return Ok(false);
        }
        let p = self.p;
        for key in &p.keys {
            if self.apply_key(key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The named nodes along `role` from `x`, each with its edge's dependencies.
    fn along(&self, x: u32, role: RoleExpr, named: &[u32]) -> Vec<(u32, DepSetId)> {
        let mut out: Vec<(u32, DepSetId)> = if role.inverse {
            self.g
                .in_edges(x)
                .filter(|(_, e)| e.role == role.role)
                .map(|(_, e)| (e.from, e.dep))
                .collect()
        } else {
            self.g
                .out_edges(x)
                .filter(|(_, e)| e.role == role.role)
                .map(|(_, e)| (e.to, e.dep))
                .collect()
        };
        out.retain(|&(z, _)| self.g.live(z) && named.binary_search(&z).is_ok());
        out
    }

    fn apply_key(&mut self, key: &KeyRule) -> Step<bool> {
        // The named nodes, and those in the key's class with the dependencies.
        let mut named: Vec<u32> = Vec::new();
        let mut members: Vec<(u32, DepSetId)> = Vec::new();
        for i in 0..self.p.individuals.len() {
            if self.p.anonymous[i] {
                continue;
            }
            let (x, merges) = self.canonical(self.roots[i]);
            if !self.g.live(x) {
                continue;
            }
            named.push(x);
            let mut dep = Some(merges);
            for &c in &key.classes {
                dep = match (dep, self.g.concept(x, c)) {
                    (Some(d), Some(f)) => Some(self.deps.union(d, self.g.unary[f as usize].dep)),
                    _ => None,
                };
            }
            if let Some(d) = dep {
                members.push((x, d));
            }
        }
        named.sort_unstable();
        named.dedup();
        members.sort_unstable_by_key(|&(x, _)| x);
        members.dedup_by_key(|&mut (x, _)| x);
        // Per member and non-simple property, the names of its group it has.
        let held: Vec<Vec<Vec<(ConceptId, DepSetId)>>> = members
            .iter()
            .map(|&(x, _)| {
                let g = &self.g;
                let has = |&c: &ConceptId| g.concept(x, c).map(|f| (c, g.unary[f as usize].dep));
                key.reach
                    .iter()
                    .map(|group| group.iter().filter_map(has).collect())
                    .collect()
            })
            .collect();
        // The pairs: every two members, or with a non-simple property those with a name of
        // its group in common (as many as a rule per named individual tried).
        let shared: Option<BTreeSet<(usize, usize)>> = (!key.reach.is_empty()).then(|| {
            let mut by_name: HashMap<ConceptId, Vec<usize>> = HashMap::new();
            for (i, groups) in held.iter().enumerate() {
                for &(c, _) in &groups[0] {
                    by_name.entry(c).or_default().push(i);
                }
            }
            let mut pairs = BTreeSet::new();
            for at in by_name.values() {
                for (k, &i) in at.iter().enumerate() {
                    pairs.extend(at[k + 1..].iter().map(|&j| (i, j)));
                }
            }
            pairs
        });
        let n = members.len();
        let pairs: Box<dyn Iterator<Item = (usize, usize)> + '_> = match &shared {
            Some(pairs) => Box::new(pairs.iter().copied()),
            None => Box::new((0..n).flat_map(move |i| (i + 1..n).map(move |j| (i, j)))),
        };
        'pairs: for (i, j) in pairs {
            let ((x, dx), (y, dy)) = (members[i], members[j]);
            let mut premise = self.deps.union(dx, dy);
            // A name both have in each non-simple property's group.
            for (hx, hy) in held[i].iter().zip(&held[j]) {
                let common = hx
                    .iter()
                    .find_map(|&(c, d)| hy.iter().find(|&&(w, _)| w == c).map(|&(_, e)| (d, e)));
                let Some((d, e)) = common else {
                    continue 'pairs;
                };
                premise = self.deps.union(premise, d);
                premise = self.deps.union(premise, e);
            }
            // A common named Pᵢ-neighbour for each object property.
            for &role in &key.objects {
                let ax = self.along(x, role, &named);
                let ay = self.along(y, role, &named);
                let common = ax
                    .iter()
                    .find_map(|&(z, d)| ay.iter().find(|&&(w, _)| w == z).map(|&(_, e)| (d, e)));
                let Some((d, e)) = common else {
                    continue 'pairs;
                };
                premise = self.deps.union(premise, d);
                premise = self.deps.union(premise, e);
            }
            // Each combination of the Dⱼ-values of the two.
            let mut values: Vec<Vec<ValuePair>> = Vec::new();
            for &d in &key.data {
                let role = RoleExpr {
                    role: d,
                    inverse: false,
                };
                let vx: Vec<(u32, DepSetId)> = self
                    .g
                    .out_edges(x)
                    .filter(|(_, e)| e.role == role.role && self.g.live(e.to))
                    .map(|(_, e)| (e.to, e.dep))
                    .collect();
                let vy: Vec<(u32, DepSetId)> = self
                    .g
                    .out_edges(y)
                    .filter(|(_, e)| e.role == role.role && self.g.live(e.to))
                    .map(|(_, e)| (e.to, e.dep))
                    .collect();
                if vx.is_empty() || vy.is_empty() {
                    continue 'pairs;
                }
                let mut pairs = Vec::new();
                for &a in &vx {
                    for &b in &vy {
                        pairs.push((a, b));
                    }
                }
                values.push(pairs);
            }
            // Every combination: x ≈ y or one pair of values differs.
            let mut combination = vec![0usize; values.len()];
            loop {
                if self.key_instance(x, y, premise, &values, &combination, key.source)? {
                    return Ok(true);
                }
                // The next combination.
                let mut k = 0;
                loop {
                    if k == values.len() {
                        continue 'pairs;
                    }
                    combination[k] += 1;
                    if combination[k] < values[k].len() {
                        break;
                    }
                    combination[k] = 0;
                    k += 1;
                }
            }
        }
        Ok(false)
    }

    /// One instance `x ≈ y ∨ ⋁ vⱼ ≉ wⱼ`: whether it acted (asserted, branched or clashed
    /// as an error).
    fn key_instance(
        &mut self,
        x: u32,
        y: u32,
        premise: DepSetId,
        values: &[Vec<ValuePair>],
        combination: &[usize],
        source: u32,
    ) -> Step<bool> {
        let mut dep = premise;
        let mut open: Vec<Lit> = Vec::new();
        for (j, pairs) in values.iter().enumerate() {
            let ((v, dv), (w, dw)) = pairs[combination[j]];
            dep = self.deps.union(dep, dv);
            dep = self.deps.union(dep, dw);
            match self.holds(Lit::Unequal(v, w)) {
                Ok(true) => return Ok(false),
                Ok(false) => open.push(Lit::Unequal(v, w)),
                Err(refuted) => dep = self.deps.union(dep, refuted),
            }
        }
        let equal = Lit::Equal(x, y, Annot::NONE);
        match self.holds(equal) {
            Ok(true) => return Ok(false),
            Ok(false) => open.insert(0, equal),
            Err(refuted) => dep = self.deps.union(dep, refuted),
        }
        self.stats.key_firings += 1;
        match open.len() {
            0 => Err(self.clash(dep, DepSetId::EMPTY)),
            1 => self.assert(open[0], dep, source).map(|()| true),
            _ => self.branch(open, dep).map(|()| true),
        }
    }
}
