use super::check;
use crate::generator::{Write, case, delete, insert, long_chain, rows};
use crate::triple;

const WATCH: [&str; 3] = ["ex:a", "ex:out", "ex:z"];
const SUBCLASS: &str = "ex:a rdfs:subClassOf ex:b";

fn chain(first: &str, second: &str, data: &str) -> Write {
    insert(&format!(
        "ex:out owl:propertyChainAxiom ex:h
         ex:h rdf:first {first}
         ex:h rdf:rest ex:t
         ex:t rdf:first {second}
         ex:t rdf:rest rdf:nil
         {data}"
    ))
}

fn check_operand(shape: &'static str, watch: [&str; 3], setup: Write, edge: &str) {
    let added = insert(edge);
    let mut loaded = setup.clone();
    loaded.insert.extend_from_slice(&added.insert);
    check(
        case(shape, watch, vec![setup, added]),
        &[true; 2],
        &[false, true],
    );
    check(
        case(shape, watch, vec![loaded, delete(edge)]),
        &[true; 2],
        &[true, false],
    );
}

#[test]
fn subclass_in_second_position() {
    // Table 5 prp-spo2 reads the subclass edge after the ex:q edge.
    check_operand(
        "second-position",
        ["ex:z", "ex:out", "ex:b"],
        chain("ex:q", "rdfs:subClassOf", "ex:z ex:q ex:a"),
        SUBCLASS,
    );
}

#[test]
fn subclass_supplied_by_a_subproperty() {
    // Table 5 prp-spo1 derives the subclass edge that prp-spo2 needs.
    check_operand(
        "derived-subclass",
        WATCH,
        chain(
            "rdfs:subClassOf",
            "ex:q",
            "ex:sub rdfs:subPropertyOf rdfs:subClassOf
             ex:b ex:q ex:z",
        ),
        "ex:a ex:sub ex:b",
    );
}

#[test]
fn equivalent_class_operand() {
    // Table 5 prp-spo2 also reads owl:equivalentClass as a property.
    check_operand(
        "equivalent-class-operand",
        WATCH,
        chain("owl:equivalentClass", "ex:q", "ex:b ex:q ex:z"),
        "ex:a owl:equivalentClass ex:b",
    );
}

#[test]
fn subclass_operand_in_a_long_chain() {
    // Tables 3 and 5 prp-spo2 reach the endpoint beyond the rule encoder's 250 atoms.
    let [mut setup, data]: [Write; 2] = long_chain(251).writes.try_into().unwrap();
    let first = triple(["ex:c0", "rdf:first", "ex:p"]);
    let operand = setup.insert.iter_mut().find(|r| **r == first).unwrap();
    *operand = triple(["ex:c0", "rdf:first", "rdfs:subClassOf"]);
    setup.insert.extend(data.insert.into_iter().skip(1));
    check_operand(
        "long-chain-subclass-operand",
        ["ex:x0", "ex:out", "ex:x251"],
        setup,
        "ex:x0 rdfs:subClassOf ex:x1",
    );
}

#[test]
fn subclass_and_list_edit_in_one_commit() {
    // Table 5 prp-spo2 reads the revised list and the edge committed with it.
    let setup = chain("rdfs:subClassOf", "ex:unused", "ex:b ex:q ex:z");
    let edit = Write {
        insert: rows("ex:a rdfs:subClassOf ex:b\nex:t rdf:first ex:q"),
        delete: rows("ex:t rdf:first ex:unused"),
        ..Write::default()
    };
    let undo = Write {
        insert: edit.delete.clone(),
        delete: edit.insert.clone(),
        ..Write::default()
    };
    check(
        case("list-and-operand-edit", WATCH, vec![setup, edit, undo]),
        &[true; 3],
        &[false, true, false],
    );
}

#[test]
fn subclass_operand_through_equality() {
    // Table 4 eq-rep-s lets Table 5 prp-spo2 read the edge at either name.
    check_operand(
        "subclass-operand-through-equality",
        WATCH,
        chain(
            "rdfs:subClassOf",
            "ex:q",
            "ex:a2 owl:sameAs ex:a
             ex:b ex:q ex:z",
        ),
        "ex:a2 rdfs:subClassOf ex:b",
    );
}
