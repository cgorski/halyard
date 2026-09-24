use super::{
    guards::{CopyWriteGuard, Mapped},
    ArcRwSignal, RwSignal,
};
use crate::{
    owner::{StoredValue, SyncStorage},
    traits::{
        DefinedAt, IsDisposed, Notify, Track, TryGetValue, TryReadUntracked,
        UntrackableGuard, Write,
    },
};
use std::{
    fmt::Debug,
    ops::{Deref, DerefMut},
    panic::Location,
    sync::Arc,
};

/// A derived signal type that wraps an [`ArcRwSignal`] with a mapping function,
///  allowing you to read or write directly to one of its field.
///
/// Tracking the mapped signal tracks changes to *any* part of the signal, and updating the signal notifies
/// and notifies *all* dependencies of the signal. This is not a mechanism for fine-grained reactive updates
/// to more complex data structures. Instead, it allows you to provide a signal-like API for wrapped types
/// without exposing the original type directly to users.
///
/// Writing it replaces the mapped part of the signal's value (`set`), or changes a copy of the
/// part (`update`, `write()`; the part must be `Clone`), then swaps it in: the signal's value
/// is never lent out, and it need not be `Clone`. A write made while this thread is using the
/// signal (inside its `with` or `update`, or while a guard of it is alive) is refused and
/// logged.
pub struct ArcMappedSignal<T> {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
    #[allow(clippy::type_complexity)]
    try_read_untracked: Arc<
        dyn Fn() -> Option<DoubleDeref<Box<dyn Deref<Target = T>>>>
            + Send
            + Sync,
    >,
    /// Swaps a new part in (the argument then holds the previous part), notifying if asked;
    /// `false` if the write could not be made.
    swap: Arc<SwapPart<T>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    track: Arc<dyn Fn() + Send + Sync>,
}
crate::impl_strong!([T] ArcMappedSignal<T>);

/// Swaps a new part into the signal's value (the argument then holds the previous part),
/// notifying if asked; `false` if the write could not be made.
type SwapPart<T> = dyn Fn(&mut T, bool) -> bool + Send + Sync;

impl<T> Clone for ArcMappedSignal<T> {
    fn clone(&self) -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
            try_read_untracked: self.try_read_untracked.clone(),
            swap: self.swap.clone(),
            notify: self.notify.clone(),
            track: self.track.clone(),
        }
    }
}

impl<T> ArcMappedSignal<T> {
    /// Wraps a signal with the given mapping functions for shared and exclusive references.
    #[track_caller]
    pub fn new<U>(
        inner: ArcRwSignal<U>,
        map: fn(&U) -> &T,
        map_mut: fn(&mut U) -> &mut T,
    ) -> Self
    where
        T: 'static,
        U: Send + Sync + 'static,
    {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            try_read_untracked: {
                let this = inner.clone();
                Arc::new(move || {
                    this.try_read_untracked().map(|guard| DoubleDeref {
                        inner: Box::new(Mapped::new_with_guard(guard, map))
                            as Box<dyn Deref<Target = T>>,
                    })
                })
            },
            swap: {
                let this = inner.clone();
                Arc::new(move |part: &mut T, notify| {
                    this.writer().swap_part(part, map_mut, notify)
                })
            },
            notify: {
                let this = inner.clone();
                Arc::new(move || {
                    this.notify();
                })
            },
            track: {
                Arc::new(move || {
                    inner.track();
                })
            },
        }
    }
}

impl<T> Debug for ArcMappedSignal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut partial = f.debug_struct("ArcMappedSignal");
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        partial.field("defined_at", &self.defined_at);
        partial.finish()
    }
}

impl<T> DefinedAt for ArcMappedSignal<T> {
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

impl<T> Notify for ArcMappedSignal<T> {
    fn notify(&self) {
        (self.notify)()
    }
}

impl<T> Track for ArcMappedSignal<T> {
    fn track(&self) {
        (self.track)()
    }
}

impl<T> TryReadUntracked for ArcMappedSignal<T> {
    type Value = DoubleDeref<Box<dyn Deref<Target = T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        (self.try_read_untracked)()
    }
}

