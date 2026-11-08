//! The shapes the QL/RL completeness reviews found by hand, as templates for the DL-oracle
//! differential (Harness note `2026-11-08-dl-oracle-differential-design.md`, "The
//! generator"). A [`case`] is a random ontology of the generator, an ABox over every
//! individual, and for half the seeds one template family's axioms, filled with random
//! terms of the signature and nested under a random `∀`, `≤ 0` or `¬≥ 1` for depth.
//! Counting restrictions and `Self` take simple properties only, so every case stays
//! within OWL 2 DL's global restrictions.

use super::{Gen, Profile, Rng, Signature, ontology};
use crate::mapping::Ontology;
use crate::model::{Axiom, ClassExpr, ExprId, ObjProp, Term, canonical};

/// The template families, as the campaign reports them; a case without one is `uniform`.
pub const FAMILIES: [&str; 8] = [
    // I24, I24b: an asserted existential; `∀R.∃S.D`.
    "asserted",
    "universal",
    // I24b review F1: an intersection or a union on the left.
    "left",
    // F2, F3: a disjoint union, a restriction in a disjointness.
    "disjoint",
    // F4 and the I24b second lead's bounded forms, in their settings.
    "bounded",
    // W-N1 `∀R.Self(S)`, W-N2 `∀R.{a}`.
    "self-nominal",
    // I24c review r1: `{a}`, `¬¬D`, `¬≥2 R`, `∀R.D` after a left side, `∀R.¬{a}`.
    "review",
    // I24c sol-loop S1–S4: zero bounds around a nominal or a complement.
    "sol",
];

/// A generated case.
pub struct Case {
    pub ontology: Ontology,
    /// One of [`FAMILIES`], or `uniform`.
    pub family: &'static str,
    /// The classes and properties the template axioms use (empty for `uniform`), for
    /// queries on them.
    pub focus: Vec<Term>,
}

/// A case of `profile` over `sig` (see the module). Nine bases in ten are inside OWL 2 QL
/// and RL: EL's constructors without chains, keeping the axioms both profiles' grammars
/// admit (`crate::profile::of`). The template then supplies the axiom outside them, and a
/// `complete` claim stays common. One in ten is all of `profile`, so the fail-closed path
/// stays covered. (Master, 8 Nov 13:33: on a fail-closed head, random SROIQ bases left 76
/// claims in 636; EL bases, 101.)
pub fn case(rng: &mut Rng, sig: &Signature, profile: Profile) -> Case {
    let sroiq = rng.one_in(10);
    let base_profile = match sroiq {
        true => profile,
        false => Profile {
            el: true,
            data: false,
            nominals: false,
            numbers: false,
            chains: false,
            abox: false,
            ..profile
        },
    };
    let mut base = ontology(rng, sig, base_profile);
    if !sroiq {
        let kept: Vec<Axiom> = base
            .axioms
            .iter()
            .filter(|a| {
                let p = crate::profile::of(&base, a);
                matches!(a, Axiom::Declaration(..)) || (p.ql && p.rl)
            })
            .cloned()
            .collect();
        base.axioms = kept;
    }
    let templated = rng.one_in(2);
    let family = match templated {
        true => FAMILIES[rng.below(FAMILIES.len() as u64) as usize],
        false => "uniform",
    };
    let mut axioms = base.axioms.clone();
    let mut t = T {
        g: Gen {
            rng,
            sig,
            profile,
            o: base,
        },
        focus: Vec::new(),
    };
    if templated {
        axioms.extend(t.family(family));
    }
    axioms.extend(t.abox());
    axioms.sort();
    axioms.dedup();
    let mut focus = std::mem::take(&mut t.focus);
    focus.sort_unstable();
    focus.dedup();
    let mut o = t.g.o;
    o.sources = vec![Vec::new(); axioms.len()];
    o.axioms = axioms;
    Case {
        ontology: o,
        family,
        focus,
    }
}

/// The generator, recording the classes and properties a template picks.
struct T<'a> {
    g: Gen<'a>,
    focus: Vec<Term>,
}

