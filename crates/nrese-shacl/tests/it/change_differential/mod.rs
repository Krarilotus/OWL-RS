mod case;
mod gaps;
mod matrix;
pub(super) mod observe;
mod stops;
mod witnesses;

// Spec: the SHACL change differential checks each hand-pinned matrix row.
#[test]
fn matrix() {
    matrix::run();
}
