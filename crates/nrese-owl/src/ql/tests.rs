//! Tree witnesses and rewritings of small queries, checked by hand.

use std::collections::HashMap;

use super::*;
use crate::mapping::{TermKind, Terms};
use crate::model::Term;
use crate::ofn::{Intern, read_functional};

/// Term ids for IRIs written `:name` (in `http://e/`).
#[derive(Default)]
struct Table {
    ids: HashMap<String, Term>,
    names: Vec<String>,
}

impl Table {
    fn id(&mut self, key: String) -> Term {
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.names.len() as Term;
        self.names.push(key.clone());
        self.ids.insert(key, id);
        id
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match self.names[term as usize].as_bytes()[0] {
            b'_' => TermKind::Blank,
            b'"' => TermKind::Literal,
            _ => TermKind::Iri,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        Some(self.names[term as usize].trim_matches('"').to_owned())
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids.get(iri).copied()
    }
}

impl Intern for Table {
    fn iri_id(&mut self, iri: &str) -> Term {
        self.id(iri.to_owned())
    }

    fn literal_id(&mut self, lexical: &str, _: &str, _: Option<&str>) -> Term {
        self.id(format!("\"{lexical}\""))
    }

    fn blank_id(&mut self, label: &str, _: u32) -> Term {
        self.id(format!("_:{label}"))
    }
}

/// The compiled TBox of the axioms (functional syntax, prefix `:`).
fn tbox(axioms: &str, lists: bool) -> (Tbox, Table) {
    let mut table = Table::default();
    let text = format!("Prefix(:=<http://e/>)\nOntology(<http://e/o>\n{axioms}\n)");
    let (ontology, _) = read_functional(&text, &mut table);
    let thing = table.iri("http://www.w3.org/2002/07/owl#Thing");
    (Tbox::compile(&ontology, Closure { lists }, thing), table)
}

/// A query: atoms `C(t)` and `p(s, o)` over variables `?a`… (existential if listed in
/// `existential`) and constants `:c`.
fn cq(table: &mut Table, atoms: &str, existential: &[&str]) -> (Cq, Vec<String>) {
    let mut vars: Vec<String> = Vec::new();
    let mut term = |table: &mut Table, text: &str| -> QTerm {
        if let Some(name) = text.strip_prefix('?') {
            let at = vars.iter().position(|v| v == name).unwrap_or_else(|| {
                vars.push(name.to_owned());
                vars.len() - 1
            });
            QTerm::Var(at as u32)
        } else {
            QTerm::Const(table.id(format!("http://e/{}", text.trim_start_matches(':'))))
        }
    };
    let mut parsed = Vec::new();
    for atom in atoms.split(',').map(str::trim) {
        let (name, args) = atom.trim_end_matches(')').split_once('(').unwrap();
        let args: Vec<&str> = args.split(' ').collect();
        let predicate = table.id(format!("http://e/{name}"));
        parsed.push(match args[..] {
            [t] => Atom::Class(term(table, t), predicate),
            [s, o] => Atom::Role(term(table, s), predicate, term(table, o)),
            _ => panic!("{atom}"),
        });
    }
    let existential = vars
        .iter()
        .map(|v| existential.contains(&v.as_str()))
        .collect();
    (
        Cq {
            atoms: parsed,
            vars: vars.len() as u32,
            existential,
        },
        vars,
    )
}

/// The branches written back as text, sorted: `A(?x) R(?x ?_1) {B(?x) | ∃R(?x)}`.
fn branches(rewriting: &Rewriting, table: &Table, vars: &[String]) -> Vec<String> {
    let name = |t: Term| {
        table.names[t as usize]
            .trim_start_matches("http://e/")
            .to_owned()
    };
    let term = |t: &QTerm| match t {
        QTerm::Var(v) if (*v as usize) < vars.len() => format!("?{}", vars[*v as usize]),
        QTerm::Var(_) => "_".to_owned(),
        QTerm::Const(c) => format!(":{}", name(*c)),
    };
    let atom = |a: &Atom| match a {
        Atom::Class(t, c) => format!("{}({})", name(*c), term(t)),
        Atom::Role(s, p, o) => format!("{}({} {})", name(*p), term(s), term(o)),
        Atom::Other(..) => "other".to_owned(),
    };
    let mut out: Vec<String> = rewriting
        .branches
        .iter()
        .map(|b| {
            let mut parts: Vec<String> = b
                .parts
                .iter()
                .map(|p| match p {
                    Part::Atom(a) => atom(a),
                    Part::Any(alternatives) => {
                        let mut alternatives: Vec<String> = alternatives.iter().map(atom).collect();
                        alternatives.sort();
                        format!("{{{}}}", alternatives.join(" | "))
                    }
                })
                .collect();
            parts.sort();
            for (v, t) in &b.merged {
                parts.push(format!("?{}={}", vars[*v as usize], term(t)));
            }
            parts.join(" ")
        })
        .collect();
    out.sort();
    out
}

fn rewritten(axioms: &str, atoms: &str, existential: &[&str]) -> Vec<String> {
    let (tbox, mut table) = tbox(axioms, true);
    let (cq, vars) = cq(&mut table, atoms, existential);
    match rewrite(&tbox, &cq, &Limits::default()) {
        Outcome::Rewritten(r) => branches(&r, &table, &vars),
        Outcome::Unchanged => vec!["unchanged".to_owned()],
        Outcome::Exceeded(what) => vec![format!("exceeded {what}")],
    }
}

#[test]
fn an_unprojected_object_is_a_tree_witness() {
    let axioms = "SubClassOf(:Employee ObjectSomeValuesFrom(:worksFor owl:Thing))";
    assert_eq!(
        rewritten(axioms, "worksFor(?x ?y)", &["y"]),
        ["worksFor(?x ?y)", "{Employee(?x)}"]
    );
    // A projected one isn't.
    assert_eq!(rewritten(axioms, "worksFor(?x ?y)", &[]), ["unchanged"]);
}

#[test]
fn ontops_example_gives_its_four_queries() {
    // Rodríguez-Muro, Kontchakov and Zakharyaschev, ISWC 2013, §2.1.
    let axioms = "SubClassOf(:RA ObjectSomeValuesFrom(:worksOn :Project))
        SubClassOf(:Project ObjectSomeValuesFrom(:isManagedBy :Prof))
        SubObjectPropertyOf(ObjectInverseOf(:worksOn) :involves)
        SubObjectPropertyOf(:isManagedBy :involves)";
    assert_eq!(
        rewritten(
            axioms,
            "worksOn(?x ?y), involves(?y ?z), Prof(?z)",
            &["y", "z"]
        ),
        [
            "Prof(?x) {RA(?x)} ?z=?x",
            "Prof(?z) involves(?y ?z) worksOn(?x ?y)",
            "worksOn(?x ?y) {Project(?y)}",
            "{RA(?x)}",
        ]
    );
}

#[test]
fn trees_are_followed_below_the_first_anonymous_element() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R :B))
        SubClassOf(:B ObjectSomeValuesFrom(:S :C))";
    let out = rewritten(axioms, "R(?x ?y), S(?y ?z), C(?z)", &["y", "z"]);
    assert!(out.contains(&"{A(?x)}".to_owned()), "{out:?}");
    assert!(out.contains(&"R(?x ?y) {B(?y)}".to_owned()), "{out:?}");
    // Without roots: any tree that reaches an `S`-successor in `C`, or a named `B`.
    let out = rewritten(axioms, "S(?y ?z), C(?z)", &["y", "z"]);
    assert!(out.contains(&"{A(_)}".to_owned()), "{out:?}");
    assert!(out.contains(&"{B(?y)}".to_owned()), "{out:?}");
}

