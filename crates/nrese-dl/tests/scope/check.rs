//! The small-scope check's procedure (`main.rs`; `../scope_one`): every case of the scope
//! answered as the recorded references, and the certificates counted.

use nrese_owl::scope::{self, Ax, Feature, Scope};
use rayon::prelude::*;

const SCOPE: &str = "3,3";

/// The reference answers, two base-64 digits a case (scope.py's `encode`).
const REFERENCE: &str = include_str!("reference-3-3.txt");

const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn decode(text: &str) -> Vec<String> {
    let digits: Vec<usize> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| ALPHABET.find(c).expect("a base-64 digit"))
        .collect();
    digits
        .chunks(2)
        .map(|pair| match pair[0] * 64 + pair[1] {
            1024 => "i".to_owned(),
            n => format!("c{n:010b}"),
        })
        .collect()
}

/// Whether claim `k` of a consistent answer is one a model shows.
fn certifiable(code: &str, k: usize) -> bool {
    if k == 0 {
        return true;
    }
    let bit = code.as_bytes()[k];
    if k <= 2 { bit == b'1' } else { bit == b'0' }
}

/// The features of data values (the certificate checker declines them).
fn data_features() -> u128 {
    use Feature as F;
    [
        F::DataHasValue,
        F::DataSome,
        F::DataAll,
        F::DataMin,
        F::DataMax,
        F::DataExact,
        F::TopData,
        F::BottomData,
        F::Literal,
        F::Integer,
        F::String,
        F::Defined,
        F::DataOneOf,
        F::Facet,
        F::DataNot,
        F::DataAnd,
        F::DataOr,
        F::SubDataPropertyOf,
        F::EquivalentDataProperties,
        F::DisjointDataProperties,
        F::DataPropertyDomain,
        F::DataPropertyRange,
        F::FunctionalDataProperty,
        F::DatatypeDefinition,
        F::DataPropertyAssertion,
        F::NegativeDataPropertyAssertion,
    ]
    .iter()
    .fold(0, |m, f| m | f.bit())
}

/// The cases of at most `axioms` axioms; at most `uncertified` claims without a checked
/// model.
pub fn check(axioms: usize, uncertified: usize) {
    let scope = Scope::parse(SCOPE).expect("scope");
    let t = scope.tables();
    let (u, names) = scope::universe();
    let data = data_features();
    let all = scope::cases(&scope, &t).count();
    let cases: Vec<(u64, String, bool)> = scope::cases(&scope, &t)
        .filter(|c| c.axioms.len() <= axioms)
        .map(|c| {
            let with_data = c.axioms.iter().any(|&a| {
                t.axiom_features(a) & data != 0
                    || matches!(t.axioms[a as usize], Ax::HasKey(_, _, true))
            });
            let o = u.ontology(&t, &c);
            (c.index, scope::document(&o, c.index, &names), with_data)
        })
        .collect();
    let expected = decode(REFERENCE);
    assert_eq!(
        expected.len(),
        all,
        "the enumeration changed: run the references and record their answers again \
         (benches/reasoning/dl/scope.py run, record)"
    );
    let results: Vec<(Option<String>, usize, usize)> = cases
        .par_iter()
        .map(|(index, document, with_data)| {
            let (o, case) = match nrese_dl::scope::read(document) {
                Ok(read) => read,
                Err(why) => return (Some(format!("case {index}: not read: {why}")), 0, 0),
            };
            // The limit the recorded answers were made under: a case past it is `t`.
            let limit = Some(std::time::Duration::from_secs(20));
            let answer = nrese_dl::scope::answer(&o, &case, limit);
            let want = &expected[*index as usize];
            if answer.code != *want {
                return (
                    Some(format!(
                        "case {index}: NRESE {}, the references {want}\n  {document}",
                        answer.code
                    )),
                    0,
                    0,
                );
            }
            if *with_data || !answer.code.starts_with('c') {
                return (None, 0, 0);
            }
            let claims = (0..11).filter(|&k| certifiable(&answer.code, k));
            let (mut shown, mut total) = (0, 0);
            for k in claims {
                total += 1;
                shown += usize::from(answer.certified.as_bytes()[k] == b'm');
            }
            (None, shown, total)
        })
        .collect();
    let failures: Vec<&String> = results.iter().filter_map(|(f, ..)| f.as_ref()).collect();
    let shown: usize = results.iter().map(|r| r.1).sum();
    let total: usize = results.iter().map(|r| r.2).sum();
    eprintln!(
        "{} cases; without data values, {shown} of {total} claims a model can show are shown",
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} cases differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        total - shown <= uncertified,
        "{} claims without a checked model (at most {uncertified})",
        total - shown
    );
}
