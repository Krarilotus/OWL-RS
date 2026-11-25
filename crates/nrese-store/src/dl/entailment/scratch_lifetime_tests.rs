use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut};

use nrese_exec::{Envelope, Reservation, SharedBudget, measure::Measured};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Payload {
    Copy,
    Joint,
}

thread_local! {
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
    static ADDRESSES: Cell<[usize; 2]> = const { Cell::new([0; 2]) };
}

struct Watch {
    payload: Payload,
    sibling: Envelope,
    registered: bool,
    receipt: Option<(usize, bool, bool)>,
}

fn watch(payload: Payload, addresses: [usize; 2]) {
    WATCH.with_borrow_mut(|slot| {
        if let Some(w) = slot.as_mut().filter(|w| w.payload == payload) {
            w.registered = true;
            ADDRESSES.set(addresses);
        }
    });
}

pub(super) fn watch_joint(o: &Ontology) {
    watch(Payload::Joint, [o.axioms.as_ptr() as usize, 0]);
}

pub(crate) struct WatchFrees;

static MEASURED: Measured<System> = Measured(System);

fn moved(from: *mut u8, to: *mut u8) {
    let _ = ADDRESSES.try_with(|watched| {
        let mut addresses = watched.get();
        for address in &mut addresses {
            if *address == from as usize {
                *address = to as usize;
            }
        }
        watched.set(addresses);
    });
}

// SAFETY: All allocation operations forward to the same allocator with the original
// arguments. The observer compares addresses only, after the allocator returns.
unsafe impl GlobalAlloc for WatchFrees {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller provides the required nonzero layout.
        unsafe { MEASURED.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller provides the required nonzero layout.
        unsafe { MEASURED.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller provides a live pointer and its original layout.
        unsafe { MEASURED.dealloc(ptr, layout) };
        moved(ptr, std::ptr::null_mut());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: The caller provides a live pointer, its layout, and a valid new size.
        let new = unsafe { MEASURED.realloc(ptr, layout, size) };
        if !new.is_null() {
            moved(ptr, new);
        }
        new
    }
}

pub(super) struct ObservedScratch(Scratch, Payload);

impl ObservedScratch {
    pub(super) fn copy(scratch: Scratch, o: &Ontology) -> Self {
        watch(
            Payload::Copy,
            [o.axioms.as_ptr() as usize, o.sources.as_ptr() as usize],
        );
        Self(scratch, Payload::Copy)
    }

    pub(super) fn joint(scratch: Scratch) -> Self {
        Self(scratch, Payload::Joint)
    }
}

impl Deref for ObservedScratch {
    type Target = Scratch;

    fn deref(&self) -> &Scratch {
        &self.0
    }
}

impl DerefMut for ObservedScratch {
    fn deref_mut(&mut self) -> &mut Scratch {
        &mut self.0
    }
}

impl Drop for ObservedScratch {
    fn drop(&mut self) {
        WATCH.with_borrow_mut(|slot| {
            let Some(w) = slot.as_mut().filter(|w| w.payload == self.1) else {
                return;
            };
            let bytes = self.0.bytes();
            let room = w.sibling.remaining();
            drop(take(&mut self.0));
            let live = ADDRESSES.get() != [0; 2];
            let mut reclaimed = Reservation::new(w.sibling.clone());
            let available =
                w.sibling.remaining() == room + bytes && reclaimed.charge(room + bytes).is_ok();
            w.receipt = Some((bytes, available, live));
        });
    }
}

/// Spec (contracts.md, Memory): a second request can reclaim private-copy scratch only
/// once the corresponding storage has been freed, on success and on memory refusal.
#[test]
fn scratch_outlives_private_storage() {
    use super::import_tests::{StubTerms, test_budget};

    #[derive(Debug, PartialEq, Eq)]
    enum Exit {
        ImportRefusal,
        EntailmentCompletion,
        JointMemoRefusal,
    }

    const N: usize = 262_144;
    let mut premise = Ontology::default();
    premise.axioms = (0..N as u64)
        .map(|i| Axiom::ObjectPropertyAssertion(1, 10 + i, 10 + i))
        .collect();
    premise.sources.resize_with(N, Vec::new);
    premise.classes.intern(ClassExpr::Class(2));

    let empty = Ontology::default();
    let mut conclusion = Ontology::default();
    conclusion
        .axioms
        .push(Axiom::ObjectPropertyAssertion(1, 3, 4));
    conclusion.sources.push(Vec::new());
    let deadline = Deadline::after(None);
    let tight = |env: Option<&Envelope>| {
        let env = env.expect("bounded request");
        let mut guard = Reservation::new(env.clone());
        guard
            .charge(env.remaining())
            .expect("take remaining headroom");
        Some(guard)
    };

    let mut early_releases = Vec::new();
    for (case, payload) in [
        (Exit::ImportRefusal, Payload::Copy),
        (Exit::EntailmentCompletion, Payload::Copy),
        (Exit::JointMemoRefusal, Payload::Joint),
    ] {
        let parent = SharedBudget::new(128 << 20);
        let sibling = Envelope::exact(None, Some(parent.clone()));
        let budget = test_budget(Some(Envelope::exact(None, Some(parent))));
        WATCH.replace(Some(Watch {
            payload,
            sibling,
            registered: false,
            receipt: None,
        }));
        let answer = match case {
            Exit::ImportRefusal => {
                test_hooks::IMPORT_ENTRY.replace(Some(Box::new(tight)));
                let result = import_into(&premise, &conclusion, &budget, deadline);
                test_hooks::IMPORT_ENTRY.replace(None);
                result.err().expect("import charge must refuse")
            }
            Exit::EntailmentCompletion => {
                let prepared = import_into(&premise, &empty, &budget, deadline)
                    .expect("the empty conclusion must import");
                entails_prepared_within(prepared, false, &[], &budget, deadline)
            }
            Exit::JointMemoRefusal => {
                test_hooks::MEMBERSHIP_ENTRY.replace(Some(Box::new(tight)));
                let result = admit_and_prepare(
                    &premise,
                    &conclusion,
                    &StubTerms,
                    &|_| String::new(),
                    &budget,
                    deadline,
                );
                test_hooks::MEMBERSHIP_ENTRY.replace(None);
                result.err().expect("memo charge must refuse")
            }
        };
        if case == Exit::EntailmentCompletion {
            assert_eq!(answer, Entailed::Yes, "{case:?}");
        } else {
            assert_eq!(answer, Entailed::from(Stopped::Memory), "{case:?}");
        }
        let watch = WATCH.replace(None).unwrap();
        assert!(watch.registered, "{case:?}: storage watch must run");
        let (bytes, available, live) = watch.receipt.expect("scratch released");
        assert!(
            bytes > 0 && available,
            "{case:?}: sibling must reclaim {bytes} bytes"
        );
        assert_eq!(
            ADDRESSES.replace([0; 2]),
            [0; 2],
            "{case:?}: storage must be freed"
        );
        if live {
            early_releases.push(case);
        }
    }
    assert!(
        early_releases.is_empty(),
        "scratch released with live storage: {early_releases:?}"
    );
}