#[test]
fn inverses_and_role_hierarchies_reach_the_anonymous_part() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R owl:Thing))
        InverseObjectProperties(:R :S)
        SubObjectPropertyOf(:S :T)";
    assert_eq!(
        rewritten(axioms, "T(?y ?x)", &["y"]),
        ["T(?y ?x)", "{A(?x)}"]
    );
    // The wrong direction has no witness.
    assert_eq!(rewritten(axioms, "T(?x ?y)", &["y"]), ["unchanged"]);
}

#[test]
fn roots_of_one_witness_are_one_individual() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R owl:Thing))";
    assert_eq!(
        rewritten(axioms, "R(?x ?y), R(?w ?y)", &["y"]),
        ["R(?w ?y) R(?x ?y)", "{A(?x)} ?w=?x"]
    );
    // Two constants can't be one individual.
    assert_eq!(
        rewritten(axioms, "R(:a ?y), R(:b ?y)", &["y"]),
        ["unchanged"]
    );
}

#[test]
fn memberships_only_an_existential_gives_are_alternatives() {
    let axioms = "SubClassOf(:Employee ObjectSomeValuesFrom(:worksFor :Organisation))
        ObjectPropertyDomain(:worksFor :Person)
        SubClassOf(:Manager :Employee)";
    // `Manager` is an `Employee` in the closure already.
    assert_eq!(
        rewritten(axioms, "Person(?x)", &[]),
        ["{Employee(?x) | Person(?x)}"]
    );
    // Applied by the materialisation: nothing to add.
    assert_eq!(rewritten(axioms, "Employee(?x)", &[]), ["unchanged"]);
    // The filler's class holds of the anonymous employer.
    assert_eq!(
        rewritten(axioms, "worksFor(?x ?y), Organisation(?y)", &["y"]),
        ["Organisation(?y) worksFor(?x ?y)", "{Employee(?x)}"]
    );
}

