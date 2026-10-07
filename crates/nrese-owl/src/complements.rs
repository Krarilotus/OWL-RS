//! Complementary definitions (`Options::complements`): `A ≡ X` and `B ≡ Y` where `Y` is
//! `¬X` in every model (`≥ n R.F` and `≤ n−1 R.F`, `∃R.F` and `∀R.¬F`, the data forms, `X`
//! and `¬X`) give `B ≡ ¬A`, whatever else the ontology says. Each such pair becomes one
//! class and its negation:
//!
//! - the class whose only definition is the pair's (`B` where both qualify) is replaced by
//!   `¬A` in every other axiom, and its definition left out: `B` is then a name nothing
//!   constrains;
//! - `A ≡ X` is left out too where `X` restricts a property nothing else mentions to one
//!   successor in `⊤` (or `rdfs:Literal`): such an `A` is free, as any interpretation of
//!   `A` extends to the property (an edge to itself, or a value, for each element of `A`
//!   and none elsewhere; the reverse for `≤ 0`). Not for `≥ 2`: that needs two elements,
//!   which nominals can deny.
//!
//! Without this, `≤ 0 R ⊑ B` is the clause `⊤ → B ∨ ∃R`, a choice on every element: the
//! OWL Lite encoding of complements (`C ≡ ≥ 1 P`, `C.comp ≡ = 0 P`, as OilEd writes
//! them) puts 24 to 47 of them on each node of W3C DL-662 to 664, where the same formula
//! with `owl:complementOf` (DL-202) has none. Equisatisfiable; a model of the result says
//! nothing of the classes and properties left out, so it is for callers that read none.

use std::collections::{HashMap, HashSet};

use crate::mapping::Ontology;
use crate::model::{Axiom, ClassExpr, DataRange, ExprId, ObjProp, RangeId, Term};

