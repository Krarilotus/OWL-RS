//! FC1-F1 temporal activation: old data, new schema, one retained semantic rule.
//! The review's original example was UNRUN; these assert the executable phases.
use nrese_reasoner as r;
use r::eval::{Seg, Source};
use r::ir::{Atom, Head, Rule, Term, Triple, Vocabulary};
use r::vocabulary::LocalVocabulary;
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::Relaxed},
};

struct Fixture {
    rules: Vec<Rule>,
    schema: r::eval::Schema,
    data: Triple,
    added: Triple,
    want: Triple,
    never: u64,
}
fn fixture(grouped: bool) -> Fixture {
    let mut v = LocalVocabulary::default();
    let mut rules = r::ir::parse_rules(
        "eq-sym: (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)\n\
         eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)\n\
         eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)\n\
         eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)\n\
         eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)",
        &mut v,
    )
    .unwrap();
    assert_eq!(rules.len(), 5);
    let input = v.iri("urn:input");
    let output = v.iri("urn:out");
    rules.push(Rule {
        name: "eq-rep-p".into(),
        body: vec![Atom([Term::Var(0), Term::Const(input), Term::Var(1)])],
        guards: vec![],
        head: Head::Facts(vec![Atom([
            Term::Var(0),
            Term::Const(output),
            Term::Var(1),
        ])]),
    });
    assert_eq!(rules.len(), 6);
    assert!(rules.iter().all(|r| matches!(r.head, Head::Facts(_))));
    if grouped {
        let extra = r::RuleProgram::new(
            None,
            Some(Arc::new(
                r::UserRules::pie(
                    "fc1.pie",
                    r#"Prefices {}
Rules {
Id: eq-rep-p
<urn:decl> <http://www.w3.org/2000/01/rdf-schema#domain> <urn:class>
x <urn:input> y
---
x <urn:never> y [Constraint x != x]
x <urn:out> y
}
"#,
                )
                .unwrap(),
            )),
        )
        .unwrap()
        .rules(&mut v)
        .unwrap();
        assert_eq!(extra.len(), 1);
        assert!(matches!(extra[0].head, Head::Grouped(_)));
        // Two grouped origins reaching the same residual must still issue one event.
        let mut alias = extra[0].clone();
        alias.name = "eq-rep-o".into();
        rules.extend(extra);
        rules.push(alias);
    }
    let [s, o, input, out, decl, class, never] = [
        "urn:s",
        "urn:o",
        "urn:input",
        "urn:out",
        "urn:decl",
        "urn:class",
        "urn:never",
    ]
    .map(|x| v.iri(x));
    let domain = v.iri("http://www.w3.org/2000/01/rdf-schema#domain");
    Fixture {
        rules,
        schema: r::batch::Schema::owl(&mut v),
        data: [s, input, o],
        added: [decl, domain, class],
        want: [s, out, o],
        never,
    }
}
fn spec(f: &Fixture) -> r::delta::Rules<'_> {
    r::delta::Rules {
        rules: &f.rules,
        lists: None,
        schema: &f.schema,
    }
}
fn cache(f: &Fixture) -> r::eval::GroundProgram {
    let cold = r::batch::materialise(&[f.data], &f.rules, None, &f.schema);
    assert!(
        cold.derived.is_empty(),
        "old ordinary name contract is deliberate"
    );
    r::delta::program(&r::delta::MemoryBase::new(&[f.data], &[]), spec(f))
}
fn residual(p: &r::eval::GroundProgram, f: &Fixture) -> usize {
    let found:Vec<_>=p.rules.iter().enumerate().filter(|(_,rule)|rule.name=="eq-rep-p"
        &&rule.body.len()==1&&matches!(&rule.head,Head::Facts(h) if h.len()==1&&h[0].0[1]==r::ir::Term::Const(f.want[1]))).map(|(r,_)|r).collect();
    assert_eq!(found.len(), 1, "one retained ordinary residual");
    found[0]
}
fn set(f: &[Triple]) -> HashSet<Triple> {
    f.iter().copied().collect()
}

#[test]
fn fc1_schema_upgrade_evaluates_old_data() {
    let f = fixture(true);
    let old = cache(&f);
    let index = residual(&old, &f);
    let old_key = r::eval::rule_key(&old.rules[index]);
    let asserted = [f.data, f.added];
    let cold = r::batch::materialise(&asserted, &f.rules, None, &f.schema);
    assert_eq!(set(&cold.derived), HashSet::from([f.want]));
    let update = r::delta::update(
        &r::delta::MemoryBase::new(&asserted, &[]),
        &[f.added],
        &[],
        spec(&f),
        Some(&old),
    );
    let now = update
        .program
        .as_ref()
        .expect("schema insertion publishes grounded cache");
    assert_eq!(
        now.rules.len(),
        old.rules.len(),
        "activation must not append a semantic duplicate"
    );
    assert_eq!(residual(now, &f), index);
    assert_eq!(r::eval::rule_key(&now.rules[index]), old_key);
    assert_eq!(now.rules[index].name, "eq-rep-p");
    assert!(update.remove.is_empty());
    println!(
        "FC1 old_data=1 inserted_schema=1 cold={} cached={} rules_before={} rules_after={} index={index} rounds={}",
        cold.derived.len(),
        update.insert.len(),
        old.rules.len(),
        now.rules.len(),
        update.rounds
    );
    assert_eq!(
        set(&update.insert),
        set(&cold.derived),
        "FC1-F1: newly generic retained residual must evaluate old data"
    );
    assert!(!update.insert.iter().any(|t| t[1] == f.never));
    let facts = r::delta::MemoryBase::new(&asserted, &update.insert);
    let proof = r::explain::explain_with(&facts, now, f.want, 64).expect("original named proof");
    assert_eq!(proof.steps[0].rule.as_deref(), Some("eq-rep-p"));
    assert!(
        now.premise_alternatives(r::eval::Grounding::Rule(index))
            .iter()
            .any(|p| p.contains(&f.added))
    );
    // A schema plus data insertion exercises suppression of variants covered by full evaluation.
    let more = [f.data[0] + 1, f.data[1], f.data[2]];
    let combined = [f.data, f.added, more];
    let cold = r::batch::materialise(&combined, &f.rules, None, &f.schema);
    let update = r::delta::update(
        &r::delta::MemoryBase::new(&combined, &[]),
        &[f.added, more],
        &[],
        spec(&f),
        Some(&old),
    );
    assert_eq!(set(&update.insert), set(&cold.derived));
    assert_eq!(
        update.program.as_ref().unwrap().rules.len(),
        old.rules.len()
    );
}

