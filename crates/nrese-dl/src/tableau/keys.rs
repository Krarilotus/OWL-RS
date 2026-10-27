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

use std::collections::HashMap;

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
            // A step of the heap profile: what one key's evaluation holds (`tests/key_scratch.rs`).
            nrese_exec::heap::begin_step();
            let acted = self.apply_key(key);
            nrese_exec::heap::end_step("dl: a key evaluated");
            if acted? {
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
        // Per member and non-simple property, the names of its group it has (a group has a
        // name per named individual: the time is checked per member, here and below).
        let mut held: Vec<Vec<Vec<(ConceptId, DepSetId)>>> = Vec::with_capacity(members.len());
        for &(x, _) in &members {
            self.check_time()?;
            let g = &self.g;
            let has = |&c: &ConceptId| g.concept(x, c).map(|f| (c, g.unary[f as usize].dep));
            held.push(
                key.reach
                    .iter()
                    .map(|group| group.iter().filter_map(has).collect())
                    .collect(),
            );
        }
        // Per name of the first non-simple property's group, the members that have it, in order.
        let mut by_name: HashMap<ConceptId, Vec<usize>> = HashMap::new();
        for (i, groups) in held.iter().enumerate() {
            for &(c, _) in groups.first().into_iter().flatten() {
                by_name.entry(c).or_default().push(i);
            }
        }
        // The scratch, charged before any pair is tried: linear in the members and their
        // names, with the partners of one member at a time (every later member, or the
        // buckets of its names), never one entry per pair. Storing the pairs came to 3 GB for
        // 20,000 members that share a name (PR #48's review, F1).
        let n = members.len();
        let names: usize = held.iter().flatten().map(Vec::len).sum();
        let in_buckets: usize = by_name.values().map(Vec::len).sum();
        let scratch = named.capacity() * size_of::<u32>()
            + members.capacity() * size_of::<(u32, DepSetId)>()
            + held.len() * (key.reach.len() + 1) * size_of::<Vec<()>>()
            + names * size_of::<(ConceptId, DepSetId)>()
            + crate::memory::table(by_name.len(), size_of::<(ConceptId, Vec<usize>)>())
            + (in_buckets + n.max(in_buckets)) * size_of::<usize>();
        self.check_memory_with(scratch)?;
        let mut partners: Vec<usize> = Vec::new();
        for i in 0..n {
            self.check_time()?;
            // i's pairs, each once: every later member, or with a non-simple property the later
            // ones with a name of its first group in common (as many as a rule per named
            // individual tried).
            partners.clear();
            match held[i].first() {
                None => partners.extend(i + 1..n),
                Some(own) => {
                    for (c, _) in own {
                        let at = &by_name[c];
                        partners.extend_from_slice(&at[at.partition_point(|&j| j <= i)..]);
                    }
                    partners.sort_unstable();
                    partners.dedup();
                }
            }
            'pairs: for &j in &partners {
                let ((x, dx), (y, dy)) = (members[i], members[j]);
                let mut premise = self.deps.union(dx, dy);
                // A name both have in each non-simple property's group.
                for (hx, hy) in held[i].iter().zip(&held[j]) {
                    let common = hx.iter().find_map(|&(c, d)| {
                        hy.iter().find(|&&(w, _)| w == c).map(|&(_, e)| (d, e))
                    });
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
                    let common = ax.iter().find_map(|&(z, d)| {
                        ay.iter().find(|&&(w, _)| w == z).map(|&(_, e)| (d, e))
                    });
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