/// What a restriction says, up to its polarity: at least `n` successors over the
/// property in the filler (`Not`: in the filler's complement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Object(ObjProp, Filler, u32),
    Data(Term, RangeId, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Filler {
    Is(ExprId),
    Not(ExprId),
}

impl Filler {
    fn of(o: &Ontology, x: ExprId) -> Self {
        match o.classes.get(x.0) {
            ClassExpr::Not(y) => Self::Not(*y),
            _ => Self::Is(x),
        }
    }

    fn complement(o: &Ontology, x: ExprId) -> Self {
        match o.classes.get(x.0) {
            ClassExpr::Not(y) => Self::Is(*y),
            _ => Self::Not(x),
        }
    }
}

/// `x` as a key and whether it says at least (`true`) or its negation (`false`).
fn key(o: &Ontology, x: ExprId) -> Option<(Key, bool)> {
    Some(match *o.classes.get(x.0) {
        ClassExpr::Not(y) => {
            let (k, positive) = key(o, y)?;
            (k, !positive)
        }
        ClassExpr::Some(r, f) => (Key::Object(r, Filler::of(o, f), 1), true),
        ClassExpr::Min(n, r, f) if n > 0 => (Key::Object(r, Filler::of(o, f), n), true),
        ClassExpr::Max(n, r, f) => (Key::Object(r, Filler::of(o, f), n.saturating_add(1)), false),
        ClassExpr::Exact(0, r, f) => (Key::Object(r, Filler::of(o, f), 1), false),
        ClassExpr::All(r, g) => (Key::Object(r, Filler::complement(o, g), 1), false),
        ClassExpr::DataSome(p, d) => (Key::Data(p, d, 1), true),
        ClassExpr::DataMin(n, p, d) if n > 0 => (Key::Data(p, d, n), true),
        ClassExpr::DataMax(n, p, d) => (Key::Data(p, d, n.saturating_add(1)), false),
        ClassExpr::DataExact(0, p, d) => (Key::Data(p, d, 1), false),
        ClassExpr::DataAll(p, d) => match o.ranges.get(d.0) {
            DataRange::Not(e) => (Key::Data(p, *e, 1), false),
            _ => return None,
        },
        _ => return None,
    })
}

/// A class's definition by a restriction: `(class, axiom index, key, at least)`.
struct Definition {
    class: Term,
    index: usize,
    key: Key,
    positive: bool,
}

/// `ontology` with its complementary definitions rewritten; `None` where it has none.
pub(crate) fn rewrite(ontology: &Ontology) -> Option<Ontology> {
    let class = |e: ExprId| match ontology.classes.get(e.0) {
        ClassExpr::Class(a) => Some(*a),
        _ => None,
    };
    let mut definitions: HashMap<Term, usize> = HashMap::new();
    let mut named: HashSet<Term> = HashSet::new();
    let mut by_key: HashMap<Key, (Vec<Definition>, Vec<Definition>)> = HashMap::new();
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        if let Axiom::DisjointUnion(a, _) = axiom {
            // Its class is a term, not an expression `¬A` could replace.
            named.insert(*a);
        }
        let Axiom::EquivalentClasses(xs) = axiom else {
            continue;
        };
        for &x in xs {
            if let Some(a) = class(x) {
                *definitions.entry(a).or_default() += 1;
            }
        }
        let (a, x) = match xs[..] {
            [x, y] => match (class(x), class(y)) {
                (Some(a), None) => (a, y),
                (None, Some(a)) => (a, x),
                _ => continue,
            },
            _ => continue,
        };
        if let Some((key, positive)) = key(ontology, x) {
            let d = Definition {
                class: a,
                index,
                key,
                positive,
            };
            let sides = by_key.entry(key).or_default();
            (if positive { &mut sides.0 } else { &mut sides.1 }).push(d);
        }
    }
    // Pairs in axiom order, each class in one pair at most.
    let mut pairs: Vec<(&Definition, &Definition)> = by_key
        .values()
        .filter_map(|(at_least, not)| Some((at_least.first()?, not.first()?)))
        .filter(|(a, b)| a.class != b.class)
        .collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by_key(|(a, _)| a.index);
    let mentions = mention_counts(ontology);
    let mut o = ontology.clone();
    let thing = ExprId(o.classes.intern(ClassExpr::Thing));
    let mut used: HashSet<Term> = HashSet::new();
    let mut replaced: HashMap<Term, ExprId> = HashMap::new();
    let mut left_out: HashSet<usize> = HashSet::new();
    for (a, b) in pairs {
        if used.contains(&a.class) || used.contains(&b.class) {
            continue;
        }
        let only =
            |d: &Definition| definitions.get(&d.class) == Some(&1) && !named.contains(&d.class);
        // `gone` becomes `¬kept`; `B`, the at-most side, where both qualify.
        let (gone, kept) = if only(b) {
            (b, a)
        } else if only(a) {
            (a, b)
        } else {
            continue;
        };
        debug_assert!(gone.positive != kept.positive);
        used.extend([a.class, b.class]);
        let kept_class = ExprId(o.classes.intern(ClassExpr::Class(kept.class)));
        replaced.insert(
            gone.class,
            ExprId(o.classes.intern(ClassExpr::Not(kept_class))),
        );
        left_out.insert(gone.index);
        if free(ontology, kept.key, &mentions) {
            left_out.insert(kept.index);
        }
    }
    if replaced.is_empty() {
        return None;
    }
    let mut memo: HashMap<ExprId, ExprId> = HashMap::new();
    for index in 0..o.axioms.len() {
        o.axioms[index] = if left_out.contains(&index) {
            Axiom::SubClassOf(thing, thing)
        } else {
            let axiom = o.axioms[index].clone();
            map_axiom(&axiom, &mut |e| substitute(&mut o, e, &replaced, &mut memo))
        };
    }
    Some(o)
}

/// `e` with each class in `replaced` replaced by its expression.
fn substitute(
    o: &mut Ontology,
    e: ExprId,
    replaced: &HashMap<Term, ExprId>,
    memo: &mut HashMap<ExprId, ExprId>,
) -> ExprId {
    if let Some(&done) = memo.get(&e) {
        return done;
    }
    let expr = o.classes.get(e.0).clone();
    if let ClassExpr::Class(a) = expr {
        return replaced.get(&a).copied().unwrap_or(e);
    }
    let mut one = |o: &mut Ontology, x: ExprId| substitute(o, x, replaced, memo);
    let rebuilt = match expr {
        ClassExpr::And(xs) => {
            let xs = xs.iter().map(|&x| one(o, x)).collect();
            ClassExpr::And(crate::model::canonical(xs))
        }
        ClassExpr::Or(xs) => {
            let xs = xs.iter().map(|&x| one(o, x)).collect();
            ClassExpr::Or(crate::model::canonical(xs))
        }
        ClassExpr::Not(x) => ClassExpr::Not(one(o, x)),
        ClassExpr::Some(r, x) => ClassExpr::Some(r, one(o, x)),
        ClassExpr::All(r, x) => ClassExpr::All(r, one(o, x)),
        ClassExpr::Min(n, r, x) => ClassExpr::Min(n, r, one(o, x)),
        ClassExpr::Max(n, r, x) => ClassExpr::Max(n, r, one(o, x)),
        ClassExpr::Exact(n, r, x) => ClassExpr::Exact(n, r, one(o, x)),
        _ => return e,
    };
    let out = ExprId(o.classes.intern(rebuilt));
    memo.insert(e, out);
    out
}

