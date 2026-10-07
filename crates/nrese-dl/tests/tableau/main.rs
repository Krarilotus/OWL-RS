//! The hypertableau on hand-made ontologies whose answers are known: each construct the
//! engine handles, consistent and inconsistent, with every optimisation on and off.

mod build;
mod ni;
mod sat;

use build::Build;
use nrese_dl::tableau::{Answer, Config, consistency, satisfiable};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ObjProp, normalise};

/// The answer under every combination of the switches; they must agree.
fn answer(o: &nrese_owl::Ontology) -> Answer {
    let mut answers = Vec::new();
    for bits in 0..64u32 {
        let config = Config {
            semantic_branching: bits & 1 != 0,
            backjumping: bits & 2 != 0,
            anywhere_blocking: bits & 4 != 0,
            single_blocking: bits & 8 != 0,
            lazy_definitions: bits & 16 != 0,
            complements: bits & 32 != 0,
            portfolio: false,
            check_blocking: true,
            ..Config::default()
        };
        answers.push(consistency(o, &config).answer);
    }
    assert!(
        answers.windows(2).all(|w| w[0] == w[1]),
        "switches change the answer: {answers:?}"
    );
    answers.remove(0)
}

/// Definitions unfolded lazily: `¬A` as `¬D` wherever it occurs (directly, under a value
/// restriction, or through another definition); `answer` checks each with and without.
#[test]
fn lazily_unfolded_definitions_keep_answers() {
    let r = ObjProp::Named(300);
    // k_grz's shape: A ≡ ¬B ⊓ ¬C, D ≡ ∃r.A; D(a).
    let mut b = Build::default();
    let (a, bb, c, d) = (b.class(1), b.class(2), b.class(3), b.class(4));
    let (nb, nc) = (b.not(bb), b.not(c));
    let def_a = b.and(&[nb, nc]);
    b.equivalent(a, def_a);
    let def_d = b.some(r, a);
    b.equivalent(d, def_d);
    b.assert(d, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    // ... and D's successor can't be A: a clash only the forward direction finds.
    let nd = b.all(r, bb);
    b.assert(nd, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // A ≡ B ⊓ C with B(a), C(a): ¬A(a), A ⊑ ⊥, or E ≡ ¬A with E(a) each need B ⊓ C ⊑ A.
    for case in 0..3 {
        let mut b = Build::default();
        let (a, bb, c, e) = (b.class(1), b.class(2), b.class(3), b.class(5));
        let def_a = b.and(&[bb, c]);
        b.equivalent(a, def_a);
        b.assert(bb, 100);
        b.assert(c, 100);
        let na = b.not(a);
        match case {
            0 => b.assert(na, 100),
            1 => {
                let nothing = b.e(ClassExpr::Nothing);
                b.sub(a, nothing);
            }
            _ => {
                b.equivalent(e, na);
                b.assert(e, 100);
            }
        }
        assert_eq!(answer(&b.o), Answer::Inconsistent, "case {case}");
    }
}

#[test]
fn subsumption_and_disjointness() {
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    let nb = b.not(bb);
    b.sub(a, bb);
    b.sub(a, nb);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    b.sub(a, bb);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
}

#[test]
fn disjunctions_branch_and_backtrack() {
    // A ⊑ B ⊔ C, B ⊑ ⊥, C ⊑ D ⊔ E, D ⊑ ⊥, E ⊑ ⊥: a:A is inconsistent.
    let mut b = Build::default();
    let [a, bb, c, d, e] = [1, 2, 3, 4, 5].map(|t| b.class(t));
    let nothing = b.e(ClassExpr::Nothing);
    let bc = b.or(&[bb, c]);
    let de = b.or(&[d, e]);
    b.sub(a, bc);
    b.sub(bb, nothing);
    b.sub(c, de);
    b.sub(d, nothing);
    b.sub(e, nothing);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // Without E ⊑ ⊥ it is consistent.
    b.o.axioms.pop();
    b.o.axioms
        .retain(|x| !matches!(x, nrese_owl::Axiom::SubClassOf(s, _) if *s == e));
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
}

#[test]
fn backjumping_skips_unrelated_choices() {
    // Many independent disjunctions, then a clash that depends on none of them.
    let mut b = Build::default();
    let a = b.class(1);
    for i in 0..12 {
        let (x, y) = (b.class(10 + 2 * i), b.class(11 + 2 * i));
        let xy = b.or(&[x, y]);
        b.sub(a, xy);
    }
    let (p, q) = (b.class(50), b.class(51));
    let some = b.some(ObjProp::Named(200), p);
    b.sub(a, some);
    let nq = b.not(q);
    b.sub(p, q);
    let all = b.all(ObjProp::Named(200), nq);
    b.sub(a, all);
    b.assert(a, 100);
    let config = Config::default();
    let out = consistency(&b.o, &config);
    assert_eq!(out.answer, Answer::Inconsistent);
}

#[test]
fn existentials_and_blocking() {
    // A ⊑ ∃R.A: an infinite chain, cut by blocking.
    let mut b = Build::default();
    let a = b.class(1);
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    // ... and ∀R.¬A at the start: inconsistent.
    let na = b.not(a);
    let all = b.all(ObjProp::Named(200), na);
    let start = b.class(2);
    b.sub(start, all);
    b.assert(start, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn universal_restrictions_reach_asserted_successors() {
    let mut b = Build::default();
    let (a, bb) = (b.class(1), b.class(2));
    let all = b.all(ObjProp::Named(200), bb);
    b.sub(a, all);
    b.assert(a, 100);
    b.role_assertion(200, 100, 101);
    let nb = b.not(bb);
    b.assert(nb, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn inverses_propagate_back() {
    // A ⊑ ∃R.B, B ⊑ ∀R⁻.C, C ⊓ A ⊑ ⊥: a:A inconsistent.
    let mut b = Build::default();
    let [a, bb, c] = [1, 2, 3].map(|t| b.class(t));
    let some = b.some(ObjProp::Named(200), bb);
    b.sub(a, some);
    let back = b.all(ObjProp::Inverse(200), c);
    b.sub(bb, back);
    let both = b.and(&[a, c]);
    let nothing = b.e(ClassExpr::Nothing);
    b.sub(both, nothing);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn pairwise_blocking_with_inverses() {
    // A ⊑ ∃R.A, A ⊑ ∀R⁻.B... consistent; with B ⊑ ⊥ inconsistent only below the root.
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    let back = b.all(ObjProp::Inverse(200), bb);
    b.sub(a, back);
    b.assert(a, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    let nb = b.not(bb);
    b.sub(a, nb);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn functional_roles_merge() {
    // Functional R, a: ∃R.B ⊓ ∃R.C, B ⊓ C ⊑ ⊥.
    let mut b = Build::default();
    let [bb, c] = [2, 3].map(|t| b.class(t));
    let (sb, sc) = (
        b.some(ObjProp::Named(200), bb),
        b.some(ObjProp::Named(200), c),
    );
    let both = b.and(&[sb, sc]);
    b.assert(both, 100);
    b.characteristic(Characteristic::Functional, ObjProp::Named(200));
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.disjoint(bb, c);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn number_restrictions() {
    // ≥ 3 R ⊓ ≤ 2 R is unsatisfiable; ≥ 2 R ⊓ ≤ 2 R isn't.
    for (min, max, expected) in [(3, 2, Answer::Inconsistent), (2, 2, Answer::Consistent)] {
        for expand in [0, 2, 8] {
            let mut b = Build::default();
            let thing = b.e(ClassExpr::Thing);
            let at_least = b.e(ClassExpr::Min(min, ObjProp::Named(200), thing));
            let at_most = b.e(ClassExpr::Max(max, ObjProp::Named(200), thing));
            let both = b.and(&[at_least, at_most]);
            b.assert(both, 100);
            let config = Config {
                expand_at_most_up_to: expand,
                ..Config::default()
            };
            assert_eq!(
                consistency(&b.o, &config).answer,
                expected,
                "{min} {max} {expand}"
            );
        }
    }
}

#[test]
fn qualified_at_most_merges_by_filler() {
    // a: ≥2 R.B ⊓ ≤1 R.B is inconsistent; ≥2 R.B ⊓ ≤1 R.C is fine.
    for expand in [0, 2] {
        let mut b = Build::default();
        let bb = b.class(2);
        let at_least = b.e(ClassExpr::Min(2, ObjProp::Named(200), bb));
        let at_most = b.e(ClassExpr::Max(1, ObjProp::Named(200), bb));
        let both = b.and(&[at_least, at_most]);
        b.assert(both, 100);
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        assert_eq!(consistency(&b.o, &config).answer, Answer::Inconsistent);
        let mut b = Build::default();
        let [bb, c] = [2, 3].map(|t| b.class(t));
        let at_least = b.e(ClassExpr::Min(2, ObjProp::Named(200), bb));
        let at_most = b.e(ClassExpr::Max(1, ObjProp::Named(200), c));
        let both = b.and(&[at_least, at_most]);
        b.assert(both, 100);
        assert_eq!(consistency(&b.o, &config).answer, Answer::Consistent);
    }
}

#[test]
fn transitive_roles_through_automata() {
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    b.characteristic(Characteristic::Transitive, ObjProp::Named(200));
    let all = b.all(ObjProp::Named(200), bb);
    b.sub(a, all);
    b.assert(a, 100);
    b.role_assertion(200, 100, 101);
    b.role_assertion(200, 101, 102);
    let nb = b.not(bb);
    b.assert(nb, 102);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn individuals_and_equality() {
    let mut b = Build::default();
    let a = b.class(1);
    let na = b.not(a);
    b.assert(a, 100);
    b.assert(na, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.same(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    let mut b = Build::default();
    b.same(100, 101);
    b.different(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn nominals() {
    // A ⊑ {o}, a:A, b:A, a ≠ b: inconsistent.
    let mut b = Build::default();
    let a = b.class(1);
    let o = b.e(ClassExpr::OneOf(vec![300]));
    b.sub(a, o);
    b.assert(a, 100);
    b.assert(a, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.different(100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn class_satisfiability() {
    let mut b = Build::default();
    let [a, bb, c] = [1, 2, 3].map(|t| b.class(t));
    let nb = b.not(bb);
    b.sub(a, bb);
    b.sub(c, nb);
    let ac = b.and(&[a, c]);
    let d = b.class(4);
    b.equivalent(d, ac);
    let n = normalise(&b.o);
    let config = Config::default();
    assert_eq!(
        satisfiable(&b.o, &n, 4, &config).answer,
        Answer::Inconsistent
    );
    assert_eq!(satisfiable(&b.o, &n, 1, &config).answer, Answer::Consistent);
    // A class the clauses never mention.
    assert_eq!(
        satisfiable(&b.o, &n, 99, &config).answer,
        Answer::Consistent
    );
}

/// Data values are values: two literals of one value are one (`1` and `1.0`), two of
/// different values two.
#[test]
fn data_values_are_decided_by_the_datatype_theory() {
    let mut b = Build::default();
    b.axiom(Axiom::FunctionalDataProperty(400));
    let one = b.literal("1", "integer");
    let also_one = b.literal("1.0", "decimal");
    b.axiom(Axiom::DataPropertyAssertion(400, 100, one));
    b.axiom(Axiom::DataPropertyAssertion(400, 100, also_one));
    assert_eq!(answer(&b.o), Answer::Consistent);
    let float_one = b.literal("1", "float");
    let mut c = Build { o: b.o.clone() };
    c.axiom(Axiom::DataPropertyAssertion(400, 100, float_one));
    assert_eq!(answer(&c.o), Answer::Inconsistent, "a float isn't a real");
    let two = b.literal("2", "integer");
    b.axiom(Axiom::DataPropertyAssertion(400, 100, two));
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

/// Disjoint data properties compare values, not data nodes: the literal 10 of one and the
/// value `∃dp2.integer[10, 10]` makes for the other are two nodes with one value. W3C
/// "Inconsistent Disjoint Dataproperties" was answered consistent while the clause
/// shared one data variable (5 October 2026).
#[test]
fn disjoint_data_properties_compare_values() {
    for (bound, expected) in [(10, Answer::Inconsistent), (11, Answer::Consistent)] {
        let mut b = Build::default();
        b.axiom(Axiom::DisjointDataProperties(vec![400, 401]));
        let ten = b.literal("10", "integer");
        b.axiom(Axiom::DataPropertyAssertion(400, 100, ten));
        let range = b.restricted(
            "integer",
            &[("minInclusive", bound), ("maxInclusive", bound)],
        );
        let some = b.e(ClassExpr::DataSome(401, range));
        b.assert(some, 100);
        assert_eq!(answer(&b.o), expected, "dp2 in [{bound}, {bound}]");
    }
    // The same literal for both: one node, as before.
    let mut b = Build::default();
    b.axiom(Axiom::DisjointDataProperties(vec![400, 401]));
    let ten = b.literal("10", "integer");
    b.axiom(Axiom::DataPropertyAssertion(400, 100, ten));
    b.axiom(Axiom::DataPropertyAssertion(401, 100, ten));
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

/// The W3C test "Inconsistent String Pattern with Disjoint Dataproperties": `a(b|c)` has
/// two strings, and both are taken by the disjoint property; with one taken, the other
/// is left.
#[test]
fn patterns_count_their_strings() {
    for (taken, expected) in [
        (&["ab", "ac"][..], Answer::Inconsistent),
        (&["ab"][..], Answer::Consistent),
    ] {
        let mut b = Build::default();
        b.axiom(Axiom::DisjointDataProperties(vec![400, 401]));
        for s in taken {
            let l = b.literal(s, "string");
            b.axiom(Axiom::DataPropertyAssertion(400, 100, l));
        }
        let string = b.term(Some("http://www.w3.org/2001/XMLSchema#string".into()));
        let facet = b.term(Some("http://www.w3.org/2001/XMLSchema#pattern".into()));
        let pattern = b.literal("a(b|c)", "string");
        let range = b.range(nrese_owl::DataRange::Restriction(
            string,
            vec![(facet, pattern)],
        ));
        let some = b.e(ClassExpr::DataSome(401, range));
        b.assert(some, 100);
        assert_eq!(answer(&b.o), expected, "{taken:?}");
    }
}

/// Cardinalities against the values a range has: 256 bytes, but not 257.
#[test]
fn data_cardinalities_count_values() {
    for (n, expected) in [(256, Answer::Consistent), (257, Answer::Inconsistent)] {
        let mut b = Build::default();
        let byte = b.datatype("byte");
        b.axiom(Axiom::DataPropertyRange(400, byte));
        let literal = b.range(nrese_owl::DataRange::Literal);
        let at_least = b.e(ClassExpr::DataMin(n, 400, literal));
        b.assert(at_least, 100);
        assert_eq!(answer(&b.o), expected, "{n}");
    }
}

/// A disjunction whose every branch only a facet combination refutes: the clashes carry
/// the facts' dependencies, so the search backjumps soundly through both.
#[test]
fn facet_clashes_refute_each_branch() {
    for (lo, hi, expected) in [(4, 4, Answer::Inconsistent), (3, 4, Answer::Consistent)] {
        let mut b = Build::default();
        let (a, bb) = (b.class(1), b.class(2));
        let either = b.or(&[a, bb]);
        b.assert(either, 100);
        let big = b.restricted("integer", &[("minInclusive", 5)]);
        let small = b.restricted("integer", &[("maxInclusive", 3)]);
        let some_big = b.e(ClassExpr::DataSome(400, big));
        let some_small = b.e(ClassExpr::DataSome(400, small));
        b.sub(a, some_big);
        b.sub(bb, some_small);
        let range = b.restricted("integer", &[("minInclusive", lo), ("maxInclusive", hi)]);
        b.axiom(Axiom::DataPropertyRange(400, range));
        assert_eq!(answer(&b.o), expected, "[{lo}, {hi}]");
    }
}

/// A datatype outside the map is approximated: a model is no answer, a clash still is.
#[test]
fn approximated_datatypes_are_unsupported_not_consistent() {
    let mut b = Build::default();
    let date = b.datatype("date");
    b.axiom(Axiom::DataPropertyRange(400, date));
    let some = b.e(ClassExpr::DataSome(400, date));
    b.assert(some, 100);
    assert!(matches!(answer(&b.o), Answer::Unsupported(_)));
    let literal = b.range(nrese_owl::DataRange::Literal);
    let none = b.e(ClassExpr::DataMax(0, 400, literal));
    b.assert(none, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

/// Keys make named individuals with equal key values equal, never anonymous ones.
#[test]
fn keys_merge_named_individuals_only() {
    let base = |second: &str, anonymous: bool| {
        let mut b = Build::default();
        let (c, d) = (b.class(1), b.class(2));
        b.axiom(Axiom::HasKey(c, Vec::new(), vec![400]));
        let one = b.literal("1", "integer");
        let other = b.literal(second, "integer");
        b.assert(c, 100);
        b.assert(c, 101);
        b.axiom(Axiom::DataPropertyAssertion(400, 100, one));
        b.axiom(Axiom::DataPropertyAssertion(400, 101, other));
        // 100 is D, 101 isn't: equal, they clash.
        b.assert(d, 100);
        let not_d = b.not(d);
        b.assert(not_d, 101);
        if anonymous {
            b.o.anonymous.insert(101);
        }
        answer(&b.o)
    };
    assert_eq!(base("01", false), Answer::Inconsistent);
    assert_eq!(base("2", false), Answer::Consistent);
    assert_eq!(base("01", true), Answer::Consistent);
}

#[test]
fn budgets_give_up() {
    // ∃R.A with A ⊑ ∃R.B ⊓ ∃R.C ... grows; a tiny node budget gives up.
    let mut b = Build::default();
    let a = b.class(1);
    let some = b.some(ObjProp::Named(200), a);
    b.sub(a, some);
    b.assert(a, 100);
    let config = Config {
        max_nodes: 1,
        ..Config::default()
    };
    assert!(matches!(
        consistency(&b.o, &config).answer,
        Answer::GaveUp(why) if !why.contains("time budget")
    ));
}

#[test]
fn the_ni_rule_is_never_skipped_silently() {
    // JAIR 2009's caterpillar (8): S(a, a), a: ∃R.B, B ⊑ ∃R.C, C ⊑ ∃S.D, D ⊑ {a}, S
    // inverse-functional. Consistent; the derivation needs the NI rule (on c ≈ c).
    let mut b = Build::default();
    let [bb, c, d] = [2, 3, 4].map(|t| b.class(t));
    let (r, s) = (ObjProp::Named(200), ObjProp::Named(201));
    b.role_assertion(201, 100, 100);
    let rb = b.some(r, bb);
    b.assert(rb, 100);
    let rc = b.some(r, c);
    b.sub(bb, rc);
    let sd = b.some(s, d);
    b.sub(c, sd);
    let a = b.e(ClassExpr::OneOf(vec![100]));
    b.sub(d, a);
    b.characteristic(Characteristic::InverseFunctional, s);
    for expand in [0, 2] {
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        let answer = consistency(&b.o, &config).answer;
        eprintln!("caterpillar, expand {expand}: {answer:?}");
        assert_eq!(answer, Answer::Consistent);
    }
}

#[test]
fn premature_blocking_without_the_ni_rule_is_caught() {
    // JAIR 2009's (9): A(a), a: ∃R.B, A ⊑ ∀R⁻.⊥, B ⊑ ∃R.B, B ⊑ ∃S.{a}, R
    // inverse-functional, ⊤ ⊑ ≤3 S⁻.⊤. Inconsistent; without the NI rule a blocked chain
    // looks like a model (Figure 9a).
    let mut b = Build::default();
    let [a, bb] = [1, 2].map(|t| b.class(t));
    let (r, s) = (ObjProp::Named(200), ObjProp::Named(201));
    let thing = b.e(ClassExpr::Thing);
    let nothing = b.e(ClassExpr::Nothing);
    b.assert(a, 100);
    let rb = b.some(r, bb);
    b.assert(rb, 100);
    let none_back = b.all(ObjProp::Inverse(200), nothing);
    b.sub(a, none_back);
    b.sub(bb, rb);
    let at_a = b.e(ClassExpr::OneOf(vec![100]));
    let s_a = b.some(s, at_a);
    b.sub(bb, s_a);
    b.characteristic(Characteristic::InverseFunctional, r);
    let at_most = b.e(ClassExpr::Max(3, ObjProp::Inverse(201), thing));
    b.sub(thing, at_most);
    for expand in [0, 2, 3] {
        let config = Config {
            expand_at_most_up_to: expand,
            ..Config::default()
        };
        let answer = consistency(&b.o, &config).answer;
        eprintln!("premature blocking, expand {expand}: {answer:?}");
        assert_eq!(answer, Answer::Inconsistent, "{expand}");
    }
}

#[test]
fn the_bottom_property_relates_nothing() {
    // 500 is owl:bottomObjectProperty. a: ∃⊥.⊤ is inconsistent; so is an edge a
    // subproperty or a chain puts into it.
    let bottom = 500;
    let with_bottom = |b: &mut Build| b.o.builtin.bottom_object = Some(bottom);
    let mut b = Build::default();
    with_bottom(&mut b);
    let thing = b.e(ClassExpr::Thing);
    let some = b.some(ObjProp::Named(bottom), thing);
    b.assert(some, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);

    let mut b = Build::default();
    with_bottom(&mut b);
    b.axiom(Axiom::SubObjectPropertyOf(
        vec![ObjProp::Named(200)],
        ObjProp::Named(bottom),
    ));
    b.role_assertion(200, 100, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);

    let mut b = Build::default();
    with_bottom(&mut b);
    b.axiom(Axiom::SubObjectPropertyOf(
        vec![ObjProp::Named(200), ObjProp::Named(201)],
        ObjProp::Named(bottom),
    ));
    b.role_assertion(200, 100, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.role_assertion(201, 101, 102);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn the_universal_property_relates_everything() {
    // 501 is owl:topObjectProperty.
    let top = ObjProp::Named(501);
    let universal = |b: &mut Build| b.o.builtin.top_object = Some(501);
    // a: ¬∃U.⊤ is inconsistent (a is a U-successor of itself; New-Feature-TopObjectProperty-001).
    let mut b = Build::default();
    universal(&mut b);
    let thing = b.e(ClassExpr::Thing);
    let some = b.some(top, thing);
    let none = b.not(some);
    b.assert(none, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // a: ∀U.C reaches an unrelated b: ¬C.
    let mut b = Build::default();
    universal(&mut b);
    let c = b.class(1);
    let all = b.all(top, c);
    b.assert(all, 100);
    let nc = b.not(c);
    b.assert(nc, 101);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // ... but without b it holds, and a: ∃U.C with C ⊑ ∃R.C too.
    b.o.axioms.pop();
    b.o.sources.pop();
    let rc = b.some(ObjProp::Named(200), c);
    b.sub(c, rc);
    let some_c = b.some(top, c);
    b.assert(some_c, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    // D ⊑ ∃U.C with C ⊑ ⊥ makes D empty, not the ontology inconsistent; d: D does.
    let mut b = Build::default();
    universal(&mut b);
    let [c, d] = [1, 2].map(|t| b.class(t));
    let some_c = b.some(top, c);
    b.sub(d, some_c);
    let nothing = b.e(ClassExpr::Nothing);
    b.sub(c, nothing);
    let thing = b.e(ClassExpr::Thing);
    b.assert(thing, 100);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.assert(d, 100);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // ¬U(a, b) can't hold; U(a, b) always does.
    let mut b = Build::default();
    universal(&mut b);
    b.role_assertion(501, 100, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.axiom(Axiom::NegativeObjectPropertyAssertion(501, 100, 101));
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // A cardinality over U (OWL 2 DL forbids it: U isn't simple) is unsupported.
    let mut b = Build::default();
    universal(&mut b);
    let thing = b.e(ClassExpr::Thing);
    let two = b.e(ClassExpr::Max(2, top, thing));
    b.assert(two, 100);
    assert!(matches!(answer(&b.o), Answer::Unsupported(_)));
}

#[test]
fn horn_encodings_keep_their_answers() {
    // Man ⊔ Woman ⊑ Person, Person ⊓ ∃worksFor.Org ⊑ Employee with worksFor transitive;
    // a: Man, worksFor(a, b), worksFor(b, c), c: Org, Employee ⊑ ⊥: inconsistent, and
    // consistent without c: Org.
    let mut b = Build::default();
    let [man, woman, person, org, employee] = [1, 2, 3, 4, 5].map(|t| b.class(t));
    let works_for = ObjProp::Named(200);
    b.characteristic(Characteristic::Transitive, works_for);
    let either = b.or(&[man, woman]);
    b.sub(either, person);
    let some = b.some(works_for, org);
    let both = b.and(&[person, some]);
    b.sub(both, employee);
    let nothing = b.e(ClassExpr::Nothing);
    b.sub(employee, nothing);
    b.assert(man, 100);
    b.role_assertion(200, 100, 101);
    b.role_assertion(200, 101, 102);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.assert(org, 102);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn an_inverse_pair_with_a_transitive_role_is_regular() {
    // ore_ont_15971: after inverse of before, before transitive. a: ∀before.¬C,
    // before(a, b), after(c, b) (so before(b, c)), c: C is inconsistent; it was
    // "unsupported: an irregular role hierarchy".
    let mut b = Build::default();
    let c = b.class(1);
    let (before, after) = (ObjProp::Named(200), ObjProp::Named(201));
    b.axiom(Axiom::InverseObjectProperties(before, after));
    b.characteristic(Characteristic::Transitive, before);
    let nc = b.not(c);
    let all = b.all(before, nc);
    b.assert(all, 100);
    b.role_assertion(200, 100, 101);
    b.role_assertion(201, 102, 101);
    assert_eq!(answer(&b.o), Answer::Consistent);
    b.assert(c, 102);
    assert_eq!(answer(&b.o), Answer::Inconsistent);
    // ... and along after: c: ∀after.¬C, before(a, b), before(b, c), a: C.
    let mut b = Build::default();
    let c = b.class(1);
    b.axiom(Axiom::InverseObjectProperties(before, after));
    b.characteristic(Characteristic::Transitive, before);
    let nc = b.not(c);
    let all = b.all(after, nc);
    b.assert(all, 102);
    b.role_assertion(200, 100, 101);
    b.role_assertion(200, 101, 102);
    b.assert(c, 100);
    assert!(normalise(&b.o).unsupported.is_empty());
    assert_eq!(answer(&b.o), Answer::Inconsistent);
}

#[test]
fn a_branch_too_large_to_expand_is_abandoned_not_the_run() {
    // a: A, A ⊑ (C ⊓ ≥10⁹ R) ⊔ {o}: the first disjunct can't be expanded within a budget,
    // the second is a model (as in DL-909, which the search order decided).
    let mut b = Build::default();
    let [a, c] = [1, 2].map(|t| b.class(t));
    let thing = b.e(ClassExpr::Thing);
    let huge = b.e(ClassExpr::Min(1_000_000_000, ObjProp::Named(200), thing));
    let big = b.and(&[c, huge]);
    let o = b.e(ClassExpr::OneOf(vec![300]));
    let either = b.or(&[big, o]);
    b.sub(a, either);
    b.assert(a, 100);
    let config = Config {
        max_branch_points: Some(100_000),
        timeout: Some(std::time::Duration::from_secs(300)),
        max_memory: 256 << 20,
        ..Config::default()
    };
    assert_eq!(consistency(&b.o, &config).answer, Answer::Consistent);
    // With a ≠ o the other branch clashes: the run gives up, it doesn't refute.
    b.different(100, 300);
    assert!(matches!(
        consistency(&b.o, &config).answer,
        Answer::GaveUp(_)
    ));
}

/// Guard (the ≤-rule's merge filter, W3C DL-903): `a: ≥k p.A ⊓ ≥k q.B ⊓ ≤(2k-1) r` with
/// `p, q ⊑ r` and `A ⊓ B ⊑ ⊥` is inconsistent: the `p`- and the `q`-successors are
/// pairwise unequal among themselves, and a `p`- and a `q`-successor can't merge. Every
/// pair is ruled out before a choice, so the clash comes without a branch point
/// (DL-903, k = 200/300: a 60 s timeout before, 9 ms after); with the filter off the
/// ≤-rule branches over the k² mixed pairs.
#[test]
fn disjoint_successors_are_no_merge_candidates() {
    let k = 40;
    let mut b = Build::default();
    let (p, q, r) = (200, 201, 202);
    let [x, a, bb] = [1, 2, 3].map(|t| b.class(t));
    let thing = b.e(ClassExpr::Thing);
    let nothing = b.e(ClassExpr::Nothing);
    for s in [p, q] {
        b.axiom(Axiom::SubObjectPropertyOf(
            vec![ObjProp::Named(s)],
            ObjProp::Named(r),
        ));
    }
    let both = b.and(&[a, bb]);
    b.sub(both, nothing);
    let at_least_p = b.e(ClassExpr::Min(k, ObjProp::Named(p), a));
    let at_least_q = b.e(ClassExpr::Min(k, ObjProp::Named(q), bb));
    let at_most_r = b.e(ClassExpr::Max(2 * k - 1, ObjProp::Named(r), thing));
    let all = b.and(&[at_least_p, at_least_q, at_most_r]);
    b.sub(x, all);
    b.assert(x, 100);
    let config = Config {
        max_branch_points: Some(10_000),
        ..Config::default()
    };
    let out = consistency(&b.o, &config);
    assert_eq!(out.answer, Answer::Inconsistent, "{}", out.telemetry);
    assert_eq!(out.telemetry.branch_points, 0, "{}", out.telemetry);
    let off = consistency(
        &b.o,
        &Config {
            merge_filter: false,
            ..config
        },
    );
    assert!(off.telemetry.branch_points > 0, "{}", off.telemetry);
}

/// Guard (keyed role plans): `Aᵢ ⊑ ¬∃partOf.Bᵢ` over a transitive `partOf` with a
/// hierarchy of subroles gives each role a transition per axiom, nearly all failing their
/// first check; an edge tries only the plans its ends' concepts key (the fast suite's
/// dl-roles, 5,000 axioms: saturation 1.2-2.3 s -> 18-27 ms, the same 58,724 firings).
#[test]
fn an_edge_tries_only_the_plans_its_ends_key() {
    let mut b = Build::default();
    let (roles, axioms) = (20u64, 400u64);
    let part_of = 500;
    b.characteristic(Characteristic::Transitive, ObjProp::Named(part_of));
    for r in 1..roles {
        let parent = if r < 2 { part_of } else { 500 + r / 2 };
        if r % 2 == 0 {
            b.characteristic(Characteristic::Transitive, ObjProp::Named(500 + r));
        }
        b.axiom(Axiom::SubObjectPropertyOf(
            vec![ObjProp::Named(500 + r)],
            ObjProp::Named(parent),
        ));
    }
    for i in 0..axioms {
        let (a, bi) = (b.class(10_000 + i), b.class(20_000 + i));
        let some = b.some(ObjProp::Named(part_of), bi);
        b.disjoint(a, some);
        b.role_assertion(500 + 1 + i % (roles - 1), 30_000 + i, 40_000 + i);
        let next = b.class(20_000 + (i + 1) % axioms);
        b.assert(next, 40_000 + i);
    }
    let out = consistency(&b.o, &Config::default());
    assert_eq!(out.answer, Answer::Consistent, "{}", out.telemetry);
    assert!(
        out.telemetry.plans_tried <= 2 * out.telemetry.facts,
        "{}",
        out.telemetry
    );
}

/// The W3C integer-multiplication shape (DL-906, 907, 910) with `N`, `M` and the count
/// `K` of `d`'s r-predecessors: `{d} ≡ (= N p⁻) ≡ (= K r⁻)`, `CN ≡ ∃p.{d} ≡ (= M q⁻)`,
/// `NM ≡ ∃q.CN ≡ ∃r.{d}`; p, r and (where `q_functional`) q functional. Consistent iff
/// `K = N·M` (with q functional).
fn multiplication(n: u32, m: u32, k: u32, q_functional: bool) -> nrese_owl::Ontology {
    let mut b = Build::default();
    let (p, q, r) = (
        ObjProp::Named(200),
        ObjProp::Named(201),
        ObjProp::Named(202),
    );
    let only_d = b.class(1);
    let cn = b.class(2);
    let nm = b.class(3);
    let d = b.e(ClassExpr::OneOf(vec![100]));
    b.equivalent(only_d, d);
    let thing = b.e(ClassExpr::Thing);
    let n_p = b.e(ClassExpr::Exact(n, p.inverse(), thing));
    b.equivalent(only_d, n_p);
    let k_r = b.e(ClassExpr::Exact(k, r.inverse(), thing));
    b.equivalent(only_d, k_r);
    let to_d = b.some(p, only_d);
    b.equivalent(cn, to_d);
    let m_q = b.e(ClassExpr::Exact(m, q.inverse(), thing));
    b.equivalent(cn, m_q);
    let to_cn = b.some(q, cn);
    b.equivalent(nm, to_cn);
    let r_d = b.some(r, only_d);
    b.equivalent(nm, r_d);
    b.characteristic(Characteristic::Functional, p);
    b.characteristic(Characteristic::Functional, r);
    if q_functional {
        b.characteristic(Characteristic::Functional, q);
    }
    b.o
}

/// Guard (counted classes): `K ≠ N·M` is refuted by arithmetic before the search, with no
/// branch point (the merge search is pigeonhole-hard: N=2, M=3, K=7 gave up after 774 k
/// branch points in 30 s; DL-910 likewise). `K = N·M` stays consistent; without q
/// functional the product doesn't hold and nothing is refuted.
#[test]
fn class_sizes_refute_a_wrong_product() {
    let config = Config {
        max_branch_points: Some(20_000),
        ..Config::default()
    };
    let out = consistency(&multiplication(2, 3, 7, true), &config);
    assert_eq!(out.answer, Answer::Inconsistent, "{}", out.telemetry);
    assert_eq!(out.telemetry.branch_points, 0, "{}", out.telemetry);
    let off = consistency(
        &multiplication(2, 3, 7, true),
        &Config {
            counting: false,
            ..config.clone()
        },
    );
    assert!(off.telemetry.branch_points > 0, "{}", off.telemetry);
    let out = consistency(&multiplication(2, 3, 6, true), &config);
    assert_eq!(out.answer, Answer::Consistent, "{}", out.telemetry);
    let out = consistency(&multiplication(2, 3, 7, false), &config);
    assert_ne!(out.answer, Answer::Inconsistent, "{}", out.telemetry);
}

/// Guard (keyed role plans on a self-loop): a plan keyed by the edge's target must run
/// for `r(x, x)` too (the brute-force campaign, seed 96, case 206: a subsumption through
/// `∃r.Self` was missed where the keyed lookup stopped after the source). Here
/// `A ⊓ ∃r.Self ⊓ ∀r⁻.B`: `r(x, x)` with `∀r⁻.B(x)` (keyed by the target) gives `B(x)`,
/// which clashes with `A`. Padding universals make the role's keyed plans outnumber the
/// labels, so the lookup path runs.
#[test]
fn keyed_plans_run_on_a_self_loop() {
    let mut b = Build::default();
    let r = ObjProp::Named(200);
    let (a, bb) = (b.class(1), b.class(2));
    let own = b.e(ClassExpr::HasSelf(r));
    let back = b.all(r.inverse(), bb);
    let x = b.and(&[a, own, back]);
    let nothing = b.e(ClassExpr::Nothing);
    let ab = b.and(&[a, bb]);
    b.sub(ab, nothing);
    for i in 0..50 {
        let pad = b.class(100 + i);
        let pad_to = b.class(200 + i);
        let all = b.all(r, pad_to);
        b.sub(pad, all);
    }
    b.assert(x, 1000);
    let out = consistency(&b.o, &Config::default());
    assert_eq!(out.answer, Answer::Inconsistent, "{}", out.telemetry);
}
