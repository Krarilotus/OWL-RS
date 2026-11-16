//! U1, the upper bound off the RL route, on inputs longer than one rule may hold: a rule
//! numbers its variables and atoms by `u8`. U1 must use such an input or withhold its
//! certification (no answer `complete`), never panic and never fail the commit.

use super::queries::query;
use super::{insert, pipeline};

/// A union on the right of a subclass axiom: outside OWL 2 RL, so U1 and the DL engines.
const OFF_RL: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .";

/// The message of a caught panic.
fn message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

/// Inserts `data` off the RL route and asks `ask`: what fails, or `None` where the answer
/// is true or not `complete`.
fn verdict(data: &str, ask: &str) -> Option<String> {
    let run = std::panic::catch_unwind(|| {
        let dl = pipeline();
        insert(&dl, &format!("{OFF_RL} {data}")).map_err(|error| error.to_string())?;
        let (rows, status) = query(&dl, ask);
        Ok::<_, String>((rows, status))
    });
    match run {
        Ok(Ok((rows, _))) if rows == ["true"] => None,
        Ok(Ok((_, status))) if !status.is_complete() => None,
        Ok(Ok((rows, status))) => Some(format!("{rows:?}, {}", status.shared.as_str())),
        Ok(Err(error)) => Some(format!("the commit failed: {error}")),
        Err(payload) => Some(format!("panicked: {}", message(&*payload))),
    }
}

/// prp-key (OWL 2 Structural Specification §9.5): two `:K` with the same value on every key
/// property are one individual. U1's key rule has two atoms per property.
#[test]
fn a_long_key_off_the_rules_route_is_used_or_not_certified() {
    let mut wrong = Vec::new();
    for n in [127, 128, 251] {
        let members: Vec<String> = (1..=n).map(|i| format!(":k{i}")).collect();
        let mut data = format!(":K owl:hasKey ( {} ) .", members.join(" "));
        for x in ["x", "y"] {
            data.push_str(&format!(" :{x} a :K"));
            for i in 1..=n {
                data.push_str(&format!(" ; :k{i} :v{i}"));
            }
            data.push_str(" .");
        }
        if let Some(why) = verdict(&data, "ASK { :x owl:sameAs :y }") {
            wrong.push(format!("{n} properties: {why}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `∃r1.⊤ ⊓ … ⊓ ∃rn.⊤ ⊑ ⊥`, `:E ⊑ ∃ri.⊤` for each `i`, and `:x a :E`. The first axiom
/// is one DL clause whose body has a successor per existential: `n + 1` variables, so 256
/// existentials need 257, in 256 atoms. Hand proof: in any model `:x` has an
/// `ri`-successor for each `i` (`:E ⊑ ∃ri.⊤`), so `:x ∈ ∃r1.⊤ ⊓ … ⊓ ∃rn.⊤ ⊑ ⊥`, which no
/// element is: the data has no model, and the commit that makes it so must be rejected.
/// (An existential on the right is outside OWL 2 RL, so U1 takes part.) A commit
/// accepted must leave the verdict short of `consistent`; never a panic.
#[test]
fn a_wide_class_off_the_rules_route_is_used_or_not_certified() {
    let mut wrong = Vec::new();
    for n in [127, 256] {
        let members: Vec<String> = (1..=n)
            .map(|i| format!("[ owl:onProperty :r{i} ; owl:someValuesFrom owl:Thing ]"))
            .collect();
        let mut data = format!(
            "[ owl:intersectionOf ( {} ) ] rdfs:subClassOf owl:Nothing . :x a :E .",
            members.join(" ")
        );
        for member in &members {
            data.push_str(&format!(" :E rdfs:subClassOf {member} ."));
        }
        let run = std::panic::catch_unwind(|| {
            let dl = pipeline();
            let commit = insert(&dl, &data).map_err(|error| error.to_string());
            let status = dl.store().dl().status();
            let verdict = status.map(|s| s.consistency.verdict.as_str().to_owned());
            (commit, verdict)
        });
        match run {
            Ok((Err(why), _)) => eprintln!("{n} existentials: rejected: {why}"),
            Ok((Ok(_), Some(verdict))) if verdict != "consistent" => {}
            Ok(outcome) => wrong.push(format!("{n} existentials: accepted, {outcome:?}")),
            Err(payload) => wrong.push(format!(
                "{n} existentials: panicked: {}",
                message(&*payload)
            )),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
