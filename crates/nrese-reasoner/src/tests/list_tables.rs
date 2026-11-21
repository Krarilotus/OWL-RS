use super::{ListVocabulary, LocalVocabulary, Ruleset, Schema, batch, load};
use crate::delta::{self, MemoryBase, Rules};

#[test]
fn short_cyclic_memberships_and_keys_need_no_private_facts() {
    // Spec: OWL 2 RL Tables 3, 5 and 6 give U, I and sameAs from the
    // three simple walks. Each turn around a loop repeats a premise.
    let mut vocabulary = LocalVocabulary::default();
    let input = load(
        &mut vocabulary,
        "
        ex:U owl:unionOf ex:u
        ex:u rdf:first ex:A
        ex:u rdf:rest ex:u
        ex:u rdf:rest rdf:nil
        ex:I owl:intersectionOf ex:i
        ex:i rdf:first ex:A
        ex:i rdf:rest ex:i
        ex:i rdf:rest rdf:nil
        ex:K owl:hasKey ex:k
        ex:k rdf:first ex:p
        ex:k rdf:rest ex:k
        ex:k rdf:rest rdf:nil
        ex:a rdf:type ex:A
        ex:a rdf:type ex:K
        ex:b rdf:type ex:K
        ex:a ex:p ex:v
        ex:b ex:p ex:v
        ",
    );
    let answers = load(
        &mut vocabulary,
        "ex:a rdf:type ex:U\nex:a rdf:type ex:I\nex:a owl:sameAs ex:b",
    );
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let closure = batch::materialise(&input, &rules, Some(&lists), &schema);
    assert!(answers.iter().all(|row| closure.derived.contains(row)));
    let base = MemoryBase::new(&input, &closure.derived);
    let program = delta::program(
        &base,
        Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
            equality: crate::representatives::same_as(&rules, schema.same_as()),
        },
    );
    assert!(program.table.is_none());
}
