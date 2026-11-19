//! OWL 2 DL's typing constraints (*Structural Specification* §5.8.1): an IRI used as an
//! object, a data or an annotation property is used as no other of the three, and a class
//! is no datatype (`nrese_owl::typing`). Every slot an axiom, a class expression, a data
//! range or a rule atom has gives its name a kind; so does a statement read past as an
//! annotation (*Mapping to RDF* Table 10). A name with two kinds is reported, with whether
//! more than an assertion names it.
//!
//! And the reader's account of a structure no axiom uses ([`Diagnostic::Unused`]): whether
//! the OWL 2 RL rules may read more of it than one structure says.

use nrese_owl::typing::{TwoKinds, two_kinds};
use nrese_owl::{Diagnostic, EntityKind, Ontology, Term, Terms, read, read_functional};

use super::functional::EVERYTHING;
use super::{Table, load};

const OBJECT_DATA: [EntityKind; 2] = [EntityKind::ObjectProperty, EntityKind::DataProperty];
const OBJECT_ANNOTATION: [EntityKind; 2] =
    [EntityKind::ObjectProperty, EntityKind::AnnotationProperty];
const CLASS_DATATYPE: [EntityKind; 2] = [EntityKind::Class, EntityKind::Datatype];

/// A name used as two kinds: its local name in `ex:`, the kinds, whether an axiom names it.
type Report = (String, [EntityKind; 2], bool);

/// `ontology`'s names used as two kinds.
fn reported(ontology: &Ontology, table: &Table) -> Vec<Report> {
    let local = |t: Term| {
        let iri = table.iri_text(t).unwrap_or_else(|| format!("#{t}"));
        iri.strip_prefix(super::EX)
            .map_or(iri.clone(), str::to_owned)
    };
    (two_kinds(ontology).into_iter())
        .map(|t: TwoKinds| (local(t.term), t.kinds, t.by_axiom))
        .collect()
}

/// The names used as two kinds in a functional-syntax document of `annotations`, `:x`
/// declared `kind`, and `axiom`.
fn functional(annotations: &str, kind: &str, axiom: &str) -> Vec<Report> {
    let text = format!(
        "Prefix(:=<http://example.com/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Ontology({annotations}\nDeclaration({kind}(:x))\n{axiom}\n)"
    );
    let mut table = Table::default();
    let (ontology, _) = read_functional(&text, &mut table);
    reported(&ontology, &table)
}

/// The names used as two kinds in Turtle statements.
fn turtle(statements: &str) -> Vec<Report> {
    let mut table = Table::default();
    let read_from = load(&mut table, statements);
    reported(&read(&read_from, &table), &table)
}

