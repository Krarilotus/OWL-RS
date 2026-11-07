//! The models a run hands back (`keep_model`), checked by the certificate checker's own
//! semantics rather than by the engine.

use nrese_dl::certificate::{candidate, check};
use nrese_dl::tableau::{Answer, Config, Model, consistency};
use nrese_owl::{ClassExpr, ObjProp, Ontology};

use super::build::Build;

/// Where a candidate the model offers (folded, and unravelled where that differs) isn't a
/// model of every axiom, as the checker reads them: the check's verdict, per candidate.
fn not_models(o: &Ontology, m: &Model, what: &str, wrong: &mut Vec<String>) {
    let folded = check(o, &candidate(o, m));
    if folded.is_err() {
        wrong.push(format!("{what}: folded {folded:?}: {m:?}"));
    }
    if let Some(u) = &m.unravelled {
        let unravelled = check(o, &candidate(o, u));
        if unravelled.is_err() {
            wrong.push(format!("{what}: unravelled {unravelled:?}: {u:?}"));
        }
    }
}

/// Review A-1503: a retracted edge between surviving nodes is no edge of the model. At `a`,
/// `(A ⊔ B)` before `(U ⊔ V)`; `A ⊑ ∃R.Self`, `A ⊑ ∃S.C`, `C ⊑ ⊥`, `B ⊑ ¬∃R.Self`; and
/// `∃T.W(a)` with `W ⊑ ∃T.W`, a chain that blocking cuts, so the unravelled candidate
/// differs from the folded one. Deciding disjunctions first, the search takes A (giving
/// `R(a, a)`) and then U; only the ≥-rule then finds C's clash, which depends on A alone.
/// Dynamic backtracking retracts A, keeps U and re-decides B, so the graph's live part has
/// `B(a)` and no `R(a, a)`. The model is `a ∈ B ∩ U` with its T-chain: by the semantics,
/// `R(a, a)` would break `B ⊑ ¬∃R.Self`. Chronological backtracking cuts the edge away and
/// must give a model as well: that row is a guard (it holds before the fix too). Each row
/// is reported on its own.
#[test]
fn a_retracted_edge_is_no_edge_of_the_model() {
    let mut b = Build::default();
    let (r, s, t) = (
        ObjProp::Named(200),
        ObjProp::Named(201),
        ObjProp::Named(202),
    );
    let (a, bb, u, v, c, w) = (
        b.class(1),
        b.class(2),
        b.class(3),
        b.class(4),
        b.class(5),
        b.class(6),
    );
    let ab = b.or(&[a, bb]);
    let uv = b.or(&[u, v]);
    b.assert(ab, 100);
    b.assert(uv, 100);
    let own = b.e(ClassExpr::HasSelf(r));
    b.sub(a, own);
    let some_c = b.some(s, c);
    b.sub(a, some_c);
    let nothing = b.e(ClassExpr::Nothing);
    b.sub(c, nothing);
    let not_own = b.not(own);
    b.sub(bb, not_own);
    let chain = b.some(t, w);
    b.assert(chain, 100);
    b.sub(w, chain);
    let mut wrong = Vec::new();
    for dynamic in [true, false] {
        let config = Config {
            disjunctions_first: true,
            dynamic_backtracking: dynamic,
            check_retraction: true,
            disjunct_learning: false,
            keep_model: true,
            lazy_definitions: false,
            complements: false,
            counting: false,
            portfolio: false,
            ..Config::default()
        };
        let out = consistency(&b.o, &config);
        let what = if dynamic { "dynamic" } else { "chronological" };
        if out.answer != Answer::Consistent {
            wrong.push(format!("{what}: {:?} ({})", out.answer, out.telemetry));
            continue;
        }
        // The schedule the case is about: one level retracted alone.
        if (out.telemetry.retractions > 0) != dynamic {
            wrong.push(format!("{what}: retractions ({})", out.telemetry));
        }
        let Some(m) = out.model else {
            wrong.push(format!("{what}: no model"));
            continue;
        };
        if m.unravelled.is_none() {
            wrong.push(format!("{what}: blocking left no unravelled candidate"));
        }
        not_models(&b.o, &m, what, &mut wrong);
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
