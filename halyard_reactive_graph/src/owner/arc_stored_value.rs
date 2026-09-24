use crate::{
    error::Access,
    or_poisoned::OrPoisoned,
    reentry::{held_by_this_thread, lock_id, SINGLE_THREADED},
    signal::guards::{report_reentered, CopyWriteGuard, Plain, ReadGuard},
    traits::{DefinedAt, IntoInner, IsDisposed, TryReadValue, WriteValue},
};
use std::{
    fmt::{Debug, Formatter},
    hash::Hash,
    mem,
    panic::Location,
    sync::{Arc, PoisonError, RwLock, TryLockError},
};

/// A reference-counted getter for any value non-reactively.
///
/// This is a reference-counted value, which is `Clone` but not `Copy`.
/// For arena-allocated `Copy` values, use [`StoredValue`](super::StoredValue).
///
/// This allows you to create a stable reference for any value by storing it within
/// the reactive system. Unlike e.g. [`ArcRwSignal`](crate::signal::ArcRwSignal), it is not reactive;
/// accessing it does not cause effects to subscribe, and
/// updating it does not notify anything else.
///
/// Its value is never lent out for a change in place: `set_value` replaces it, and
/// `update_value` and the `write_value` guard change a copy (the value must be `Clone`),
/// which then replaces it. So a read of it is never refused while it is being changed.
pub struct ArcStoredValue<T> {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
    value: Arc<RwLock<T>>,
}
crate::impl_strong!([T] ArcStoredValue<T>);

impl<T> ArcStoredValue<T> {
    /// Returns a weak (arena) handle to the value: `Copy`, and it does not keep the value
    /// alive (like [`std::sync::Arc::downgrade`]). The reverse is `upgrade` on the weak
    /// handle.
    #[track_caller]
    pub fn downgrade(&self) -> crate::owner::StoredValue<T>
    where
        crate::owner::StoredValue<T>: From<Self>,
    {
        self.clone().into()
    }
}

impl<T> Clone for ArcStoredValue<T> {
    fn clone(&self) -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
            value: Arc::clone(&self.value),
        }
    }
}

impl<T> Debug for ArcStoredValue<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArcStoredValue")
            .field("type", &std::any::type_name::<T>())
            .field("value", &Arc::as_ptr(&self.value))
            .finish()
    }
}

impl<T: Default> Default for ArcStoredValue<T> {
    #[track_caller]
    fn default() -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            value: Arc::new(RwLock::new(T::default())),
        }
    }
}

impl<T> PartialEq for ArcStoredValue<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }
}

impl<T> Eq for ArcStoredValue<T> {}

impl<T> Hash for ArcStoredValue<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(&Arc::as_ptr(&self.value), state);
    }
}

impl<T> DefinedAt for ArcStoredValue<T> {
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

impl<T> ArcStoredValue<T> {
    /// Creates a new stored value, taking the initial value as its argument.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", skip_all)
    )]
    #[track_caller]
    pub fn new(value: T) -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            value: Arc::new(RwLock::new(value)),
        }
    }
}

impl<T> TryReadValue for ArcStoredValue<T>
where
    T: 'static,
{
    type Value = ReadGuard<T, Plain<T>>;

    fn try_read_value(&self) -> Option<ReadGuard<T, Plain<T>>> {
        Plain::try_new_at(Arc::clone(&self.value), self.defined_at())
            .map(ReadGuard::new)
    }
}

impl<T> WriteValue for ArcStoredValue<T>
where
    T: 'static,
{
    type Value = T;

    fn try_write_value(&self) -> Option<CopyWriteGuard<T>>
    where
        T: Clone,
    {
        let value = self.try_read_value().map(|value| (*value).clone())?;
        let this = self.clone();
        Some(CopyWriteGuard::new(value, move |value, _| {
            this.try_swap_value(value)
        }))
    }

    fn try_swap_value(&self, value: &mut T) -> bool {
        // no code but the swap runs under the lock, so this thread holds it only if it is
        // using the value (inside its `with_value`, or holding a read guard): refused
        if held_by_this_thread(lock_id(&*self.value)) {
            report_reentered(Access::Write, self.defined_at());
            return false;
        }
        let stored = if SINGLE_THREADED {
            match self.value.try_write() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => {
                    Some(poisoned.into_inner())
                }
                Err(TryLockError::WouldBlock) => None,
            }
        } else {
            // another thread is using it: wait
            Some(self.value.write().or_poisoned())
        };
        let Some(mut stored) = stored else {
            report_reentered(Access::Write, self.defined_at());
            return false;
        };
        mem::swap(&mut *stored, value);
        true
    }
}

impl<T> IsDisposed for ArcStoredValue<T> {
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T> IntoInner for ArcStoredValue<T> {
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
