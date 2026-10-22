//! Semantic witnesses for the normaliser's fresh-definition repair, under the existing
//! search and preprocessing switches. The engine is only a consumer of the repair.

use super::{Build, answer};
use nrese_dl::tableau::Answer;
use nrese_owl::{ClassExpr, ObjProp};

#[test]
fn wide_definitions_cannot_drop_named_or_existential_conjuncts() {
    for width in [15, 16, 17] {
        let mut b = Build::default();
        let (a, bb) = (b.class(1), b.class(2));
        let r = ObjProp::Named(300);
        let mut parts = vec![bb];
        let mut last = bb;
        for i in 1..width {
            last = b.class(10 + i);
            parts.push(b.some(r, last));
        }
        let definition = b.and(&parts);
        b.equivalent(a, definition);
        b.assert(a, 200);
        assert_eq!(answer(&b.o), Answer::Consistent, "width {width}");
        let baseline = b.o.clone();
        let not_b = b.not(bb);
        b.assert(not_b, 200);
        assert_eq!(answer(&b.o), Answer::Inconsistent, "B at width {width}");
        b.o = baseline;
        let not_last = b.not(last);
        let forbid_last = b.all(r, not_last);
        b.assert(forbid_last, 200);
        assert_eq!(answer(&b.o), Answer::Inconsistent, "last at width {width}");
    }
}

#[test]
fn wide_negative_union_fillers_reject_each_disjunct() {
    for width in [16, 17] {
        let mut b = Build::default();
        let parts: Vec<_> = (1..=width).map(|t| b.class(t)).collect();
        let union = b.or(&parts);
        let r = ObjProp::Named(300);
        let none = b.e(ClassExpr::Max(0, r, union));
        b.assert(none, 200);
        assert_eq!(answer(&b.o), Answer::Consistent, "width {width}");
        let baseline = b.o.clone();
        for &part in &parts {
            b.o = baseline.clone();
            let some = b.some(r, part);
            b.assert(some, 200);
            assert_eq!(
                answer(&b.o),
                Answer::Inconsistent,
                "{part:?}, width {width}"
            );
        }
    }
}
