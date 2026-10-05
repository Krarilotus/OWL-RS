//! The functional-syntax reader on the W3C test cases:
//! - every DL ontology read from RDF/XML, written as a functional-syntax document and read
//!   back, is the same model ([`round_trip`], from `check`);
//! - where a test case gives an ontology in both syntaxes, both readings are the same
//!   ontology: equal axioms, equivalences as the same partition and n-ary disjointness as
//!   the same pairs (the RDF mapping writes those in more than one form), anonymous
//!   individuals as any anonymous individual. Differences are listed in
//!   `functional-differences.txt` with what causes them; the run fails on any other.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nrese_owl::{
    Axiom, Diagnostic, Intern, Ontology, Statement, Term, iri_text, literal_text, read,
    read_functional,
};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term as RdfTerm};

use super::{TEST, Table, parse_rdf_xml, suite_path};

const DIFFERENCES: &str = include_str!("functional-differences.txt");

impl Intern for Table {
    fn iri_id(&mut self, iri: &str) -> Term {
        self.id(NamedNode::new_unchecked(iri).into())
    }

    fn literal_id(&mut self, lexical: &str, datatype: &str, language: Option<&str>) -> Term {
        let literal = match language {
            Some(tag) => Literal::new_language_tagged_literal(lexical, tag).unwrap_or_else(|_| {
                Literal::new_language_tagged_literal_unchecked(lexical, tag.to_lowercase())
            }),
            None => Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)),
        };
        self.id(literal.into())
    }

    fn blank_id(&mut self, label: &str, document: u32) -> Term {
        self.id(BlankNode::new_unchecked(format!("fs{document}x{label}")).into())
    }
}

/// A term as the functional syntax writes it.
fn ofn_name(table: &Table, t: Term) -> String {
    match &table.terms[t as usize] {
        RdfTerm::NamedNode(n) => iri_text(n.as_str()),
        RdfTerm::BlankNode(b) => format!("_:{}", b.as_str()),
        RdfTerm::Literal(l) => literal_text(l.value(), Some(l.datatype().as_str()), l.language()),
        other => panic!("not an OWL term: {other}"),
    }
}

/// A blank node label without the `fs<document>x` the table puts before the reader's.
fn without_document(label: &str) -> &str {
    label
        .strip_prefix("fs")
        .and_then(|rest| {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0)
                .then(|| rest[digits..].strip_prefix('x'))
                .flatten()
        })
        .unwrap_or(label)
}

/// `o` (read from RDF/XML) as a document and read back: why not the same model, if not.
pub fn round_trip(o: &Ontology, table: &mut Table) -> Result<(), String> {
    let text = o.functional_document(Some("http://example.org/round-trip"), &|t| {
        ofn_name(table, t)
    });
    let (back, _) = read_functional(&text, table);
    if let Some(d) = back
        .diagnostics
        .iter()
        .find(|d| matches!(d, Diagnostic::Syntax { .. }))
    {
        return Err(format!("the functional syntax written doesn't read: {d:?}"));
    }
    // Anonymous individuals are the document's own: compared by their labels.
    let name = |t: Term| match &table.terms[t as usize] {
        RdfTerm::BlankNode(b) => format!("_:{}", without_document(b.as_str())),
        _ => table.name(t),
    };
    let lines = |o: &Ontology| {
        let mut v: Vec<String> = o.axioms.iter().map(|a| o.functional(a, &name)).collect();
        v.sort();
        v
    };
    let (before, after) = (lines(o), lines(&back));
    if before != after {
        let missing: Vec<&String> = before.iter().filter(|l| !after.contains(l)).collect();
        let extra: Vec<&String> = after.iter().filter(|l| !before.contains(l)).collect();
        return Err(format!(
            "the functional round trip differs: missing {missing:?}, extra {extra:?}"
        ));
    }
    Ok(())
}

/// The ontology's meaning in lines: axioms, but each kind of equivalence as its partition
/// and each n-ary disjointness as its pairs; anonymous individuals as `_:`.
fn meaning(o: &Ontology, table: &Table) -> BTreeSet<String> {
    let name = |t: Term| match &table.terms[t as usize] {
        RdfTerm::BlankNode(_) => "_:".to_owned(),
        _ => table.name(t),
    };
    let mut out = BTreeSet::new();
    // Equivalences: union of the members' texts, per kind.
    let mut groups: BTreeMap<&str, Vec<BTreeSet<String>>> = BTreeMap::new();
    let mut merge = |kind: &'static str, members: Vec<String>| {
        let list = groups.entry(kind).or_default();
        let mut joined: BTreeSet<String> = members.into_iter().collect();
        list.retain(|g| {
            if g.iter().any(|m| joined.contains(m)) {
                joined.extend(g.iter().cloned());
                false
            } else {
                true
            }
        });
        list.push(joined);
    };
    // A member's text: the operand written alone (an n-ary axiom of one operand).
    let one_class = |x| o.functional(&Axiom::EquivalentClasses(vec![x]), &name);
    let one_property = |p| o.functional(&Axiom::EquivalentObjectProperties(vec![p]), &name);
    for a in &o.axioms {
        match a {
            Axiom::EquivalentClasses(xs) => {
                merge("classes", xs.iter().map(|&x| one_class(x)).collect())
            }
            Axiom::EquivalentObjectProperties(ps) => {
                merge("objects", ps.iter().map(|&p| one_property(p)).collect())
            }
            Axiom::EquivalentDataProperties(ps) => {
                merge("data", ps.iter().map(|&p| name(p)).collect())
            }
            Axiom::SameIndividual(xs) => merge("same", xs.iter().map(|&x| name(x)).collect()),
            Axiom::DisjointClasses(xs) if xs.len() > 2 => {
                for (i, &x) in xs.iter().enumerate() {
                    for &y in &xs[i + 1..] {
                        let pair = Axiom::DisjointClasses(vec![x, y]);
                        out.insert(o.functional(&pair, &name));
                    }
                }
            }
            Axiom::DifferentIndividuals(xs) if xs.len() > 2 => {
                for (i, &x) in xs.iter().enumerate() {
                    for &y in &xs[i + 1..] {
                        let pair = Axiom::DifferentIndividuals(vec![x.min(y), x.max(y)]);
                        out.insert(o.functional(&pair, &name));
                    }
                }
            }
            other => {
                out.insert(o.functional(other, &name));
            }
        }
    }
    for (kind, list) in groups {
        for g in list {
            out.insert(format!(
                "{kind} ≡ {}",
                g.into_iter().collect::<Vec<_>>().join(" ≡ ")
            ));
        }
    }
    out
}