/// `axiom` with `f` applied to each class expression it holds.
fn map_axiom(axiom: &Axiom, f: &mut dyn FnMut(ExprId) -> ExprId) -> Axiom {
    match axiom {
        Axiom::SubClassOf(a, b) => {
            let a = f(*a);
            Axiom::SubClassOf(a, f(*b))
        }
        Axiom::EquivalentClasses(xs) => {
            Axiom::EquivalentClasses(xs.iter().map(|&x| f(x)).collect())
        }
        Axiom::DisjointClasses(xs) => Axiom::DisjointClasses(xs.iter().map(|&x| f(x)).collect()),
        Axiom::DisjointUnion(a, xs) => Axiom::DisjointUnion(*a, xs.iter().map(|&x| f(x)).collect()),
        Axiom::ObjectPropertyDomain(r, x) => Axiom::ObjectPropertyDomain(*r, f(*x)),
        Axiom::ObjectPropertyRange(r, x) => Axiom::ObjectPropertyRange(*r, f(*x)),
        Axiom::DataPropertyDomain(d, x) => Axiom::DataPropertyDomain(*d, f(*x)),
        Axiom::HasKey(x, rs, ds) => Axiom::HasKey(f(*x), rs.clone(), ds.clone()),
        Axiom::ClassAssertion(x, i) => Axiom::ClassAssertion(f(*x), *i),
        other => other.clone(),
    }
}

/// Whether a restriction by `key` can be left out of its definition: one successor in `⊤`
/// (or a value), by a property that isn't built in and that only the pair's two
/// definitions mention.
fn free(o: &Ontology, key: Key, mentions: &HashMap<Term, usize>) -> bool {
    let b = o.builtin;
    let builtin = [b.top_object, b.bottom_object, b.top_data, b.bottom_data];
    let (property, top) = match key {
        Key::Object(r, Filler::Is(f), 1) => {
            (r.named(), matches!(o.classes.get(f.0), ClassExpr::Thing))
        }
        Key::Data(p, d, 1) => (p, matches!(o.ranges.get(d.0), DataRange::Literal)),
        _ => return false,
    };
    top && !builtin.contains(&Some(property)) && mentions.get(&property) == Some(&2)
}

