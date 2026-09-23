use super::arena::{Arena, NodeId};
use crate::error::{GraphError, ReportOnce};
use send_wrapper::SendWrapper;
use std::{mem, sync::Arc};

/// A trait for borrowing and taking data.
pub trait StorageAccess<T> {
    /// Borrows the value.
    fn as_borrowed(&self) -> &T;

    /// Takes the value.
    fn into_taken(self) -> T;
}

impl<T> StorageAccess<T> for T {
    fn as_borrowed(&self) -> &T {
        self
    }

    fn into_taken(self) -> T {
        self
    }
}

impl<T> StorageAccess<T> for SendWrapper<T> {
    fn as_borrowed(&self) -> &T {
        self
    }

    fn into_taken(self) -> T {
        self.take()
    }
}

/// A way of storing an [`ArenaItem`](super::arena_item::ArenaItem), either as itself or with a wrapper to make it threadsafe.
///
/// This exists because all items stored in the arena must be `Send + Sync`, but in single-threaded
/// environments you might want or need to use thread-unsafe types.
pub trait Storage<T>: Send + Sync + 'static {
    /// The type being stored, once it has been wrapped.
    type Wrapped: StorageAccess<T> + Send + Sync + 'static;

    /// Adds any needed wrapper to the type.
    fn wrap(value: T) -> Self::Wrapped;

    /// Applies the given function to the stored value, if it exists and can be accessed from this
    /// thread.
    ///
    /// The arena is not locked while `fun` runs, so `fun` may use any reactive value,
    /// including this one.
    fn try_with<U>(node: NodeId, fun: impl FnOnce(&T) -> U) -> Option<U>;

    /// Applies the given function to a mutable reference to the stored value, if it exists and can be accessed from this
    /// thread.
    ///
    /// A mutable borrow needs the only reference to the value, so this returns `None` while
    /// the value is being used elsewhere (through [`Storage::try_with`] on another thread).
    /// `fun` runs while the arena is locked: it must not use reactive values.
    fn try_with_mut<U>(
        node: NodeId,
        fun: impl FnOnce(&mut T) -> U,
    ) -> Option<U>;

    /// Sets a new value for the stored value. If it has been disposed, returns `Some(T)`.
    ///
    /// The previous value is dropped after the arena's lock is released.
    fn try_set(node: NodeId, value: T) -> Option<T>;

    /// Takes an item from the arena if it exists and can be accessed from this thread.
    /// If it cannot be casted, it will still be removed from the arena.
    ///
    /// If the value is being used elsewhere at this moment (through [`Storage::try_with`] on
    /// another thread), it is removed but not taken: it is dropped when that use ends.
    fn take(node: NodeId) -> Option<T>;
}

/// A form of [`Storage`] that stores the type as itself, with no wrapper.
#[derive(Debug, Copy, Clone)]
pub struct SyncStorage;

impl<T> Storage<T> for SyncStorage
where
    T: Send + Sync + 'static,
{
    type Wrapped = T;

    #[inline(always)]
    fn wrap(value: T) -> Self::Wrapped {
        value
    }

    fn try_with<U>(node: NodeId, fun: impl FnOnce(&T) -> U) -> Option<U> {
        // the clone of the entry keeps the value alive after the arena's lock is released
        let entry = Arena::get(node)?;
        entry.downcast_ref::<T>().map(fun)
    }

    fn try_with_mut<U>(
        node: NodeId,
        fun: impl FnOnce(&mut T) -> U,
    ) -> Option<U> {
        Arena::try_with_mut(|arena| {
            arena
                .get_mut(node)
                .and_then(Arc::get_mut)
                .and_then(|entry| entry.downcast_mut::<T>())
                .map(fun)
        })
        .flatten()
    }

    fn try_set(node: NodeId, value: T) -> Option<T> {
        let mut value = Some(value);
        let replaced = Arena::try_with_mut(|arena| {
            let entry = arena.get_mut(node).filter(|entry| entry.is::<T>())?;
            let value = value.take()?;
            Some(mem::replace(entry, Arc::new(value)))
        });
        drop(replaced);
        value
    }

    fn take(node: NodeId) -> Option<T> {
        let entry =
            Arena::try_with_mut(|arena| arena.remove(node)).flatten()?;
        Arc::downcast::<T>(entry)
            .ok()
            .and_then(|value| Arc::try_unwrap(value).ok())
    }
}

/// A form of [`Storage`] that stores the type with a wrapper that makes it `Send + Sync`, but only
/// allows it to be accessed from the thread on which it was created.
#[derive(Debug, Copy, Clone)]
pub struct LocalStorage;

/// A local value reached from a thread other than the one that created it.
static WRONG_THREAD: ReportOnce = ReportOnce::new();

fn report_wrong_thread(instead: &'static str) {
    WRONG_THREAD.report(|| GraphError::WrongThread { instead });
}

/// Whether the entry holds a `SendWrapper<T>` that can be used (and dropped) on this thread.
fn is_local_here<T: 'static>(entry: &super::arena::ArenaEntry) -> bool {
    entry
        .downcast_ref::<SendWrapper<T>>()
        .is_some_and(SendWrapper::valid)
}

impl<T> Storage<T> for LocalStorage
where
    T: 'static,
{
    type Wrapped = SendWrapper<T>;

    fn wrap(value: T) -> Self::Wrapped {
        SendWrapper::new(value)
    }

    fn try_with<U>(node: NodeId, fun: impl FnOnce(&T) -> U) -> Option<U> {
        // cloned only on the value's own thread, so that the clone, if it ends up being the
        // last reference, is dropped there
        let found = Arena::try_with(|arena| {
            arena.get(node).map(|entry| {
                is_local_here::<T>(entry).then(|| Arc::clone(entry))
            })
        })
        .flatten()?;
        let Some(entry) = found else {
            report_wrong_thread("nothing is read");
            return None;
        };
        entry
            .downcast_ref::<SendWrapper<T>>()
            .map(|inner| fun(inner))
    }

    fn try_with_mut<U>(
        node: NodeId,
        fun: impl FnOnce(&mut T) -> U,
    ) -> Option<U> {
        Arena::try_with_mut(|arena| {
            arena
                .get_mut(node)
                .filter(|entry| is_local_here::<T>(entry))
                .and_then(Arc::get_mut)
                .and_then(|entry| entry.downcast_mut::<SendWrapper<T>>())
                .map(|inner| fun(&mut *inner))
        })
        .flatten()
    }

    fn try_set(node: NodeId, value: T) -> Option<T> {
        let mut value = Some(value);
        let replaced = Arena::try_with_mut(|arena| {
            let entry = arena
                .get_mut(node)
                .filter(|entry| is_local_here::<T>(entry))?;
            let value = value.take()?;
            Some(mem::replace(entry, Arc::new(SendWrapper::new(value))))
        });
        drop(replaced);
        value
    }

    fn take(node: NodeId) -> Option<T> {
        let entry =
            Arena::try_with_mut(|arena| arena.remove(node)).flatten()?;
        let wrapper = Arc::downcast::<SendWrapper<T>>(entry).ok()?;
        if !wrapper.valid() {
            // dropping another thread's local value panics; it is leaked instead
            report_wrong_thread("it is removed and leaked, not dropped");
            mem::forget(wrapper);
            return None;
        }
        Arc::try_unwrap(wrapper).ok().map(SendWrapper::take)
    }
}
