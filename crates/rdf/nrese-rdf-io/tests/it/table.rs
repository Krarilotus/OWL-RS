//! A table test's rows, each checked on its own: the failing rows are reported together at
//! the table's end, so one failing row doesn't hide the others. A row marked `guard` held
//! before the change it tests too: it keeps what already worked.

use std::fmt::Debug;

#[derive(Default)]
pub struct Table(Vec<String>);

impl Table {
    /// Row `label` (a guard if `guard`): its outcome.
    pub fn row(&mut self, guard: bool, label: impl Debug, outcome: Result<(), String>) {
        if let Err(why) = outcome {
            let kind = if guard { "guard " } else { "" };
            self.0.push(format!("{kind}{label:?}: {why}"));
        }
    }

    pub fn done(self) {
        assert!(
            self.0.is_empty(),
            "{} row(s) failed:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

/// `Ok` when `got` is `want`.
pub fn same<T: PartialEq + Debug>(got: T, want: T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("got {got:?}, want {want:?}"))
    }
}