#[test]
fn intersections_on_the_right_are_alternatives_only_without_the_list_rules() {
    let axioms = "SubClassOf(:A ObjectIntersectionOf(:B :C))";
    let (rl, mut table) = tbox(axioms, true);
    let (query, _) = cq(&mut table, "B(?x)", &[]);
    assert_eq!(rewrite(&rl, &query, &Limits::default()), Outcome::Unchanged);
    let (ql, mut table) = tbox(axioms, false);
    let (query, vars) = cq(&mut table, "B(?x)", &[]);
    let Outcome::Rewritten(r) = rewrite(&ql, &query, &Limits::default()) else {
        panic!("not rewritten");
    };
    assert_eq!(branches(&r, &table, &vars), ["{A(?x) | B(?x)}"]);
}

#[test]
fn nested_fillers_and_data_values_generate() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R ObjectIntersectionOf(:B ObjectSomeValuesFrom(:S :C))))
        Declaration(DataProperty(:age))
        SubClassOf(:P DataSomeValuesFrom(:age xsd:integer))";
    let out = rewritten(axioms, "R(?x ?y), B(?y), S(?y ?z), C(?z)", &["y", "z"]);
    assert!(out.contains(&"{A(?x)}".to_owned()), "{out:?}");
    assert_eq!(
        rewritten(axioms, "age(?x ?v)", &["v"]),
        ["age(?x ?v)", "{P(?x)}"]
    );
    // A data value has no class and no successor.
    assert_eq!(
        rewritten(axioms, "age(?x ?v), B(?v)", &["v"]),
        ["unchanged"]
    );
}

#[test]
fn reflexive_properties_hold_of_anonymous_elements() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R :B))
        ReflexiveObjectProperty(:knows)";
    let out = rewritten(axioms, "R(?x ?y), knows(?y ?y), B(?y)", &["y"]);
    assert!(out.contains(&"{A(?x)}".to_owned()), "{out:?}");
}

#[test]
fn without_generating_axioms_nothing_changes() {
    let axioms = "SubClassOf(:A :B)
        ObjectPropertyDomain(:R :A)
        SubObjectPropertyOf(:R :S)";
    let (tbox, mut table) = tbox(axioms, false);
    assert!(tbox.is_empty());
    let (query, _) = cq(&mut table, "S(?x ?y), B(?x)", &["y"]);
    assert_eq!(
        rewrite(&tbox, &query, &Limits::default()),
        Outcome::Unchanged
    );
}

#[test]
fn rewritings_stop_at_their_bounds() {
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:R :A))";
    let chain: Vec<String> = (0..20).map(|i| format!("R(?v{i} ?v{})", i + 1)).collect();
    let existential: Vec<String> = (1..=20).map(|i| format!("v{i}")).collect();
    let existential: Vec<&str> = existential.iter().map(String::as_str).collect();
    assert_eq!(
        rewritten(axioms, &chain.join(", "), &existential),
        ["exceeded existential variables"]
    );
    // A chain of eight below an individual: witnesses for every suffix.
    let (tbox, mut table) = tbox(axioms, true);
    let chain: Vec<String> = (0..8).map(|i| format!("R(?v{i} ?v{})", i + 1)).collect();
    let existential: Vec<String> = (1..=8).map(|i| format!("v{i}")).collect();
    let existential: Vec<&str> = existential.iter().map(String::as_str).collect();
    let (query, _) = cq(&mut table, &chain.join(", "), &existential);
    let Outcome::Rewritten(r) = rewrite(&tbox, &query, &Limits::default()) else {
        panic!("not rewritten");
    };
    assert_eq!(r.witnesses, 8);
    assert_eq!(r.branches.len(), 9);
    let tight = Limits {
        branches: 4,
        ..Limits::default()
    };
    assert_eq!(
        rewrite(&tbox, &query, &tight),
        Outcome::Exceeded("branches")
    );
}

#[test]
fn witnesses_the_query_implies_are_dropped() {
    let axioms = "SubClassOf(:Wellbore ObjectSomeValuesFrom(:drilledBy owl:Thing))
        SubClassOf(:Wellbore ObjectSomeValuesFrom(:permit owl:Thing))
        SubClassOf(:Exploration :Wellbore)";
    // The root is stated to be a class that generates both arms: the class atom alone.
    assert_eq!(
        rewritten(
            axioms,
            "Exploration(?w), drilledBy(?w ?a), permit(?w ?b)",
            &["a", "b"]
        ),
        ["Exploration(?w)"]
    );
    // Not stated: four branches, the arms folded one by one.
    assert_eq!(
        rewritten(axioms, "drilledBy(?w ?a), permit(?w ?b)", &["a", "b"]).len(),
        4
    );
    // A projected arm stays.
    assert_eq!(
        rewritten(
            axioms,
            "Wellbore(?w), drilledBy(?w ?a), permit(?w ?b)",
            &["b"]
        ),
        ["Wellbore(?w) drilledBy(?w ?a)"]
    );
}

