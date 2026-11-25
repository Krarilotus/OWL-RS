mod case;
mod draw;
mod gaps;
mod matrix;
pub(super) mod observe;
mod random;
mod shrink;
mod stops;
mod witnesses;

// Spec: the SHACL change differential checks each hand-pinned matrix row.
#[test]
fn matrix() {
    matrix::run();
}

// Spec: under drawn data, inferred and shapes changes, each change report holds exactly
// the introduced keys and the later validation's failures.
#[test]
fn random_changes() {
    random::run();
}