/// Spec (§5.8.1, with *Structural Specification* §§8-10 for where each kind stands): every
/// slot of every axiom, class expression, data range and rule atom that holds an object
/// property, a data property, a class or a datatype, and every place of an annotation, gives
/// its name that kind. `:x`, declared one kind, in a slot of the other kind of its pair, is
/// one name used as two kinds, named by an axiom (its declaration at least); no other name
/// is reported.
#[test]
fn every_slot_of_every_axiom_is_typed() {
    let object_slots = [
        "SubObjectPropertyOf(:x :q)",
        "SubObjectPropertyOf(:q :x)",
        "SubObjectPropertyOf(ObjectPropertyChain(:q :x) :q)",
        "EquivalentObjectProperties(:x :q)",
        "DisjointObjectProperties(:x :q)",
        "InverseObjectProperties(:x :q)",
        "ObjectPropertyDomain(:x :C)",
        "ObjectPropertyRange(ObjectInverseOf(:x) :C)",
        "FunctionalObjectProperty(:x)",
        "TransitiveObjectProperty(:x)",
        "HasKey(:C (:x) ())",
        "ObjectPropertyAssertion(:x :a :b)",
        "NegativeObjectPropertyAssertion(:x :a :b)",
        "SubClassOf(:C ObjectSomeValuesFrom(:x :C))",
        "SubClassOf(:C ObjectAllValuesFrom(:x :C))",
        "SubClassOf(:C ObjectMinCardinality(1 :x :C))",
        "SubClassOf(:C ObjectMaxCardinality(1 :x))",
        "SubClassOf(:C ObjectExactCardinality(1 :x :C))",
        "SubClassOf(:C ObjectHasValue(:x :a))",
        "SubClassOf(:C ObjectHasSelf(:x))",
        "ClassAssertion(ObjectSomeValuesFrom(:x :C) :a)",
        "DLSafeRule(Body(ObjectPropertyAtom(:x Variable(:v) Variable(:w))) Head())",
    ];
    let data_slots = [
        "SubDataPropertyOf(:x :d)",
        "SubDataPropertyOf(:d :x)",
        "EquivalentDataProperties(:x :d)",
        "DisjointDataProperties(:x :d)",
        "DataPropertyDomain(:x :C)",
        "DataPropertyRange(:x xsd:string)",
        "FunctionalDataProperty(:x)",
        "HasKey(:C () (:x))",
        "DataPropertyAssertion(:x :a \"v\")",
        "NegativeDataPropertyAssertion(:x :a \"v\")",
        "SubClassOf(:C DataSomeValuesFrom(:x xsd:string))",
        "SubClassOf(:C DataAllValuesFrom(:x xsd:string))",
        "SubClassOf(:C DataMinCardinality(1 :x))",
        "SubClassOf(:C DataMaxCardinality(1 :x xsd:string))",
        "SubClassOf(:C DataExactCardinality(1 :x))",
        "SubClassOf(:C DataHasValue(:x \"v\"))",
        "DLSafeRule(Body(DataPropertyAtom(:x Variable(:v) Variable(:w))) Head())",
    ];
    let class_slots = [
        "SubClassOf(:x :C)",
        "SubClassOf(:C :x)",
        "EquivalentClasses(:x :C)",
        "DisjointClasses(:x :C)",
        "DisjointUnion(:C :x :D)",
        "ClassAssertion(:x :a)",
        "ClassAssertion(ObjectComplementOf(:x) :a)",
        "ObjectPropertyDomain(:q :x)",
        "ObjectPropertyRange(:q :x)",
        "DataPropertyDomain(:d :x)",
        "HasKey(:x (:q) ())",
        "SubClassOf(:C ObjectIntersectionOf(:x :D))",
        "SubClassOf(:C ObjectUnionOf(:x :D))",
        "SubClassOf(:C ObjectComplementOf(:x))",
        "SubClassOf(:C ObjectSomeValuesFrom(:q :x))",
        "SubClassOf(:C ObjectAllValuesFrom(:q :x))",
        "SubClassOf(:C ObjectMinCardinality(1 :q :x))",
        "SubClassOf(:C ObjectMaxCardinality(1 :q :x))",
        "SubClassOf(:C ObjectExactCardinality(1 :q :x))",
        "DLSafeRule(Body(ClassAtom(:x Variable(:v))) Head())",
    ];
    let datatype_slots = [
        "DataPropertyRange(:d :x)",
        "DatatypeDefinition(:x xsd:string)",
        "DataPropertyRange(:d DatatypeRestriction(:x xsd:minLength \"1\"^^xsd:integer))",
        "DataPropertyRange(:d DataIntersectionOf(:x xsd:string))",
        "DataPropertyRange(:d DataUnionOf(:x xsd:string))",
        "DataPropertyRange(:d DataComplementOf(:x))",
        "SubClassOf(:C DataSomeValuesFrom(:d :x))",
        "SubClassOf(:C DataAllValuesFrom(:d :x))",
        "SubClassOf(:C DataMaxCardinality(1 :d :x))",
        "DLSafeRule(Body(DataRangeAtom(:x Variable(:w))) Head())",
    ];
    let annotation_slots = [
        "AnnotationAssertion(:x :a \"v\")",
        "AnnotationAssertion(Annotation(:x \"v\") rdfs:label :a \"w\")",
        "SubClassOf(Annotation(:x \"v\") :C :D)",
        "Declaration(Annotation(:x \"v\") Class(:E))",
        "SubAnnotationPropertyOf(:x rdfs:comment)",
        "SubAnnotationPropertyOf(rdfs:comment :x)",
        "AnnotationPropertyDomain(:x :C)",
        "AnnotationPropertyRange(:x xsd:string)",
    ];
    let rows = [
        ("DataProperty", &object_slots[..], OBJECT_DATA),
        ("ObjectProperty", &data_slots[..], OBJECT_DATA),
        ("Datatype", &class_slots[..], CLASS_DATATYPE),
        ("Class", &datatype_slots[..], CLASS_DATATYPE),
        ("ObjectProperty", &annotation_slots[..], OBJECT_ANNOTATION),
    ];
    let mut wrong = Vec::new();
    for (kind, slots, pair) in rows {
        for axiom in slots {
            let found = functional("", kind, axiom);
            if found != [("x".to_owned(), pair, true)] {
                wrong.push(format!("{kind} :x in {axiom}: {found:?}"));
            }
        }
    }
    // The ontology's own annotation.
    let found = functional("Annotation(:x \"v\")", "ObjectProperty", "");
    if found != [("x".to_owned(), OBJECT_ANNOTATION, true)] {
        wrong.push(format!("ObjectProperty :x in the header: {found:?}"));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Spec (*Mapping to RDF* Table 10: `x *:y xlt` is an annotation only where `*:y` is an
/// annotation property; Table 4: `owl:imports` and `owl:versionIRI` are the header's
/// vocabulary, no annotation): a statement on the ontology whose property is declared an object
/// or a data property is that property's assertion about the ontology's IRI, read as any
/// other; one of an annotation property, or of an undeclared one (as the W3C test cases read
/// it, FS2RDF-ontology-annotation-annotation-ar), is an annotation.
#[test]
fn a_header_statement_is_an_annotation_only_by_its_property() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:p a owl:ObjectProperty . ex:d a owl:DatatypeProperty .
        ex:ont a owl:Ontology ; ex:p ex:b ; ex:d "v" ; rdfs:label "o" ; ex:note "n" ;
            owl:imports ex:o2 ; owl:versionIRI ex:ont1 .
        "#,
    );
    let ontology = read(&statements, &table);
    let id = |local: &str| table.iri(&format!("{}{local}", super::EX)).expect("a term");
    let (p, d, ont, b) = (id("p"), id("d"), id("ont"), id("b"));
    let v = statements
        .iter()
        .find(|s| s.triple[1] == d)
        .expect("ex:d's statement")
        .triple[2];
    assert!(
        ontology
            .axioms
            .contains(&nrese_owl::Axiom::ObjectPropertyAssertion(p, ont, b)),
        "{:?}",
        ontology.axioms
    );
    assert!(
        ontology
            .axioms
            .contains(&nrese_owl::Axiom::DataPropertyAssertion(d, ont, v)),
        "{:?}",
        ontology.axioms
    );
    let label = table.iri("http://www.w3.org/2000/01/rdf-schema#label");
    let uses: Vec<Option<Term>> = ontology.annotation_uses.iter().copied().map(Some).collect();
    assert_eq!(uses, [label, Some(id("note"))], "rdfs:label and ex:note");
    let diagnostics = &ontology.diagnostics;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Spec (*Mapping to RDF* Tables 10 and 17, §3.2.5): the properties of the statements read
/// past as annotations, of an annotated axiom's reification but its vocabulary, and of an
/// annotation property's sub-property axiom, are annotation properties' uses; the
/// reification's `rdf:type` and roles are not.
#[test]
fn the_statements_read_past_are_annotation_property_uses() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        r#"
        ex:note a owl:AnnotationProperty .
        ex:a ex:note "n" ; rdfs:comment "c" .
        ex:A rdfs:subClassOf ex:B .
        [] a owl:Axiom ; owl:annotatedSource ex:A ; owl:annotatedProperty rdfs:subClassOf ;
            owl:annotatedTarget ex:B ; ex:why "w" .
        ex:sub rdfs:subPropertyOf rdfs:label .
        "#,
    );
    let ontology = read(&statements, &table);
    let mut uses: Vec<String> = (ontology.annotation_uses.iter())
        .map(|&t| table.iri_text(t).expect("an IRI"))
        .collect();
    uses.sort();
    let ex = |l: &str| format!("{}{l}", super::EX);
    let rdfs = |l: &str| format!("http://www.w3.org/2000/01/rdf-schema#{l}");
    let mut expected = vec![
        ex("note"),
        ex("sub"),
        ex("why"),
        rdfs("comment"),
        rdfs("label"),
    ];
    expected.sort();
    assert_eq!(uses, expected);
}

