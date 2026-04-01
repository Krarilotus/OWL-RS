use crate::dataset_index::IndexedDataset;
use crate::property_chain::PropertyChainPlan;
use crate::test_support::OwnedSnapshot;

#[test]
fn property_chain_plan_accepts_binary_named_node_list() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-head",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-tail",
        ),
        (
            "http://example.com/chain-tail",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p2",
        ),
        (
            "http://example.com/chain-tail",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let plan = PropertyChainPlan::build(&index);

    assert_eq!(plan.chain_count(), 1);
    assert!(plan.diagnostics().is_empty());
}

#[test]
fn property_chain_plan_reports_longer_chain_as_bounded_unsupported() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-1",
        ),
        (
            "http://example.com/chain-1",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-1",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-2",
        ),
        (
            "http://example.com/chain-2",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p2",
        ),
        (
            "http://example.com/chain-2",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-3",
        ),
        (
            "http://example.com/chain-3",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p3",
        ),
        (
            "http://example.com/chain-3",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let plan = PropertyChainPlan::build(&index);

    assert_eq!(plan.chain_count(), 0);
    assert!(
        plan.diagnostics()
            .iter()
            .any(|message| message.contains("beyond binary chains"))
    );
}

#[test]
fn property_chain_plan_reports_malformed_list() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p1",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let plan = PropertyChainPlan::build(&index);

    assert_eq!(plan.chain_count(), 0);
    assert!(
        plan.diagnostics()
            .iter()
            .any(|message| message.contains("malformed named-node RDF lists"))
    );
}
