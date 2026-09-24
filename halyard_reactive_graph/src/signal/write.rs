use super::ArcWriteSignal;
use crate::{
    owner::{ArenaItem, FromLocal, LocalStorage, Storage, SyncStorage},
    traits::{
        DefinedAt, Dispose, IntoInner, IsDisposed, Notify, UntrackableGuard,
        Write,
    },
};
use core::fmt::Debug;
use std::{hash::Hash, ops::DerefMut, panic::Location};

/// An arena-allocated setter for a reactive signal.
///
/// A signal is a piece of data that may change over time,
/// and notifies other code when it has changed.
///
/// This is an arena-allocated signal, which is `Copy` and is disposed when its reactive
/// [`Owner`](crate::owner::Owner) cleans up. For a reference-counted signal that lives
/// as long as a reference to it is alive, see [`ArcWriteSignal`].
///
/// ## Core Trait Implementations
/// - [`.set()`](crate::traits::Set) sets the signal to a new value.
/// - [`.update()`](crate::traits::Update) updates the value of the signal by
///   applying a closure to a copy of it, which is then committed.
/// - [`.write()`](crate::traits::Write) returns a guard through which the signal
///   can be mutated, and which commits and notifies subscribers when it is dropped.
///
/// > Each of these has a related `_untracked()` method, which updates the signal
/// > without notifying subscribers. Untracked updates are not desirable in most
/// > cases, as they cause “tearing” between the signal’s value and its observed
/// > value. If you want a non-reactive container, use [`ArenaItem`] instead.
///
/// ## Examples
/// ```
/// # use halyard_reactive_graph::prelude::*; use halyard_reactive_graph::signal::*;  let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// let (count, set_count) = signal(0);
///
/// // ✅ calling the setter sets the value
/// set_count.set(1);
/// assert_eq!(count.try_get(), Some(1));
///
/// // ❌ you could call the getter within the setter
/// // set_count.set(count.get() + 1);
///
/// // ✅ however it's simpler to use .update(), which changes a copy and commits it
/// set_count.update(|count: &mut i32| *count += 1);
/// assert_eq!(count.try_get(), Some(2));
///
/// // ✅ `.write()` returns a guard that implements `DerefMut` and will notify when dropped
/// *set_count.try_write().unwrap() += 1;
/// assert_eq!(count.try_get(), Some(3));
/// ```
pub struct WriteSignal<T, S = SyncStorage> {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    pub(crate) defined_at: &'static Location<'static>,
    pub(crate) inner: ArenaItem<ArcWriteSignal<T>, S>,
}
crate::impl_weak!([T, S] WriteSignal<T, S>);

impl<T, S> Dispose for WriteSignal<T, S> {
    fn dispose(self) {
        self.inner.dispose()
    }
}

impl<T, S> Copy for WriteSignal<T, S> {}

impl<T, S> Clone for WriteSignal<T, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, S> Debug for WriteSignal<T, S>
where
    S: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteSignal")
            .field("type", &std::any::type_name::<T>())
            .field("store", &self.inner)
            .finish()
    }
}

impl<T, S> PartialEq for WriteSignal<T, S> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<T, S> Eq for WriteSignal<T, S> {}

impl<T, S> Hash for WriteSignal<T, S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl<T, S> DefinedAt for WriteSignal<T, S> {
    fn defined_at(&self) -> Option<&'static Location<'static>> {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        {
            Some(self.defined_at)
        }
        #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
        {
            None
        }
    }
}

impl<T> From<ArcWriteSignal<T>> for WriteSignal<T>
where
    T: Send + Sync + 'static,
{
    #[track_caller]
    fn from(value: ArcWriteSignal<T>) -> Self {
        WriteSignal {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner: ArenaItem::new_with_storage(value),
        }
    }
}

impl<T> FromLocal<ArcWriteSignal<T>> for WriteSignal<T, LocalStorage>
where
    T: 'static,
{
    #[track_caller]
    fn from_local(value: ArcWriteSignal<T>) -> Self {
        WriteSignal {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner: ArenaItem::new_with_storage(value),
        }
    }
}

impl<T, S> IsDisposed for WriteSignal<T, S> {
    fn is_disposed(&self) -> bool {
        self.inner.is_disposed()
    }
}

impl<T, S> IntoInner for WriteSignal<T, S>
where
    S: Storage<ArcWriteSignal<T>>,
{
    type Value = T;

    #[inline(always)]
    fn into_inner(self) -> Option<Self::Value> {
        self.inner.into_inner()?.into_inner()
    }
}

impl<T, S> Notify for WriteSignal<T, S>
where
    T: 'static,
    S: Storage<ArcWriteSignal<T>>,
{
    fn notify(&self) {
        if let Some(inner) = self.inner.try_get_value() {
            inner.notify();
        } else {
            crate::gone::report_gone(
                crate::gone::Attempt::Write,
                "WriteSignal",
                self.defined_at(),
                std::panic::Location::caller(),
            );
        }
    }
}

impl<T, S> Write for WriteSignal<T, S>
where
    T: 'static,
    S: Storage<ArcWriteSignal<T>>,
{
    type Value = T;

    /// A guard holding a copy of the value, committed when it is dropped (deferred while
    /// this thread is using the signal).
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        T: Clone,
    {
        self.inner.try_get_value()?.snapshot_guard()
    }

    /// Changes the value in place. Waits while another thread writes it; `None` if this
    /// thread is using it (the write is inside the signal's own `with` or `update`, or a
    /// guard of it is alive), which would never end. That is logged once.
    fn try_write_in_place(
        &self,
    ) -> Option<impl DerefMut<Target = Self::Value>> {
        self.inner.try_get_value()?.in_place_guard()
    }

    fn try_commit_value(&self, value: T) -> Option<T> {
        match self.inner.try_get_value() {
            Some(inner) => inner.try_commit_value(value),
            None => Some(value),
        }
    }

    fn try_update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U>
    where
        T: Clone,
    {
        self.inner.try_get_value()?.try_update_snapshot(fun)
    }

    fn try_update_in_place<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U> {
        self.inner.try_get_value()?.try_update_in_place(fun)
    }
}