/// Spec (§5.8.1; the D16-1034 runs at the line): a property read as an object property by an
/// axiom and as a data property by a literal value (prp-dom then types the literal's
/// subject); an annotation property made functional (prp-fp equates its values); one in a
/// restriction (cls-hv2 types its subjects): each named by an axiom. An undeclared property
/// with literal and IRI values and no axiom: used as two kinds by assertions alone.
#[test]
fn a_name_used_as_two_kinds_is_reported_with_whether_an_axiom_names_it() {
    for (what, statements, expected) in [
        (
            "a domain, a literal and a node",
            "ex:C a owl:Class . ex:p rdfs:domain ex:C . ex:a ex:p \"x\" . ex:b ex:p ex:c .",
            ("p", OBJECT_DATA, true),
        ),
        (
            "a functional annotation property",
            "ex:q a owl:AnnotationProperty , owl:FunctionalProperty . ex:a ex:q ex:x , ex:y .",
            ("q", OBJECT_ANNOTATION, true),
        ),
        (
            "an annotation property in a restriction",
            "ex:q a owl:AnnotationProperty . ex:C owl:equivalentClass \
             [ a owl:Restriction ; owl:onProperty ex:q ; owl:hasValue ex:b ] . ex:a ex:q ex:b .",
            ("q", OBJECT_ANNOTATION, true),
        ),
        (
            "literal and node values, no axiom",
            "ex:a ex:creator \"N\" . ex:b ex:creator ex:n .",
            ("creator", OBJECT_DATA, false),
        ),
        (
            "a declared class used as a datatype",
            "ex:T a owl:Class , rdfs:Datatype . ex:p a owl:DatatypeProperty ; rdfs:range ex:T .",
            ("T", CLASS_DATATYPE, true),
        ),
    ] {
        let (name, pair, by_axiom) = expected;
        assert_eq!(
            turtle(statements),
            [(name.to_owned(), pair, by_axiom)],
            "{what}"
        );
    }
}