/// How many axioms mention each property.
fn mention_counts(o: &Ontology) -> HashMap<Term, usize> {
    let seen = std::cell::RefCell::new(HashSet::new());
    let mut counts: HashMap<Term, usize> = HashMap::new();
    for axiom in &o.axioms {
        crate::properties::mentions(o, axiom, &|t| {
            seen.borrow_mut().insert(t);
            false
        });
        for t in seen.borrow_mut().drain() {
            *counts.entry(t).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalise::{Options, normalise_with};

    /// The clauses that hold of every element with a choice: an empty body, two heads or
    /// more.
    fn on_every_node(o: &Ontology) -> usize {
        let options = Options {
            complements: true,
            ..Options::default()
        };
        normalise_with(o, options)
            .clauses
            .iter()
            .filter(|c| c.body.is_empty() && c.head.len() > 1)
            .count()
    }

    fn e(o: &mut Ontology, x: ClassExpr) -> ExprId {
        ExprId(o.classes.intern(x))
    }

    /// Whether axiom `i` of `r` holds `¬class` somewhere.
    fn negates(r: &Ontology, i: usize, class: Term) -> bool {
        fn has(r: &Ontology, x: ExprId, class: Term) -> bool {
            match r.classes.get(x.0) {
                ClassExpr::Not(y) => {
                    r.classes.get(y.0) == &ClassExpr::Class(class) || has(r, *y, class)
                }
                ClassExpr::Some(_, y) | ClassExpr::All(_, y) => has(r, *y, class),
                ClassExpr::And(ys) | ClassExpr::Or(ys) => ys.iter().any(|&y| has(r, y, class)),
                _ => false,
            }
        }
        let Axiom::EquivalentClasses(xs) = &r.axioms[i] else {
            return false;
        };
        xs.iter().any(|&x| has(r, x, class))
    }

    fn left_out(r: &Ontology, i: usize) -> bool {
        matches!(&r.axioms[i], Axiom::SubClassOf(a, b) if a == b)
    }

    fn push(o: &mut Ontology, a: Axiom) {
        o.axioms.push(a);
        o.sources.push(Vec::new());
    }

    /// OilEd's complement: `C ≡ ≥ 1 P`, `C.comp ≡ = 0 P`, `C.comp` used under a role.
    fn oiled(o: &mut Ontology, c: Term, comp: Term, p: Term, r: Term, d: Term) {
        let thing = e(o, ClassExpr::Thing);
        let (cc, compc, dc) = (
            e(o, ClassExpr::Class(c)),
            e(o, ClassExpr::Class(comp)),
            e(o, ClassExpr::Class(d)),
        );
        let min = e(o, ClassExpr::Min(1, ObjProp::Named(p), thing));
        let zero = e(o, ClassExpr::Exact(0, ObjProp::Named(p), thing));
        let some = e(o, ClassExpr::Some(ObjProp::Named(r), compc));
        push(o, Axiom::EquivalentClasses(vec![cc, min]));
        push(o, Axiom::EquivalentClasses(vec![compc, zero]));
        push(o, Axiom::EquivalentClasses(vec![dc, some]));
    }

    /// Guard (docs/design/performance.md §0): OilEd's complements leave no choice on
    /// every element (W3C DL-662 to 664 had 24 to 47 per node).
    #[test]
    fn complementary_definitions_leave_no_choice_on_every_node() {
        let mut o = Ontology::default();
        oiled(&mut o, 1, 2, 3, 4, 5);
        oiled(&mut o, 11, 12, 13, 4, 15);
        assert_eq!(on_every_node(&o), 0);
        let plain = normalise_with(&o, Options::default()).clauses;
        assert_eq!(
            plain
                .iter()
                .filter(|c| c.body.is_empty() && c.head.len() > 1)
                .count(),
            2
        );
    }

    /// A property that something else mentions keeps its definition (`C ≡ ∃P.⊤` is then
    /// Horn both ways); `C.comp` still becomes `¬C` where it is used.
    #[test]
    fn a_property_used_elsewhere_keeps_its_definition() {
        let mut o = Ontology::default();
        oiled(&mut o, 1, 2, 3, 4, 5);
        let x = e(&mut o, ClassExpr::Class(6));
        push(&mut o, Axiom::ObjectPropertyDomain(ObjProp::Named(3), x));
        let r = rewrite(&o).expect("a pair");
        assert!(matches!(&r.axioms[0], Axiom::EquivalentClasses(_)));
        assert!(left_out(&r, 1) && negates(&r, 2, 1), "{:?}", r.axioms);
        assert_eq!(on_every_node(&o), 0);
    }

    /// `∃R.F` and `∀R.¬F`, and `≥ 2` against `≤ 1`, are complements too; a filler other than
    /// `⊤`, or more than one successor, keeps the kept side's definition.
    #[test]
    fn existentials_and_counts_pair_with_their_complements() {
        let mut o = Ontology::default();
        let (a, b, c, d, f) = (1, 2, 3, 4, 5);
        let fc = e(&mut o, ClassExpr::Class(f));
        let not_f = e(&mut o, ClassExpr::Not(fc));
        let some = e(&mut o, ClassExpr::Some(ObjProp::Named(9), fc));
        let all = e(&mut o, ClassExpr::All(ObjProp::Named(9), not_f));
        let thing = e(&mut o, ClassExpr::Thing);
        let two = e(&mut o, ClassExpr::Min(2, ObjProp::Named(8), thing));
        let one = e(&mut o, ClassExpr::Max(1, ObjProp::Named(8), thing));
        for (x, y) in [(a, some), (b, all), (c, two), (d, one)] {
            let xc = e(&mut o, ClassExpr::Class(x));
            push(&mut o, Axiom::EquivalentClasses(vec![xc, y]));
        }
        let r = rewrite(&o).expect("two pairs");
        assert!(left_out(&r, 1) && left_out(&r, 3), "{:?}", r.axioms);
        assert!(matches!(&r.axioms[0], Axiom::EquivalentClasses(_)));
        assert!(matches!(&r.axioms[2], Axiom::EquivalentClasses(_)));
        assert_eq!(on_every_node(&o), 0);
    }
}
