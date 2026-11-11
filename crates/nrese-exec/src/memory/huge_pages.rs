//! Transparent huge pages by phase (`budgets.huge_pages`): the process's switch
//! ([`set_transparent_huge_pages`](super::set_transparent_huge_pages)) as a mode, and the
//! bulk phases that hold it on under [`HugePages::Bulk`].
//!
//! Huge pages make work over large heaps faster (LUBM 100's materialisation 26 % faster)
//! and make a serving process hold more: a pool thread's allocator keeps a whole 2 MiB
//! page for a touched byte, about 13 MiB a thread after LUBM 100's queries against 6 MiB
//! without (serve-memory measurements, 11 Nov 2026). `Bulk` takes both: on while a bulk
//! load or a materialisation runs, off while serving.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// How transparent huge pages back the process's memory (Linux; elsewhere there are none
/// to switch, and every mode does nothing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HugePages {
    /// As the process was started (mimalloc asks for them for its arenas); a store opened
    /// with it leaves the mode another store set ([`set_huge_pages`]).
    #[default]
    On,
    /// Off from the store's opening on: less memory held while serving, slower work over
    /// large heaps.
    Off,
    /// On exactly while a bulk phase ([`HugePagesPhase`]) runs, off otherwise.
    Bulk,
}

/// The mode and the bulk phases alive, changed with the kernel's switch under one lock: no
/// order of phases starting and ending leaves the switch on with none alive, or off with
/// one alive. A leaf lock: nothing else is taken under it.
struct State {
    mode: HugePages,
    phases: usize,
}

static STATE: Mutex<State> = Mutex::new(State {
    mode: HugePages::On,
    phases: 0,
});

/// The state, also after a panic under its lock: its two values hold no invariant between
/// them that a panic could break halfway, and the switch is set again at the next change.
fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sets the process's mode, at a store's opening. `On`, the default, changes nothing, the
/// mode included: a store opened with the defaults (the access store, a draft check's
/// scratch store) leaves the mode the configured store set. `Off` switches huge pages off;
/// `Bulk` switches them on if a bulk phase runs and off otherwise. Between `Off` and `Bulk`
/// the last store opened sets it: process-wide, as the kernel's switch is.
pub fn set_huge_pages(mode: HugePages) {
    if mode == HugePages::On {
        return;
    }
    let mut state = state();
    state.mode = mode;
    super::set_transparent_huge_pages(mode == HugePages::Bulk && state.phases > 0);
}

/// A bulk phase (a bulk load, a materialisation) for as long as it lives: under
/// [`HugePages::Bulk`] huge pages are on while one lives, and off again once the last one
/// is dropped, however it ends (finished, failed, aborted, unwound).
#[must_use = "the phase ends when this is dropped"]
#[derive(Debug)]
pub struct HugePagesPhase {
    _private: (),
}

impl HugePagesPhase {
    /// Starts a bulk phase.
    pub fn begin() -> Self {
        let mut state = state();
        state.phases += 1;
        if state.mode == HugePages::Bulk && state.phases == 1 {
            super::set_transparent_huge_pages(true);
        }
        Self { _private: () }
    }
}

impl Drop for HugePagesPhase {
    fn drop(&mut self) {
        let mut state = state();
        state.phases -= 1;
        if state.mode == HugePages::Bulk && state.phases == 0 {
            super::set_transparent_huge_pages(false);
        }
    }
}

/// Whether huge pages may back this process now, as the kernel shows it (`THP_enabled` in
/// `/proc/self/status`, Linux 5.0 on; always `false` on a kernel built without them); `None`
/// where it can't be read: another platform or an older kernel.
pub fn transparent_huge_pages() -> Option<bool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("THP_enabled:"))?;
    Some(line.split_whitespace().nth(1)? == "1")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn enabled() -> bool {
        transparent_huge_pages().expect("shown: checked by switchable")
    }

    /// Whether this kernel lets the process's switch be set on and shows it; where not (an
    /// older kernel, one without huge pages, a container that refuses the call), the test
    /// says so and checks nothing.
    fn switchable() -> bool {
        let able = super::super::set_transparent_huge_pages(true)
            && transparent_huge_pages() == Some(true);
        if !able {
            eprintln!("not checked: this kernel doesn't show or switch the process's huge pages");
        }
        able
    }

    /// Under `Bulk`, huge pages are on exactly while a phase lives: off without one, on
    /// while one or two overlap, off once the last ends, also when it ends by unwinding.
    /// In a process of its own: the switch is the process's.
    #[test]
    fn bulk_holds_huge_pages_on_exactly_while_a_phase_lives() {
        if !crate::measure::alone() || !switchable() {
            return;
        }
        set_huge_pages(HugePages::Bulk);
        assert!(!enabled(), "serving, no phase");
        let load = HugePagesPhase::begin();
        assert!(enabled(), "a phase");
        let materialisation = HugePagesPhase::begin();
        drop(load);
        assert!(enabled(), "the other phase still runs");
        drop(materialisation);
        assert!(!enabled(), "the last phase ended");
        let unwound = std::panic::catch_unwind(|| {
            let _phase = HugePagesPhase::begin();
            assert!(enabled());
            panic!("a phase failing");
        });
        assert!(unwound.is_err());
        assert!(!enabled(), "a phase that unwound ended");
        let during = HugePagesPhase::begin();
        set_huge_pages(HugePages::Bulk);
        assert!(
            enabled(),
            "the mode set again while a phase runs keeps them on"
        );
        drop(during);
        assert!(!enabled());
    }

    /// `On`, the default, changes nothing: neither the switch nor the mode a store set
    /// before (a store opened with the defaults after a `Bulk` one leaves it `Bulk`). Under
    /// `On` and `Off`, phases change nothing.
    #[test]
    fn on_changes_nothing_and_phases_follow_only_bulk() {
        if !crate::measure::alone() || !switchable() {
            return;
        }
        drop(HugePagesPhase::begin());
        assert!(enabled(), "a phase under the default leaves the switch on");
        set_huge_pages(HugePages::Off);
        assert!(!enabled());
        let phase = HugePagesPhase::begin();
        assert!(!enabled(), "off during a phase");
        drop(phase);
        set_huge_pages(HugePages::Bulk);
        set_huge_pages(HugePages::On);
        let phase = HugePagesPhase::begin();
        assert!(
            enabled(),
            "still bulk after a store opened with the defaults"
        );
        drop(phase);
        assert!(!enabled());
        set_huge_pages(HugePages::Off);
        assert!(super::super::set_transparent_huge_pages(true));
        drop(HugePagesPhase::begin());
        assert!(enabled(), "under off, a phase's end switches nothing");
    }
}
