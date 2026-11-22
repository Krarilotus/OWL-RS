//! The SHACL gate's kept state ([`crate::StoreService::shacl_gate_state`]).

/// Writes publish in revision order but keep their shapes in any order: a bulk load or a
/// replica's records compile after their publication, a commit keeps what its check
/// compiled. The state is the latest revision's, whichever is kept last.
#[test]
fn shapes_compiled_at_an_earlier_revision_never_replace_later_ones() {
    let store = crate::StoreService::new(crate::StoreConfig::in_memory()).expect("store");
    store.keep_shapes(7, Ok((3, false)));
    store.keep_shapes(5, Ok((0, true)));
    let state = store.shacl_gate_state();
    assert_eq!(
        (state.revision, state.shapes, state.vacuous),
        (7, Some(3), Some(false)),
        "{state:?}"
    );
}