impl T<'_> {
    fn e(&mut self, expr: ClassExpr) -> ExprId {
        self.g.e(expr)
    }

    fn class_term(&mut self) -> Term {
        let c = self.g.rng.pick(&self.g.sig.classes);
        self.focus.push(c);
        c
    }

    fn named(&mut self) -> ExprId {
        let c = self.class_term();
        self.e(ClassExpr::Class(c))
    }

    /// A simple property (for counting and `Self`), named or `inverse`.
    fn simple(&mut self, inverse: bool) -> ObjProp {
        let p = self
            .g
            .rng
            .pick(&self.g.sig.object_properties[..self.g.sig.simple.max(1)]);
        self.focus.push(p);
        match inverse {
            true => ObjProp::Inverse(p),
            false => ObjProp::Named(p),
        }
    }

    fn any(&mut self) -> ObjProp {
        let p = self.g.rng.pick(&self.g.sig.object_properties);
        self.focus.push(p);
        ObjProp::Named(p)
    }

    fn individual(&mut self) -> Term {
        self.g.rng.pick(&self.g.sig.individuals)
    }

    fn not(&mut self, x: ExprId) -> ExprId {
        self.e(ClassExpr::Not(x))
    }

    fn nominal(&mut self) -> ExprId {
        let a = self.individual();
        self.e(ClassExpr::OneOf(vec![a]))
    }

    /// `∃S.D`, the filler a data existential (`∃d.integer`) where the signature has one,
    /// one time in four.
    fn some(&mut self) -> ExprId {
        if self.g.profile.data && !self.g.sig.data_properties.is_empty() && self.g.rng.one_in(4) {
            let d = self.g.rng.pick(&self.g.sig.data_properties);
            self.focus.push(d);
            let range = self.g.range();
            return self.e(ClassExpr::DataSome(d, range));
        }
        let (s, d) = (self.any(), self.named());
        self.e(ClassExpr::Some(s, d))
    }

    /// `x`, or for depth `∀T.x`, `≤ 0 T.¬x` or `¬≥ 1 T.¬x` (one time in three).
    fn nest(&mut self, x: ExprId) -> ExprId {
        match self.g.rng.below(6) {
            0 => {
                let t = self.any();
                self.e(ClassExpr::All(t, x))
            }
            1 | 2 => {
                let (t, not) = (self.simple(false), self.not(x));
                let bound = match self.g.rng.one_in(2) {
                    true => ClassExpr::Max(0, t, not),
                    false => {
                        let min = self.e(ClassExpr::Min(1, t, not));
                        return self.not(min);
                    }
                };
                self.e(bound)
            }
            _ => x,
        }
    }

    /// `C ⊑ x` (nested), with an individual in `C` and an edge from it along a property
    /// the template picked, so a universal in `x` has a successor to act on.
    fn concluded(&mut self, x: ExprId) -> Vec<Axiom> {
        let (c, x) = (self.named(), self.nest(x));
        let (a, b) = (self.individual(), self.individual());
        let sig = self.g.sig;
        let picked: Vec<Term> = self
            .focus
            .iter()
            .copied()
            .filter(|t| sig.object_properties.contains(t))
            .collect();
        let r = match picked.is_empty() {
            true => self.g.rng.pick(&sig.object_properties),
            false => self.g.rng.pick(&picked),
        };
        vec![
            Axiom::SubClassOf(c, x),
            Axiom::ClassAssertion(c, a),
            Axiom::ObjectPropertyAssertion(r, a, b),
        ]
    }

    fn family(&mut self, family: &str) -> Vec<Axiom> {
        let pick = self.g.rng.below(5);
        match family {
            "asserted" => {
                let x = self.some();
                vec![Axiom::ClassAssertion(x, self.individual())]
            }
            "universal" => {
                let (r, some) = (self.any(), self.some());
                let all = self.e(ClassExpr::All(r, some));
                self.concluded(all)
            }
            "left" => {
                let parts = canonical(vec![self.named(), self.named()]);
                let left = match self.g.rng.one_in(2) {
                    true => self.e(ClassExpr::And(parts.clone())),
                    false => self.e(ClassExpr::Or(parts.clone())),
                };
                let some = self.some();
                let right = match self.g.rng.one_in(2) {
                    true => some,
                    false => {
                        let r = self.any();
                        self.e(ClassExpr::All(r, some))
                    }
                };
                let a = self.individual();
                let mut out = vec![Axiom::SubClassOf(left, right)];
                out.extend(parts.iter().map(|&p| Axiom::ClassAssertion(p, a)));
                out
            }
            "disjoint" => {
                let a = self.individual();
                // F3's `≤ 0 S.D`.
                let (s, d) = (self.simple(false), self.named());
                let none = self.e(ClassExpr::Max(0, s, d));
                let (class, parts, member) = match pick % 3 {
                    // F2: `DisjointUnion(C, ∃R.A, ∃R.B)`, an individual in `C`.
                    0 => {
                        let class = self.class_term();
                        let r = self.any();
                        let (x, y) = (self.named(), self.named());
                        let parts =
                            vec![self.e(ClassExpr::Some(r, x)), self.e(ClassExpr::Some(r, y))];
                        let member = self.e(ClassExpr::Class(class));
                        (Some(class), parts, member)
                    }
                    // F3: `DisjointUnion(C, A, ≤ 0 S.D)`, an individual in `A`.
                    1 => {
                        let (class, part) = (self.class_term(), self.named());
                        (Some(class), vec![part, none], part)
                    }
                    // F3: `A` disjoint with `≤ 0 S.D`, an individual in `A`.
                    _ => {
                        let part = self.named();
                        (None, vec![part, none], part)
                    }
                };
                let parts = canonical(parts);
                if parts.len() < 2 {
                    return Vec::new();
                }
                let axiom = match class {
                    Some(c) => Axiom::DisjointUnion(c, parts),
                    None => Axiom::DisjointClasses(parts),
                };
                vec![axiom, Axiom::ClassAssertion(member, a)]
            }
            "bounded" => {
                let body = self.bounded_body(pick);
                match self.g.rng.below(3) {
                    // Inside a disjointness: `C` disjoint from the complement.
                    0 => {
                        let (c, not) = (self.named(), self.not(body));
                        let a = self.individual();
                        vec![
                            Axiom::DisjointClasses(canonical(vec![c, not])),
                            Axiom::ClassAssertion(c, a),
                        ]
                    }
                    // Asserted of an individual.
                    1 => vec![Axiom::ClassAssertion(body, self.individual())],
                    _ => self.concluded(body),
                }
            }
            "self-nominal" => {
                let r = self.any();
                let filler = match self.g.rng.one_in(2) {
                    true => {
                        let s = self.simple(false);
                        self.e(ClassExpr::HasSelf(s))
                    }
                    false => self.nominal(),
                };
                let all = self.e(ClassExpr::All(r, filler));
                let mut out = self.concluded(all);
                if self.g.rng.one_in(2) {
                    let thing = self.e(ClassExpr::Thing);
                    let some = self.e(ClassExpr::Some(r, thing));
                    let Axiom::SubClassOf(c, _) = out[0] else {
                        unreachable!("concluded starts with its subclass axiom")
                    };
                    out.push(Axiom::SubClassOf(c, some));
                }
                out
            }
            "review" => match pick {
                0 => {
                    let n = self.nominal();
                    self.concluded(n)
                }
                1 => {
                    let d = self.named();
                    let (not, r) = (self.not(d), self.simple(false));
                    let x = match self.g.rng.one_in(2) {
                        true => self.not(not),
                        false => {
                            let thing = self.e(ClassExpr::Thing);
                            let min = self.e(ClassExpr::Min(2, r, thing));
                            self.not(min)
                        }
                    };
                    self.concluded(x)
                }
                2 => {
                    let parts = canonical(vec![self.named(), self.named()]);
                    let left = self.e(ClassExpr::And(parts.clone()));
                    let (r, d) = (self.any(), self.named());
                    let all = self.e(ClassExpr::All(r, d));
                    let a = self.individual();
                    let mut out = vec![Axiom::SubClassOf(left, all)];
                    out.extend(parts.iter().map(|&p| Axiom::ClassAssertion(p, a)));
                    out
                }
                _ => {
                    let (r, n) = (self.any(), self.nominal());
                    let not = self.not(n);
                    let all = self.e(ClassExpr::All(r, not));
                    self.concluded(all)
                }
            },
            _ => {
                // "sol"
                let r = self.simple(false);
                let x = match pick {
                    0 => {
                        let d = self.named();
                        let not = self.not(d);
                        ClassExpr::Max(0, r, not)
                    }
                    1 => {
                        let n = self.nominal();
                        let not = self.not(n);
                        ClassExpr::Max(0, r, not)
                    }
                    2 => ClassExpr::Exact(0, r, self.nominal()),
                    _ => {
                        let s = self.any();
                        let n = self.nominal();
                        let filler = match pick {
                            3 => n,
                            _ => {
                                let inverse = ObjProp::Inverse(s.named());
                                self.e(ClassExpr::All(inverse, n))
                            }
                        };
                        let some = self.e(ClassExpr::Some(s, filler));
                        let not = self.not(some);
                        ClassExpr::Max(0, r, not)
                    }
                };
                let x = self.e(x);
                self.concluded(x)
            }
        }
    }

    /// The bounded forms around `¬∃S.D` (F4 and the second lead's): `≤n R`, `=n R` for
    /// n ∈ {0, 1}, `¬≥n R` for n ∈ {1, 2}, and `≤0 R.∀S.¬D`; `R` an inverse one time in
    /// four.
    fn bounded_body(&mut self, pick: u64) -> ExprId {
        let inverse = self.g.rng.one_in(4);
        let r = self.simple(inverse);
        let n = self.g.rng.below(2) as u32;
        let some = self.some();
        let not_some = self.not(some);
        match pick {
            0 => self.e(ClassExpr::Max(n, r, not_some)),
            1 => self.e(ClassExpr::Exact(n, r, not_some)),
            2 | 3 => {
                let min = self.e(ClassExpr::Min(n + 1, r, not_some));
                self.not(min)
            }
            _ => {
                let (s, d) = (self.any(), self.named());
                let not_d = self.not(d);
                let all = self.e(ClassExpr::All(s, not_d));
                self.e(ClassExpr::Max(0, r, all))
            }
        }
    }

    /// Every individual with one or two named types, one to three edges, and sometimes a
    /// data value.
    fn abox(&mut self) -> Vec<Axiom> {
        let mut out = Vec::new();
        let sig = self.g.sig;
        for &a in &sig.individuals {
            for _ in 0..1 + self.g.rng.below(2) {
                let c = self.g.rng.pick(&sig.classes);
                let e = self.e(ClassExpr::Class(c));
                out.push(Axiom::ClassAssertion(e, a));
            }
            for _ in 0..1 + self.g.rng.below(3) {
                let (p, b) = (
                    self.g.rng.pick(&sig.object_properties),
                    self.g.rng.pick(&sig.individuals),
                );
                out.push(Axiom::ObjectPropertyAssertion(p, a, b));
            }
            if self.g.profile.data && !sig.data_properties.is_empty() && self.g.rng.one_in(3) {
                let (d, v) = (
                    self.g.rng.pick(&sig.data_properties),
                    self.g.rng.pick(&sig.literals),
                );
                out.push(Axiom::DataPropertyAssertion(d, a, v));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diagnostic;
    use crate::fuzz::Sizes;

    /// Every case is within OWL 2 DL's global restrictions (*OWL 2 Structural
    /// Specification* §11: simple properties under counting and `Self`, named individuals
    /// in nominals), so owl2-dl can take it; and over 200 seeds every family is drawn
    /// (its template picked terms of the signature).
    #[test]
    fn cases_are_owl2_dl_and_every_family_is_drawn() {
        let mut next = 0;
        let sig = Signature::new(Sizes::default(), &mut |_| {
            next += 1;
            next
        });
        let mut drawn = std::collections::BTreeMap::new();
        for seed in 0..200 {
            let mut case = case(&mut Rng::new(seed), &sig, Profile::sroiq());
            crate::diagnostics::check_global_restrictions(&mut case.ontology);
            let broken: Vec<&Diagnostic> = case
                .ontology
                .diagnostics
                .iter()
                .filter(|d| {
                    matches!(
                        d,
                        Diagnostic::NonSimpleProperty { .. }
                            | Diagnostic::AnonymousIndividual { .. }
                    )
                })
                .collect();
            assert!(broken.is_empty(), "seed {seed}: {broken:?}");
            if case.family != "uniform" && !case.focus.is_empty() {
                *drawn.entry(case.family).or_insert(0) += 1;
            }
        }
        let missing: Vec<&&str> = FAMILIES
            .iter()
            .filter(|f| !drawn.contains_key(*f))
            .collect();
        assert!(missing.is_empty(), "never drawn: {missing:?} ({drawn:?})");
    }
}
