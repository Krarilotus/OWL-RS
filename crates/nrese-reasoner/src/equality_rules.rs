//! Which rules the equality module stands in for: the equality rules of OWL 2 RL that conclude
//! facts, over `owl:sameAs`, known by what they say. Their one home is
//! [`super::rulesets::OWL2_RL`]: a rule is one of them if it is one there up to the names of
//! its variables and the order of its premises, `owl:sameAs` its only constant. A rule's name
//! is only its name.

use std::sync::OnceLock;

use super::ir::{Atom, Head, OWL, Rule, Term, Vocabulary, parse_rules};

/// A built-in equality rule: its name, its number of premises, its premises then conclusions.
type Model = (String, usize, Vec<Atom>);

/// The built-in equality rules, and the `owl:sameAs` id they were read with.
fn built_in() -> &'static (Vec<Model>, u64) {
    static RULES: OnceLock<(Vec<Model>, u64)> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut vocabulary = super::vocabulary::LocalVocabulary::default();
        let rules = parse_rules(super::rulesets::OWL2_RL, &mut vocabulary).expect("it parses");
        let same_as = vocabulary.iri(&format!("{OWL}sameAs"));
        let equality = rules.iter().filter(|r| r.name.starts_with("eq-"));
        let models =
            equality.filter_map(|r| Some((r.name.clone(), r.body.len(), atoms(r, same_as)?)));
        (models.collect(), same_as)
    })
}

/// A rule's premises, then its conclusions; `None` with guards, a `false` head or a constant
/// other than `same_as`.
fn atoms(rule: &Rule, same_as: u64) -> Option<Vec<Atom>> {
    let Head::Facts(head) = &rule.head else {
        return None;
    };
    let mut terms = rule.body.iter().chain(head).flat_map(|a| a.0);
    let constants = terms.all(|t| matches!(t, Term::Var(_)) || t == Term::Const(same_as));
    (rule.guards.is_empty() && constants).then(|| [&rule.body[..], &head[..]].concat())
}

/// The built-in equality rule `rule` is, over `same_as`: its name there.
pub fn equality_rule(rule: &Rule, same_as: u64) -> Option<&'static str> {
    let (models, theirs) = built_in();
    let mine = atoms(rule, same_as)?;
    let ids = (same_as, *theirs);
    let same = |(_, body, model): &&Model| {
        (rule.body.len(), mine.len()) == (*body, model.len()) && fits(&mine, model, *body, &[], ids)
    };
    models.iter().find(same).map(|(name, ..)| name.as_str())
}

/// Whether `name` is a built-in equality rule's.
pub fn named(name: &str) -> bool {
    built_in().0.iter().any(|(n, ..)| n == name)
}

/// `same_as`, if `rules` hold every built-in equality rule over it.
pub fn of(rules: &[Rule], same_as: u64) -> Option<u64> {
    let found: Vec<_> = rules.iter().map(|r| equality_rule(r, same_as)).collect();
    let held = |model: &'static Model| found.contains(&Some(model.0.as_str()));
    built_in().0.iter().all(held).then_some(same_as)
}

/// Whether `atoms` are `model` in some order, the first `body` of each (premises) among
/// themselves, their variables bound one-to-one after `bound` and their constants `ids`.
fn fits(atoms: &[Atom], model: &[Atom], body: usize, bound: &[(u8, u8)], ids: (u64, u64)) -> bool {
    let Some((first, rest)) = model.split_first() else {
        return true;
    };
    let candidates = if body > 0 { body } else { atoms.len() };
    (0..candidates).any(|i| {
        let mut tried = bound.to_vec();
        let here = atoms[i].0.iter().zip(first.0).all(|(&t, m)| match (t, m) {
            (Term::Var(v), Term::Var(w)) => {
                if !tried.iter().any(|p| p.0 == v || p.1 == w) {
                    tried.push((v, w));
                }
                tried.contains(&(v, w))
            }
            (t, m) => (t, m) == (Term::Const(ids.0), Term::Const(ids.1)),
        });
        let mut others = atoms.to_vec();
        others.remove(i);
        here && fits(&others, rest, body.saturating_sub(1), &tried, ids)
    })
}

#[cfg(test)]
mod tests {
    use super::super::ir::{Vocabulary, parse_rules};
    use super::super::vocabulary::LocalVocabulary;
    use super::equality_rule;

    /// Each built-in equality rule, its variables renamed and its premises in another order, is
    /// recognised as itself; a rule that says something else is none of them: the same shape
    /// over another property than `owl:sameAs`, a premise more or less, a conclusion and a
    /// premise swapped, two variables merged or one split.
    #[test]
    fn a_rule_is_the_built_in_equality_rule_it_says() {
        let cases = [
            ("(?b owl:sameAs ?a) -> (?a owl:sameAs ?b)", Some("eq-sym")),
            (
                "(?n owl:sameAs ?k), (?m owl:sameAs ?n) -> (?m owl:sameAs ?k)",
                Some("eq-trans"),
            ),
            (
                "(?x ?y ?z), (?x owl:sameAs ?w) -> (?w ?y ?z)",
                Some("eq-rep-s"),
            ),
            (
                "(?x ?y ?z), (?y owl:sameAs ?w) -> (?x ?w ?z)",
                Some("eq-rep-p"),
            ),
            (
                "(?x ?y ?z), (?z owl:sameAs ?w) -> (?x ?y ?w)",
                Some("eq-rep-o"),
            ),
            // eq-sym over another property.
            ("(?b rdfs:seeAlso ?a) -> (?a rdfs:seeAlso ?b)", None),
            // eq-rep-s with a premise more.
            (
                "(?s owl:sameAs ?t), (?s ?p ?o), (?o ?p ?s) -> (?t ?p ?o)",
                None,
            ),
            // eq-trans with its second premise left out (and `?z` its `?y`).
            ("(?x owl:sameAs ?y) -> (?x owl:sameAs ?y)", None),
            // eq-rep-s with its conclusion and its second premise swapped.
            ("(?t ?p ?o), (?s owl:sameAs ?t) -> (?s ?p ?o)", None),
            // eq-trans with `?y` and `?z` merged.
            (
                "(?x owl:sameAs ?y), (?y owl:sameAs ?y) -> (?x owl:sameAs ?y)",
                None,
            ),
            // eq-rep-s with `?s` split in two.
            ("(?s owl:sameAs ?t), (?s2 ?p ?o) -> (?t ?p ?o)", None),
        ];
        let mut wrong = Vec::new();
        for (text, expected) in cases {
            let mut vocabulary = LocalVocabulary::default();
            let rules = parse_rules(&format!("r: {text}"), &mut vocabulary).expect("it parses");
            let same_as = vocabulary.iri("http://www.w3.org/2002/07/owl#sameAs");
            let got = equality_rule(&rules[0], same_as);
            println!("CASE {text}: {got:?}, must {expected:?}");
            if got != expected {
                wrong.push(text);
            }
        }
        assert!(wrong.is_empty(), "wrong: {wrong:?}");
    }
}