/// Both readings of every ontology a test case gives in RDF/XML and in the functional
/// syntax.
#[test]
fn functional_and_rdf_readings_agree() {
    let Ok(text) = std::fs::read_to_string(suite_path()) else {
        return;
    };
    let triples = parse_rdf_xml(&text).expect("the test case collection parses");
    let mut names: HashMap<NamedOrBlankNode, String> = HashMap::new();
    let mut dl: HashMap<NamedOrBlankNode, bool> = HashMap::new();
    // Per case and role: (RDF/XML, functional syntax).
    let mut documents: HashMap<(NamedOrBlankNode, String), (Option<String>, Option<String>)> =
        HashMap::new();
    for t in &triples {
        let Some(local) = t.predicate.as_str().strip_prefix(TEST) else {
            continue;
        };
        let value = match &t.object {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        };
        match local {
            "identifier" => {
                if let Some(v) = value {
                    names.insert(t.subject.clone(), v);
                }
            }
            "species" => {
                if matches!(&t.object, RdfTerm::NamedNode(n) if n.as_str() == format!("{TEST}DL")) {
                    dl.insert(t.subject.clone(), true);
                }
            }
            _ => {
                let (rdf, role) = if let Some(r) = local.strip_prefix("rdfXml") {
                    (true, r)
                } else if let Some(r) = local.strip_prefix("fs") {
                    (false, r)
                } else {
                    continue;
                };
                let (Some(role), Some(v)) = (role.strip_suffix("Ontology"), value) else {
                    continue;
                };
                let slot = documents
                    .entry((t.subject.clone(), role.to_owned()))
                    .or_default();
                if rdf {
                    slot.0 = Some(v);
                } else {
                    slot.1 = Some(v);
                }
            }
        }
    }
    let listed: BTreeMap<&str, &str> = DIFFERENCES
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split_once('\t')
                .map_or((l.trim(), ""), |(n, w)| (n.trim(), w))
        })
        .collect();
    let (mut compared, mut unlisted, mut agreeing_listed) = (0, Vec::new(), Vec::new());
    let mut lines = 0;
    let mut keys: Vec<_> = documents.keys().cloned().collect();
    keys.sort_by(|a, b| (names.get(&a.0), &a.1).cmp(&(names.get(&b.0), &b.1)));
    for key in keys {
        let (node, role) = &key;
        let (Some(rdf), Some(fs)) = &documents[&key] else {
            continue;
        };
        if !dl.get(node).copied().unwrap_or(false) {
            continue;
        }
        let Some(name) = names.get(node) else {
            continue;
        };
        let id = format!("{name} ({role})");
        compared += 1;
        let mut table = Table::default();
        let Ok(rdf_triples) = parse_rdf_xml(rdf) else {
            continue;
        };
        let statements: Vec<Statement> = rdf_triples
            .into_iter()
            .map(|t| Statement {
                triple: [
                    table.id(t.subject.into()),
                    table.id(t.predicate.into()),
                    table.id(t.object),
                ],
                graph: 0,
            })
            .collect();
        let from_rdf = read(&statements, &table);
        let (from_fs, _) = read_functional(fs, &mut table);
        let syntax: Vec<&Diagnostic> = from_fs
            .diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::Syntax { .. }))
            .collect();
        let (a, b) = (meaning(&from_rdf, &table), meaning(&from_fs, &table));
        lines += a.len();
        let difference = if !syntax.is_empty() {
            Some(format!("functional syntax not read: {syntax:?}"))
        } else if a != b {
            let only_rdf: Vec<&String> = a.difference(&b).collect();
            let only_fs: Vec<&String> = b.difference(&a).collect();
            Some(format!(
                "only from RDF/XML {only_rdf:?}; only from the functional syntax {only_fs:?}"
            ))
        } else {
            None
        };
        match (difference, listed.contains_key(id.as_str())) {
            (Some(why), false) => unlisted.push(format!("{id}: {why}")),
            (None, true) => agreeing_listed.push(id),
            _ => {}
        }
    }
    eprintln!("{compared} ontologies in both syntaxes compared, {lines} axioms");
    assert!(
        compared > 70 && lines > 300,
        "only {compared}, {lines} axioms"
    );
    assert!(
        unlisted.is_empty() && agreeing_listed.is_empty(),
        "differences not listed:\n{}\nlisted but agreeing:\n{}",
        unlisted.join("\n"),
        agreeing_listed.join("\n")
    );
}
