//! Dictionary 3.0b: the bounds' views, which they derive from a published snapshot alone,
//! keep its publication. This is F2's contract revision for the DL bounds (the master, 8 Nov,
//! IB2250-153c): without it the result cache refuses them, and a pinned `owl2-dl` query
//! fails, which fails a store's open.

use super::*;
use crate::StoreConfig;

/// An existential axiom (U1's route, not the OWL 2 RL shortcut) and a member of its class,
/// with a named superclass: an upper bound, and memberships the lower bound can add.
const DATA: &str = "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
     INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
         owl:someValuesFrom :B ] . :A rdfs:subClassOf :C . :a a :A . }";

/// The bounds' views of the engine's published snapshot have a token as it has: the upper
/// view (an addition of U1's facts, read with its equality) and the lower one.
#[test]
fn the_bounds_views_of_a_published_snapshot_are_published() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    store.update_str_ungated(DATA).unwrap();
    let (snapshot, view) = view(&store);
    assert!(
        snapshot.shared_state().is_some(),
        "the engine's snapshot is published"
    );
    let upper = view
        .upper
        .as_ref()
        .expect("the existential gives an upper bound");
    assert!(
        upper.shared_state().is_some(),
        "the upper view of a published snapshot has a token"
    );
    assert!(
        view.lower.shared_state().is_some(),
        "the lower view of a published snapshot has a token"
    );
}