/// Spec (§5.8.1 constrains properties, classes and datatypes only; §5.9: a name may be an
/// individual and a class or a property, and a class and an object property): no name of a
/// valid OWL 2 DL ontology is reported. Every construct of the functional syntax; a class and
/// a property used as individuals and a class as a property; a declared object property's
/// statement on the ontology; annotations of an annotation property only.
#[test]
fn no_name_of_a_valid_ontology_is_reported() {
    let mut table = Table::default();
    let (everything, _) = read_functional(EVERYTHING, &mut table);
    assert_eq!(reported(&everything, &table), []);
    let found = turtle(
        r#"
        ex:C a owl:Class . ex:p a owl:ObjectProperty ; rdfs:domain ex:C .
        ex:a a ex:C ; ex:p ex:C , ex:p ; rdfs:label "a" .
        ex:C ex:p ex:b . ex:a ex:C ex:b .
        ex:ont a owl:Ontology ; ex:p ex:b ; rdfs:comment "o" .
        ex:note a owl:AnnotationProperty . ex:C ex:note "n" . ex:p ex:note ex:b .
        "#,
    );
    assert_eq!(found, []);
}

/// The reader's [`Diagnostic::Unused`] for the blank node that is the subject of
/// `property`'s statement, as `reads_more`.
fn unused(turtle: &str, property: &str) -> Option<bool> {
    let mut table = Table::default();
    let statements = load(&mut table, turtle);
    let ontology = read(&statements, &table);
    let p = table.iri(property).expect("the property");
    let node = statements
        .iter()
        .find(|s| s.triple[1] == p)
        .expect("its statement")
        .triple[0];
    ontology.diagnostics.iter().find_map(|d| match *d {
        Diagnostic::Unused {
            node: n,
            reads_more,
        } if n == node => Some(reads_more),
        _ => None,
    })
}

const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// Spec (*Mapping to RDF* Tables 12 and 13: a blank node of a structure has one form; OWL 2
/// RL's rules read a node by each form it has, prp-npa1 and prp-npa2 a negative assertion's
/// three statements untyped, and eq-rep-s and eq-rep-o copy statements along an equality):
/// a structure no axiom uses may be read as more than one structure where it has two forms,
/// a structure property twice, two restriction fillers, a negative assertion's statements,
/// or where it is also an individual of a read axiom; one form alone reads as one.
#[test]
fn an_unused_structure_reads_more_by_its_forms_or_as_an_individual() {
    let on = format!("{OWL}onProperty");
    let intersection = format!("{OWL}intersectionOf");
    let source = format!("{OWL}sourceIndividual");
    for (what, statements, property, expected) in [
        (
            "two forms",
            "_:x owl:intersectionOf ( ex:A ex:B ) ; owl:onProperty ex:p ; \
             owl:allValuesFrom ex:D .",
            &intersection,
            true,
        ),
        (
            "two set operators",
            "_:x owl:intersectionOf ( ex:A ex:B ) ; owl:unionOf ( ex:C ex:D ) .",
            &intersection,
            true,
        ),
        (
            "two fillers",
            "_:r owl:onProperty ex:p ; owl:hasValue ex:b ; owl:allValuesFrom ex:D .",
            &on,
            true,
        ),
        (
            "a structure property twice",
            "_:r owl:onProperty ex:p , ex:q ; owl:someValuesFrom ex:C .",
            &on,
            true,
        ),
        (
            "a negative assertion untyped",
            "_:n owl:sourceIndividual ex:a ; owl:assertionProperty ex:p ; \
             owl:targetIndividual ex:b .",
            &source,
            true,
        ),
        (
            "an individual of an equality",
            "_:x owl:intersectionOf ( ex:A ex:B ) . _:z owl:onProperty ex:p ; \
             owl:allValuesFrom ex:D . _:x owl:sameAs _:z . ex:x a ex:A , ex:B . ex:x ex:p ex:y .",
            &on,
            true,
        ),
        (
            "one form",
            "ex:p a owl:ObjectProperty . _:r owl:onProperty ex:p ; owl:someValuesFrom ex:C .",
            &on,
            false,
        ),
        (
            "one set operator",
            "_:x owl:intersectionOf ( ex:A ex:B ) .",
            &intersection,
            false,
        ),
    ] {
        assert_eq!(unused(statements, property), Some(expected), "{what}");
    }
}
