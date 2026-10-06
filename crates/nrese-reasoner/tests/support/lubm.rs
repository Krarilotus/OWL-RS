//! The LUBM-shaped input of the batch executor's guards (`memory_guards`, `work_guards`).
// Each guard binary uses part of it.
#![allow(dead_code)]

use nrese_reasoner::ir::{OWL, RDF, RDFS, Vocabulary};
use nrese_reasoner::vocabulary::LocalVocabulary;

pub const EX: &str = "http://example.com/";

/// The guards' input size: about 60,000 facts, 37,000 derived.
pub const DEPARTMENTS: u64 = 200;

/// Facts per predicate as the store hands them over: each predicate once, its
/// `(object, subject)` pairs sorted and distinct.
pub type Grouped = Vec<(u64, Vec<(u64, u64)>)>;

/// A LUBM-shaped input: a class hierarchy, sub-, inverse and transitive properties,
/// domains and ranges, and `departments` departments of people taking and teaching
/// courses.
pub fn lubm_like(vocabulary: &mut LocalVocabulary, departments: u64) -> Vec<[u64; 3]> {
    let mut iri = |name: &str| match name.split_once(':') {
        Some(("rdf", local)) => vocabulary.iri(&format!("{RDF}{local}")),
        Some(("rdfs", local)) => vocabulary.iri(&format!("{RDFS}{local}")),
        Some(("owl", local)) => vocabulary.iri(&format!("{OWL}{local}")),
        _ => vocabulary.iri(&format!("{EX}{name}")),
    };
    let mut facts = Vec::new();
    let schema = [
        "Employee rdfs:subClassOf Person",
        "Faculty rdfs:subClassOf Employee",
        "Professor rdfs:subClassOf Faculty",
        "FullProfessor rdfs:subClassOf Professor",
        "AssociateProfessor rdfs:subClassOf Professor",
        "Student rdfs:subClassOf Person",
        "GraduateStudent rdfs:subClassOf Student",
        "UndergraduateStudent rdfs:subClassOf Student",
        "Department rdfs:subClassOf Organization",
        "University rdfs:subClassOf Organization",
        "GraduateCourse rdfs:subClassOf Course",
        "worksFor rdfs:subPropertyOf memberOf",
        "headOf rdfs:subPropertyOf worksFor",
        "member owl:inverseOf memberOf",
        "subOrganizationOf rdf:type owl:TransitiveProperty",
        "takesCourse rdfs:domain Student",
        "teacherOf rdfs:domain Faculty",
        "teacherOf rdfs:range Course",
        "advisor rdfs:range Professor",
        "degreeFrom rdfs:range University",
    ];
    for line in schema {
        let [s, p, o]: [&str; 3] = line
            .split(' ')
            .collect::<Vec<_>>()
            .try_into()
            .expect("three terms");
        facts.push([iri(s), iri(p), iri(o)]);
    }
    let (ty, university) = (iri("rdf:type"), iri("University0"));
    facts.push([university, ty, iri("University")]);
    let names = [
        "Department",
        "FullProfessor",
        "AssociateProfessor",
        "GraduateStudent",
        "UndergraduateStudent",
        "GraduateCourse",
        "subOrganizationOf",
        "worksFor",
        "headOf",
        "teacherOf",
        "takesCourse",
        "advisor",
        "memberOf",
        "degreeFrom",
    ];
    let ids: Vec<u64> = names.iter().map(|name| iri(name)).collect();
    let [
        dept,
        full,
        assoc,
        grad,
        under,
        course,
        sub,
        works,
        head,
        teacher,
        takes,
        advisor,
        member,
        degree,
    ]: [u64; 14] = ids.try_into().expect("fourteen names");
    for d in 0..departments {
        let department = iri(&format!("Department{d}"));
        facts.push([department, ty, dept]);
        facts.push([department, sub, university]);
        let faculty: Vec<u64> = (0..8).map(|f| iri(&format!("Faculty{d}.{f}"))).collect();
        for (f, &person) in faculty.iter().enumerate() {
            facts.push([person, ty, if f % 2 == 0 { full } else { assoc }]);
            facts.push([person, if f == 0 { head } else { works }, department]);
            facts.push([person, degree, university]);
            for c in 0..2 {
                let taught = iri(&format!("Course{d}.{f}.{c}"));
                facts.push([taught, ty, course]);
                facts.push([person, teacher, taught]);
            }
        }
        for s in 0..40u64 {
            let student = iri(&format!("Student{d}.{s}"));
            facts.push([student, ty, if s % 4 == 0 { grad } else { under }]);
            facts.push([student, member, department]);
            facts.push([student, advisor, faculty[(s % 8) as usize]]);
            for c in 0..3 {
                let f = (s + c) % 8;
                let taken = iri(&format!("Course{d}.{f}.{}", c % 2));
                facts.push([student, takes, taken]);
            }
        }
    }
    facts
}

/// The declarations univ-bench makes and [`lubm_like`] leaves out: each class an
/// `owl:Class`, each property an `owl:ObjectProperty`. Under OWL 2 RL they give the
/// reflexive schema facts (`scm-cls`, `scm-op`: `C subClassOf C`, `p subPropertyOf p`).
pub fn declarations(vocabulary: &mut LocalVocabulary) -> Vec<[u64; 3]> {
    let ty = vocabulary.iri(&format!("{RDF}type"));
    let class = vocabulary.iri(&format!("{OWL}Class"));
    let property = vocabulary.iri(&format!("{OWL}ObjectProperty"));
    let classes = [
        "Person",
        "Employee",
        "Faculty",
        "Professor",
        "FullProfessor",
        "AssociateProfessor",
        "Student",
        "GraduateStudent",
        "UndergraduateStudent",
        "Organization",
        "Department",
        "University",
        "Course",
        "GraduateCourse",
    ];
    let properties = [
        "worksFor",
        "memberOf",
        "headOf",
        "member",
        "subOrganizationOf",
        "takesCourse",
        "teacherOf",
        "advisor",
        "degreeFrom",
    ];
    let mut out = Vec::new();
    for name in classes {
        out.push([vocabulary.iri(&format!("{EX}{name}")), ty, class]);
    }
    for name in properties {
        out.push([vocabulary.iri(&format!("{EX}{name}")), ty, property]);
    }
    out
}

/// `facts` as the store hands them over ([`Grouped`]).
pub fn grouped(mut facts: Vec<[u64; 3]>) -> Grouped {
    facts.sort_unstable_by_key(|&[s, p, o]| (p, o, s));
    facts.dedup();
    let mut groups: Grouped = Vec::new();
    for [s, p, o] in facts {
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => pairs.push((o, s)),
            _ => groups.push((p, vec![(o, s)])),
        }
    }
    groups
}