/// The hazards a query's atoms reach, as `term: hazard` with local names.
fn concerns(axioms: &str, atoms: &str) -> Vec<String> {
    let (tbox, mut table) = tbox(axioms, true);
    let (query, _) = cq(&mut table, atoms, &[]);
    let name = |t: Term| {
        table.names[t as usize]
            .trim_start_matches("http://e/")
            .to_owned()
    };
    tbox.concerns(&query)
        .into_iter()
        .map(|(t, h)| format!("{}: {}", name(t), h.describe(&name)))
        .collect()
}

#[test]
fn axioms_beyond_ql_that_meet_anonymous_individuals_are_hazards() {
    let ql = "SubClassOf(:A ObjectSomeValuesFrom(:R :B))
        SubObjectPropertyOf(:R :S)";
    // Pure QL: nothing.
    assert!(concerns(ql, "S(?x ?y), B(?y), A(?x)").is_empty());
    // A transitive super-role: its atoms, the role's and the filler's.
    let transitive = format!("{ql}\nTransitiveObjectProperty(:S)");
    assert_eq!(
        concerns(&transitive, "S(?x ?y), B(?y), C(?x)"),
        ["S: S is transitive", "B: S is transitive"]
    );
    // A transitive property no existential reaches: nothing.
    let apart = format!("{ql}\nTransitiveObjectProperty(:T)");
    assert!(concerns(&apart, "S(?x ?y), T(?x ?y)").is_empty());
    // A chain through the role, and what the chain's result implies.
    let chain = format!(
        "{ql}\nSubObjectPropertyOf(ObjectPropertyChain(:R :Q) :P)\nObjectPropertyDomain(:P :D)"
    );
    let found = concerns(&chain, "P(?x ?y), D(?x), A(?x)");
    assert!(found.iter().any(|c| c.starts_with("P: ")), "{found:?}");
    assert!(found.iter().any(|c| c.starts_with("D: ")), "{found:?}");
    // A functional role can make an anonymous individual a named one: every term.
    let functional = format!("{ql}\nFunctionalObjectProperty(:R)");
    assert_eq!(
        concerns(&functional, "E(?x)"),
        ["E: R is functional or inverse functional"]
    );
    // An RL axiom on the left classifies anonymous individuals: its conclusion.
    let left = format!("{ql}\nSubClassOf(ObjectSomeValuesFrom(:R :B) :D)");
    assert_eq!(
        concerns(&left, "D(?x)"),
        ["D: a qualified someValuesFrom on R on the left of an axiom"]
    );
}

#[test]
fn an_equating_hazard_is_not_hidden_by_another_on_the_same_role() {
    // Found by the mixed differential test (case 2378 of seed 11): a transitive property
    // whose inverse is inverse functional makes two named individuals equal through an
    // anonymous one; transitivity came first and hid it.
    let axioms = "SubClassOf(:A ObjectSomeValuesFrom(:P :A))
        InverseObjectProperties(:P :Q)
        TransitiveObjectProperty(:Q)
        InverseFunctionalObjectProperty(:P)";
    assert_eq!(
        concerns(axioms, "R(?x :a)"),
        ["R: P is functional or inverse functional"]
    );
}

/// A TBox where every anonymous element has ten successor types, and a chain of twelve
/// existential arms ending in a class none of them has: the witness search would try
/// about 10^11 places before failing.
fn dense() -> (String, String) {
    let axioms: Vec<String> = (0..10)
        .flat_map(|i| {
            (0..10).map(move |j| format!("SubClassOf(:C{i} ObjectSomeValuesFrom(:P :C{j}))"))
        })
        .collect();
    let mut chain: Vec<String> = vec!["P(?x ?v1)".to_owned()];
    chain.extend((1..12).map(|i| format!("P(?v{i} ?v{})", i + 1)));
    chain.push("D(?v12)".to_owned());
    (axioms.join("\n"), chain.join(", "))
}

#[test]
fn the_work_bound_stops_a_search_the_size_bounds_dont() {
    let (axioms, atoms) = dense();
    let existential: Vec<String> = (1..=12).map(|i| format!("v{i}")).collect();
    let existential: Vec<&str> = existential.iter().map(String::as_str).collect();
    // Within every size bound (12 existential variables, few witnesses), but not the work.
    assert_eq!(rewritten(&axioms, &atoms, &existential), ["exceeded work"]);
}
