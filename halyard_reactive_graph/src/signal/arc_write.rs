use crate::{
    graph::SubscriberSet,
    prelude::{IsDisposed, Notify},
    traits::{DefinedAt, IntoInner, UntrackableGuard, Write},
};
use core::fmt::{Debug, Formatter, Result};
use std::{
    hash::Hash,
    panic::Location,
    sync::{Arc, Mutex, PoisonError, RwLock},
};

/// A reference-counted setter for a reactive signal.
///
/// A signal is a piece of data that may change over time,
/// and notifies other code when it has changed.
///
/// This is a reference-counted signal, which is `Clone` but not `Copy`.
/// For arena-allocated `Copy` signals, use [`WriteSignal`](super::WriteSignal).
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
/// > value. If you want a non-reactive container, used [`ArenaItem`](crate::owner::ArenaItem)
/// > instead.
///
/// ## Examples
/// ```
/// # use halyard_reactive_graph::prelude::*; use halyard_reactive_graph::signal::*;
/// let (count, set_count) = arc_signal(0);
///
/// // ✅ calling the setter sets the value
/// set_count.set(1);
/// assert_eq!(count.get(), 1);
///
/// // ❌ you could call the getter within the setter
/// // set_count.set(count.get() + 1);
///
/// // ✅ however it's simpler to use .update(), which changes a copy and commits it
/// set_count.update(|count: &mut i32| *count += 1);
/// assert_eq!(count.get(), 2);
///
/// // ✅ `.write()` returns a guard that implements `DerefMut` and will notify when dropped
/// *set_count.write() += 1;
/// assert_eq!(count.get(), 3);
/// ```
pub struct ArcWriteSignal<T> {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    pub(crate) defined_at: &'static Location<'static>,
    pub(crate) value: Arc<RwLock<T>>,
    pub(crate) inner: Arc<RwLock<SubscriberSet>>,
    /// The writer turn: writes to the signal serialize on it (see `commit.rs`).
    pub(crate) turn: Arc<Mutex<()>>,
}
crate::impl_strong!([T] ArcWriteSignal<T>);

impl<T> ArcWriteSignal<T> {
    /// Returns a weak (arena) handle to the value: `Copy`, and it does not keep the value
    /// alive (like [`std::sync::Arc::downgrade`]). The reverse is `upgrade` on the weak
    /// handle.
    #[track_caller]
    pub fn downgrade(&self) -> crate::signal::WriteSignal<T>
    where
        crate::signal::WriteSignal<T>: From<Self>,
    {
        self.clone().into()
    }
}

impl<T> Clone for ArcWriteSignal<T> {
    #[track_caller]
    fn clone(&self) -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
            value: Arc::clone(&self.value),
            inner: Arc::clone(&self.inner),
            turn: Arc::clone(&self.turn),
        }
    }
}

impl<T> Debug for ArcWriteSignal<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.debug_struct("ArcWriteSignal")
            .field("type", &std::any::type_name::<T>())
            .field("value", &Arc::as_ptr(&self.value))
            .finish()
    }
}

impl<T> PartialEq for ArcWriteSignal<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }
}

impl<T> Eq for ArcWriteSignal<T> {}

impl<T> Hash for ArcWriteSignal<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(&Arc::as_ptr(&self.value), state);
    }
}

impl<T> DefinedAt for ArcWriteSignal<T> {
    #[inline(always)]
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

impl<T> IsDisposed for ArcWriteSignal<T> {
    #[inline(always)]
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T> IntoInner for ArcWriteSignal<T> {
    type Value = T;

    #[inline(always)]
    fn into_inner(self) -> Option<Self::Value> {
        // a lock poisoned by a panic still holds the value
        Some(
            Arc::into_inner(self.value)?
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

impl<T: 'static> Notify for ArcWriteSignal<T> {
    /// Deferred while this thread is using the signal.
    fn notify(&self) {
        self.notify_or_defer();
    }
}

impl<T: 'static> Write for ArcWriteSignal<T> {
    type Value = T;

    /// A guard holding a copy of the value, committed when it is dropped (deferred while
    /// this thread is using the signal).
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        T: Clone,
    {
        self.snapshot_guard()
    }

    fn try_commit_value(&self, value: T, notify: bool) -> Option<T> {
        self.set_value(value, notify);
        None
    }

    fn try_update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U>
    where
        T: Clone,
    {
        self.update_snapshot(fun)
    }
}