impl<T> IsDisposed for ArcMappedSignal<T> {
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T> Write for ArcMappedSignal<T>
where
    T: 'static,
{
    type Value = T;

    /// A guard over a copy of the mapped part, swapped into the signal's value when it is
    /// dropped.
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        T: Clone,
    {
        self.copy_guard()
    }

    fn try_commit_value(&self, value: T, notify: bool) -> Option<T> {
        let mut value = value;
        if (self.swap)(&mut value, notify) {
            // the previous part
            drop(value);
            None
        } else {
            Some(value)
        }
    }

    fn try_update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U>
    where
        T: Clone,
    {
        let mut part = self.try_read_untracked().map(|part| (*part).clone())?;
        let (changed, out) = fun(&mut part);
        (self.swap)(&mut part, changed).then_some(out)
    }
}

impl<T> ArcMappedSignal<T>
where
    T: 'static,
{
    /// A guard over a copy of the mapped part, swapped in when it is dropped.
    fn copy_guard(&self) -> Option<CopyWriteGuard<T>>
    where
        T: Clone,
    {
        let part = self.try_read_untracked().map(|part| (*part).clone())?;
        let swap = Arc::clone(&self.swap);
        Some(CopyWriteGuard::new(part, move |part, notify| {
            swap(part, notify)
        }))
    }
}

/// A wrapper for a smart pointer that implements [`Deref`] and [`DerefMut`]
/// by dereferencing the type *inside* the smart pointer.
///
/// This is quite obscure and mostly useful for situations in which we want
/// a wrapper for `Box<dyn Deref<Target = T>>` that dereferences to `T` rather
/// than dereferencing to `dyn Deref<Target = T>`.
///
/// This is used internally in [`MappedSignal`] and [`ArcMappedSignal`].
pub struct DoubleDeref<T> {
    inner: T,
}

impl<T> Deref for DoubleDeref<T>
where
    T: Deref,
    T::Target: Deref,
{
    type Target = <T::Target as Deref>::Target;

    fn deref(&self) -> &Self::Target {
        self.inner.deref().deref()
    }
}

impl<T> DerefMut for DoubleDeref<T>
where
    T: DerefMut,
    T::Target: DerefMut,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.deref_mut().deref_mut()
    }
}

impl<T> UntrackableGuard for DoubleDeref<T>
where
    T: UntrackableGuard,
    T::Target: DerefMut,
{
    fn untrack(&mut self) {
        self.inner.untrack();
    }
}

/// A derived signal type that wraps an [`RwSignal`] with a mapping function,
///  allowing you to read or write directly to one of its field.
///
/// Tracking the mapped signal tracks changes to *any* part of the signal, and updating the signal notifies
/// and notifies *all* dependencies of the signal. This is not a mechanism for fine-grained reactive updates
/// to more complex data structures. Instead, it allows you to provide a signal-like API for wrapped types
/// without exposing the original type directly to users.
pub struct MappedSignal<T, S = SyncStorage> {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
    inner: StoredValue<ArcMappedSignal<T>, S>,
}
crate::impl_weak!([T, S] MappedSignal<T, S>);

impl<T, S> MappedSignal<T, S> {
    /// A handle whose value is gone (what an accessor of a gone handle returns).
    #[track_caller]
    pub(crate) fn disposed() -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner: StoredValue::disposed(),
        }
    }
}

impl<T> MappedSignal<T> {
    /// Wraps a signal with the given mapping functions for shared and exclusive references.
    #[track_caller]
    pub fn new<U>(
        inner: RwSignal<U>,
        map: fn(&U) -> &T,
        map_mut: fn(&mut U) -> &mut T,
    ) -> Self
    where
        T: Send + Sync + 'static,
        U: Send + Sync + 'static,
    {
        // a signal mapped from a gone one is gone too
        let Some(this) = inner.upgrade() else {
            return Self::disposed();
        };
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner: {
                StoredValue::new_with_storage(ArcMappedSignal::new(
                    this, map, map_mut,
                ))
            },
        }
    }
}

impl<T> Copy for MappedSignal<T> {}

impl<T> Clone for MappedSignal<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Debug for MappedSignal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut partial = f.debug_struct("MappedSignal");
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        partial.field("defined_at", &self.defined_at);
        partial.finish()
    }
}

impl<T> DefinedAt for MappedSignal<T> {
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

impl<T> Notify for MappedSignal<T>
where
    T: 'static,
{
    fn notify(&self) {
        if let Some(inner) = self.inner.try_get_value() {
            inner.notify();
        }
    }
}

impl<T> Track for MappedSignal<T>
where
    T: 'static,
{
    fn track(&self) {
        if let Some(inner) = self.inner.try_get_value() {
            inner.track();
        }
    }
}

impl<T> TryReadUntracked for MappedSignal<T>
where
    T: 'static,
{
    type Value = DoubleDeref<Box<dyn Deref<Target = T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        self.inner
            .try_get_value()
            .and_then(|inner| inner.try_read_untracked())
    }
}

impl<T> Write for MappedSignal<T>
where
    T: 'static,
{
    type Value = T;

    /// A guard over a copy of the mapped part, swapped into the signal's value when it is
    /// dropped.
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        T: Clone,
    {
        self.inner.try_get_value()?.copy_guard()
    }

    fn try_commit_value(&self, value: T, notify: bool) -> Option<T> {
        match self.inner.try_get_value() {
            Some(inner) => inner.try_commit_value(value, notify),
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
}

impl<T> MappedSignal<T>
where
    T: 'static,
{
    /// The reference-counted form, unless the owner is gone.
    pub(crate) fn try_to_arc(&self) -> Option<ArcMappedSignal<T>> {
        self.inner.try_get_value()
    }
}

impl<T> From<ArcMappedSignal<T>> for MappedSignal<T>
where
    T: 'static,
{
    #[track_caller]
    fn from(value: ArcMappedSignal<T>) -> Self {
        MappedSignal {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner: StoredValue::new(value),
        }
    }
}

impl<T> IsDisposed for MappedSignal<T> {
    fn is_disposed(&self) -> bool {
        self.inner.is_disposed()
    }
}
