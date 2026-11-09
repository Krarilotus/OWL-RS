//! State an unpublished version keeps for its own readers (contract S3, "state owned by its
//! own version"; `notes/2026-11-09-dict3-0c-own-state-design.md`): the other half of
//! [`Snapshot::admission`] beside [`SharedState`].

use std::any::{Any, TypeId};
use std::sync::Arc;

use parking_lot::Mutex;

use super::{SharedState, Snapshot};

/// Where a snapshot's derived state may be kept ([`Snapshot::admission`]): every snapshot
/// gets exactly one of the two.
#[derive(Clone, Copy)]
pub enum Admission<'s> {
    /// A published snapshot, or a restriction of one: it may admit into state other
    /// operations reach.
    Shared(SharedState<'s>),
    /// A pending, private, staged or added-to snapshot: what it derives is kept in its own
    /// version's slots, for readers of that version alone.
    Own(OwnState<'s>),
}

/// The capability of an unpublished snapshot to keep derived state in its own version, one
/// value per type. The version holds each value until the version's last holder drops it;
/// a value is released then only if nothing else holds it too. An `Arc<T>` a caller kept
/// from [`get`](Self::get) or [`get_or_insert_with`](Self::get_or_insert_with) keeps the
/// value (and its charges) alive past the version, and a value that holds its own version
/// (a snapshot of it, directly or through what it keeps) is a cycle that is never released:
/// a value keys by identity and never stores a [`Snapshot`] of its version. Every view of one
/// version reaches the same value (`with_equality`, `stored`, the canonical views,
/// `reading_within`), so a value keys what it holds by full
/// [`SnapshotIdentity`](crate::SnapshotIdentity). The slots' lock is a leaf: held only to
/// look a value up or store one, never while a value is made or dropped.
///
/// ```
/// let engine = nrese_engine::Engine::new(Default::default()).unwrap();
/// let pending = engine.speculative().pending_snapshot();
/// let nrese_engine::Admission::Own(own) = pending.admission() else {
///     panic!("a pending snapshot is unpublished");
/// };
/// let first = own.get_or_insert_with(|| 7_u32);
/// assert!(std::sync::Arc::ptr_eq(&first, &own.get_or_insert_with(|| 8_u32)));
/// assert!(matches!(engine.snapshot().admission(), nrese_engine::Admission::Shared(_)));
/// ```
///
/// No code outside the engine can make one:
///
/// ```compile_fail
/// fn forge(snapshot: &nrese_engine::Snapshot) -> nrese_engine::OwnState<'_> {
///     nrese_engine::OwnState { snapshot }
/// }
/// ```
#[derive(Clone, Copy)]
pub struct OwnState<'s> {
    snapshot: &'s Snapshot,
}

impl<'s> OwnState<'s> {
    /// For [`Snapshot::admission`] alone, of an unpublished snapshot.
    pub(super) fn new(snapshot: &'s Snapshot) -> Self {
        Self { snapshot }
    }

    /// The unpublished snapshot it was taken from.
    pub fn snapshot(&self) -> &'s Snapshot {
        self.snapshot
    }

    fn slots(&self) -> &'s OwnSlots {
        &self.snapshot.version.own
    }

    /// The version's value of type `T`, if one was stored. The `Arc` returned keeps the value
    /// alive while it is held, past the version's last holder too ([`OwnState`]).
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.slots().get()
    }

    /// The version's value of type `T`, made by `make` if there is none. `make` runs without
    /// the slots' lock, so it may read the snapshot and its other slots; where two calls make
    /// one together, the first value stored is kept and returned to both, and the other is
    /// dropped. A value that may be refused is a container made empty, refusing inside it.
    /// `make` must not capture a [`Snapshot`] of this version into the value: the value would
    /// then hold its own version, and neither would be released ([`OwnState`]).
    pub fn get_or_insert_with<T: Send + Sync + 'static>(&self, make: impl FnOnce() -> T) -> Arc<T> {
        if let Some(value) = self.get() {
            return value;
        }
        let made = Arc::new(make());
        // A value stored meanwhile wins; `made` is then dropped here, after the lock.
        self.slots().keep(made)
    }
}

/// A version's own values, one per type ([`OwnState`]). Empty at every construction, and
/// never copied to another version: an installed version is made as a value
/// (`Versions::install`), so no snapshot can have filled it. Dropped with the version;
/// a value outlives it only through an `Arc` held elsewhere ([`OwnState`]).
#[derive(Default)]
pub(crate) struct OwnSlots(Mutex<Vec<(TypeId, Arc<dyn Any + Send + Sync>)>>);

impl OwnSlots {
    fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let slots = self.0.lock();
        found::<T>(&slots)
    }

    /// The value of type `T` already stored, else `made`, stored. A `made` that lost is
    /// dropped after the lock: its drop may release charges.
    fn keep<T: Send + Sync + 'static>(&self, made: Arc<T>) -> Arc<T> {
        let mut slots = self.0.lock();
        match found::<T>(&slots) {
            Some(kept) => {
                drop(slots);
                drop(made);
                kept
            }
            None => {
                slots.push((
                    TypeId::of::<T>(),
                    Arc::clone(&made) as Arc<dyn Any + Send + Sync>,
                ));
                made
            }
        }
    }
}

fn found<T: Send + Sync + 'static>(
    slots: &[(TypeId, Arc<dyn Any + Send + Sync>)],
) -> Option<Arc<T>> {
    let (_, value) = slots.iter().find(|(id, _)| *id == TypeId::of::<T>())?;
    Some(
        Arc::clone(value)
            .downcast()
            .unwrap_or_else(|_| unreachable!("a slot holds a value of the type it is keyed by")),
    )
}

impl std::fmt::Debug for OwnSlots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let values = self.0.try_lock().map(|slots| slots.len());
        f.debug_struct("OwnSlots").field("values", &values).finish()
    }
}
