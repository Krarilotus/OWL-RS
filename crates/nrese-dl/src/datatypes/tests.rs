//! The theory against brute force: random constraints over a finite universe (each
//! variable confined to it by an enumeration), decided by trying every assignment, with
//! the membership of a value in a range computed straight from the range's meaning, not
//! through value sets. A clash must hold for exactly the constraints its dependency set
//! names (what backjumping relies on).

use std::collections::HashMap;

use nrese_owl::{DataRange, DataTerms, Interner, Literal, RangeId, Term};
use nrese_xsd::owl::{Rational, Value};

use super::{DataVar, DatatypeTheory, Ranges, Verdict};
use crate::tableau::depset::{DepSetId, DepSets};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

struct Fixture {
    table: Interner<DataRange>,
    data: DataTerms,
    next: Term,
    integer: Term,
    string: Term,
    boolean: Term,
    min: Term,
    max_ex: Term,
    universe: Vec<(Term, Value)>,
}

impl Fixture {
    fn new() -> Self {
        let mut f = Fixture {
            table: Interner::default(),
            data: DataTerms::default(),
            next: 100,
            integer: 1,
            string: 2,
            boolean: 3,
            min: 4,
            max_ex: 5,
            universe: Vec::new(),
        };
        for (t, local) in [
            (1, "integer"),
            (2, "string"),
            (3, "boolean"),
            (4, "minInclusive"),
            (5, "maxExclusive"),
        ] {
            f.data.iris.insert(t, format!("{XSD}{local}"));
        }
        for i in -2..=2 {
            let t = f.literal(&i.to_string(), "integer");
            f.universe
                .push((t, Value::Real(Rational::integer(i).unwrap())));
        }
        for s in ["a", "b"] {
            let t = f.literal(s, "string");
            f.universe.push((t, Value::String(s.into())));
        }
        let t = f.literal("true", "boolean");
        f.universe.push((t, Value::Boolean(true)));
        f
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.next += 1;
        self.data.literals.insert(
            self.next,
            Literal {
                lexical: lexical.into(),
                datatype: Some(format!("{XSD}{datatype}")),
                language: None,
            },
        );
        self.next
    }

    fn r(&mut self, range: DataRange) -> RangeId {
        RangeId(self.table.intern(range))
    }

    fn random(&mut self, rng: &mut impl FnMut(u64) -> u64, depth: u32) -> RangeId {
        if depth == 0 || rng(3) == 0 {
            let k = rng(5) as usize;
            let lit = self.universe[k].0;
            return match rng(7) {
                0 => self.r(DataRange::Datatype(self.integer)),
                1 => self.r(DataRange::Datatype(self.string)),
                2 => self.r(DataRange::Datatype(self.boolean)),
                3 => self.r(DataRange::Restriction(self.integer, vec![(self.min, lit)])),
                4 => self.r(DataRange::Restriction(
                    self.integer,
                    vec![(self.max_ex, lit)],
                )),
                5 => {
                    let one = self.universe[rng(self.universe.len() as u64) as usize].0;
                    self.r(DataRange::OneOf(vec![one]))
                }
                _ => self.r(DataRange::Literal),
            };
        }
        let a = self.random(rng, depth - 1);
        match rng(3) {
            0 => self.r(DataRange::Not(a)),
            1 => {
                let b = self.random(rng, depth - 1);
                self.r(DataRange::And(sorted(vec![a, b])))
            }
            _ => {
                let b = self.random(rng, depth - 1);
                self.r(DataRange::Or(sorted(vec![a, b])))
            }
        }
    }

    /// Membership from the range's meaning.
    fn member(&self, r: RangeId, v: &Value) -> bool {
        let integer = |v: &Value| matches!(v, Value::Real(q) if q.is_integer());
        let literal = |t: &Term| self.universe.iter().find(|(u, _)| u == t).map(|(_, x)| x);
        match self.table.get(r.0) {
            DataRange::Literal => true,
            DataRange::Datatype(t) if *t == self.integer => integer(v),
            DataRange::Datatype(t) if *t == self.string => matches!(v, Value::String(_)),
            DataRange::Datatype(_) => matches!(v, Value::Boolean(_)),
            DataRange::Restriction(_, facets) => {
                let (facet, bound) = facets[0];
                let Some(Value::Real(b)) = literal(&bound) else {
                    // A facet value outside the facet space: no member.
                    return false;
                };
                let Value::Real(q) = v else { return false };
                integer(v) && if facet == self.min { q >= b } else { q < b }
            }
            DataRange::OneOf(xs) => xs.iter().any(|t| literal(t) == Some(v)),
            DataRange::Not(x) => !self.member(*x, v),
            DataRange::And(xs) => xs.iter().all(|x| self.member(*x, v)),
            DataRange::Or(xs) => xs.iter().any(|x| self.member(*x, v)),
        }
    }
}

fn sorted(mut v: Vec<RangeId>) -> Vec<RangeId> {
    v.sort();
    v.dedup();
    v
}

/// A theory instance as plain data, for brute force.
struct Case {
    vars: usize,
    /// (var, range, positive, point)
    constraints: Vec<(usize, RangeId, bool, u32)>,
    unequal: Vec<(usize, usize, u32)>,
    merges: Vec<(usize, usize, u32)>,
}