struct Split {
    old: Vec<Triple>,
    delta: Vec<Triple>,
    data_predicate: u64,
    data_reads: AtomicUsize,
}
impl Split {
    fn matches(pattern: [Option<u64>; 3], fact: Triple) -> bool {
        pattern
            .into_iter()
            .zip(fact)
            .all(|(p, t)| p.is_none_or(|p| p == t))
    }
    fn facts(&self, seg: Seg) -> impl Iterator<Item = Triple> + '_ {
        let old = if !matches!(seg.plain(), Seg::Delta) {
            self.old.as_slice()
        } else {
            &[]
        };
        let delta = if !matches!(seg.plain(), Seg::Old) {
            self.delta.as_slice()
        } else {
            &[]
        };
        old.iter().chain(delta).copied()
    }
}
impl Source for Split {
    fn scan(&self, p: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        for t in self.facts(seg).filter(|&t| Self::matches(p, t)) {
            if t[1] == self.data_predicate {
                self.data_reads.fetch_add(1, Relaxed);
            }
            f(t);
        }
    }
    fn estimate(&self, p: [Option<u64>; 3], seg: Seg) -> usize {
        self.facts(seg).filter(|&t| Self::matches(p, t)).count()
    }
    fn contains(&self, t: Triple) -> bool {
        self.old.contains(&t) || self.delta.contains(&t)
    }
}
#[test]
fn fc1_registration_is_once_without_rule_or_driver_duplication() {
    let f = fixture(true);
    let old = cache(&f);
    let index = residual(&old, &f);
    let source = Split {
        old: vec![f.data],
        delta: vec![f.added],
        data_predicate: f.data[1],
        data_reads: AtomicUsize::new(0),
    };
    let mut now = old.clone();
    let (ready, checks) = now.ground_delta(&source, &f.schema, &f.rules);
    assert_eq!(
        ready,
        vec![index],
        "one transition event despite two grouped origins"
    );
    assert!(checks.is_empty());
    assert_eq!(now.rules.len(), old.rules.len());
    source.data_reads.store(0, Relaxed);
    let job = r::eval::Job::full(&source, &now.rules[ready[0]]).unwrap();
    let facts = r::eval::run_jobs(&source, &[job], &|_| true, r::eval::NEVER);
    assert_eq!(set(&facts), HashSet::from([f.want]));
    assert_eq!(source.data_reads.load(Relaxed), 1, "one body driver walk");
    let (again, checks) = now.ground_delta(&source, &f.schema, &f.rules);
    assert!(
        again.is_empty() && checks.is_empty(),
        "same schema delta must not register twice"
    );
    let (after_clone, _) = now.clone().ground_delta(&source, &f.schema, &f.rules);
    assert!(after_clone.is_empty());
    println!(
        "FC1_EVENT emitted=1 repeat=0 clone_repeat=0 appended=0 body_reads=1 event_capacity={} event_retained_bytes={} repeat_capacity={} program_bytes={}",
        ready.capacity(),
        ready.capacity() * size_of::<usize>(),
        again.capacity(),
        size_of::<r::eval::GroundProgram>()
    );
}

#[test]
fn fc1_ordinary_contract_and_interrupted_publication() {
    let f = fixture(false);
    let old = cache(&f);
    let asserted = [f.data, f.added];
    let cold = r::batch::materialise(&asserted, &f.rules, None, &f.schema);
    assert!(cold.derived.is_empty());
    let update = r::delta::update(
        &r::delta::MemoryBase::new(&asserted, &[]),
        &[f.added],
        &[],
        spec(&f),
        Some(&old),
    );
    assert!(
        update.insert.is_empty(),
        "ordinary named Facts contract is unchanged"
    );
    assert_eq!(
        update.program.as_ref().unwrap().rules.len(),
        old.rules.len()
    );
    let f = fixture(true);
    let old = cache(&f);
    let index = residual(&old, &f);
    let asserted = [f.data, f.added];
    let stop = || true;
    let result = r::delta::update_with(
        &r::delta::MemoryBase::new(&asserted, &[]),
        &[f.added],
        &[],
        spec(&f),
        Some(&old),
        &stop,
        r::delta::policy::Policy::default(),
    );
    assert!(result.is_err(), "interrupted update publishes no candidate");
    let source = Split {
        old: vec![f.data],
        delta: vec![f.added],
        data_predicate: f.data[1],
        data_reads: AtomicUsize::new(0),
    };
    let (ready, _) = old.clone().ground_delta(&source, &f.schema, &f.rules);
    assert_eq!(
        ready,
        vec![index],
        "caller cache still has its unconsumed transition"
    );
}