/// Whether the constraints whose points are in `allowed` (all if `None`) are satisfiable.
fn brute_force(f: &Fixture, case: &Case, allowed: Option<&[u32]>) -> bool {
    let ok = |p: u32| allowed.is_none_or(|a| a.contains(&p));
    let values: Vec<&Value> = f.universe.iter().map(|(_, v)| v).collect();
    let mut assignment = vec![0usize; case.vars];
    loop {
        let holds = case
            .constraints
            .iter()
            .filter(|c| ok(c.3))
            .all(|&(v, r, positive, _)| f.member(r, values[assignment[v]]) == positive)
            && case
                .unequal
                .iter()
                .filter(|u| ok(u.2))
                .all(|&(a, b, _)| values[assignment[a]] != values[assignment[b]])
            && case
                .merges
                .iter()
                .filter(|m| ok(m.2))
                .all(|&(a, b, _)| values[assignment[a]] == values[assignment[b]]);
        if holds {
            return true;
        }
        let mut k = 0;
        loop {
            if k == case.vars {
                return false;
            }
            assignment[k] += 1;
            if assignment[k] < values.len() {
                break;
            }
            assignment[k] = 0;
            k += 1;
        }
    }
}

#[test]
fn the_theory_agrees_with_brute_force_and_its_clashes_suffice() {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut rng = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let (mut sat, mut clash) = (0, 0);
    for round in 0..3000 {
        let mut f = Fixture::new();
        let all: Vec<Term> = f.universe.iter().map(|(t, _)| *t).collect();
        let universe = f.r(DataRange::OneOf(all));
        let vars = 1 + rng(4) as usize;
        let mut case = Case {
            vars,
            constraints: Vec::new(),
            unequal: Vec::new(),
            merges: Vec::new(),
        };
        let mut point = 0u32;
        for v in 0..vars {
            // Confined to the universe, at no point (always).
            case.constraints.push((v, universe, true, 0));
            for _ in 0..rng(3) {
                point += 1;
                let r = f.random(&mut rng, 2);
                case.constraints.push((v, r, rng(3) != 0, point));
            }
        }
        for _ in 0..rng(5) {
            let (a, b) = (rng(vars as u64) as usize, rng(vars as u64) as usize);
            if a != b {
                point += 1;
                case.unequal.push((a, b, point));
            }
        }
        if rng(4) == 0 && vars > 1 {
            point += 1;
            case.merges.push((0, 1, point));
        }
        let mut ranges = Ranges::from_parts(f.table.clone(), f.data.clone(), HashMap::new());
        ranges.finish();
        let mut deps = DepSets::default();
        let mut theory = DatatypeTheory::default();
        let vs: Vec<DataVar> = (0..vars).map(|_| theory.var()).collect();
        let dep = |p: u32, deps: &mut DepSets| {
            if p == 0 {
                DepSetId::EMPTY
            } else {
                deps.single(p)
            }
        };
        for &(v, r, positive, p) in &case.constraints {
            let d = dep(p, &mut deps);
            theory.add_range(vs[v], r, positive, d);
        }
        for &(a, b, p) in &case.unequal {
            let d = dep(p, &mut deps);
            theory.add_not_equal(vs[a], vs[b], d);
        }
        for &(a, b, p) in &case.merges {
            let d = dep(p, &mut deps);
            theory.merge(vs[a], vs[b], d, &mut deps);
        }
        let mut verdict = Verdict::Sat { approximate: None };
        for component in theory.components(&mut deps) {
            let v = theory.check(&component, &ranges, &mut deps);
            if matches!(v, Verdict::Clash(_)) {
                verdict = v;
                break;
            }
            if let Verdict::Sat {
                approximate: Some(why),
            } = v
            {
                panic!("round {round}: approximate on exact ranges: {why}");
            }
        }
        let expected = brute_force(&f, &case, None);
        match verdict {
            Verdict::Sat { .. } => {
                assert!(
                    expected,
                    "round {round}: the theory says sat, brute force not"
                );
                sat += 1;
            }
            Verdict::Clash(d) => {
                assert!(
                    !expected,
                    "round {round}: the theory says clash, brute force not"
                );
                let points = deps.points(d);
                assert!(
                    !brute_force(&f, &case, Some(&points)),
                    "round {round}: the clash's dependencies {points:?} are satisfiable"
                );
                clash += 1;
            }
        }
    }
    assert!(
        sat > 300 && clash > 300,
        "both kinds occur: {sat} sat, {clash} clash"
    );
}

#[test]
fn counting_clashes_need_every_unequal_variable() {
    // 257 pairwise unequal bytes have no values; 256 do.
    let mut table = Interner::default();
    let mut data = DataTerms::default();
    data.iris.insert(1, format!("{XSD}byte"));
    let byte = RangeId(table.intern(DataRange::Datatype(1)));
    let mut ranges = Ranges::from_parts(table, data, HashMap::new());
    ranges.finish();
    for (n, sat) in [(256, true), (257, false)] {
        let mut deps = DepSets::default();
        let mut theory = DatatypeTheory::default();
        let vs: Vec<DataVar> = (0..n).map(|_| theory.var()).collect();
        for &v in &vs {
            theory.add_range(v, byte, true, DepSetId::EMPTY);
        }
        for (i, &a) in vs.iter().enumerate() {
            for &b in &vs[i + 1..] {
                theory.add_not_equal(a, b, DepSetId::EMPTY);
            }
        }
        let components = theory.components(&mut deps);
        assert_eq!(components.len(), 1);
        assert_eq!(theory.min_cardinality(&components[0], &mut deps), n as u64);
        let verdict = theory.check(&components[0], &ranges, &mut deps);
        assert_eq!(matches!(verdict, Verdict::Sat { .. }), sat, "{n}");
    }
}
