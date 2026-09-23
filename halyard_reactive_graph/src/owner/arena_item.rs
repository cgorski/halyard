use super::{
    arena::{Arena, ArenaEntry, NodeId},
    LocalStorage, Storage, SyncStorage, OWNER,
};
use crate::{
    error::{GraphError, ReportOnce},
    traits::{Dispose, IntoInner, IsDisposed},
};
use send_wrapper::SendWrapper;
use std::{hash::Hash, marker::PhantomData, panic::Location, sync::Arc};

/// A reactive value created where no arena is active (with `sandboxed-arenas`).
static NO_ARENA: ReportOnce = ReportOnce::new();

/// A copyable, stable reference for any value, stored on the arena whose ownership is managed by the
/// reactive ownership tree.
#[derive(Debug)]
pub struct ArenaItem<T, S = SyncStorage> {
    node: NodeId,
    #[allow(clippy::type_complexity)]
    ty: PhantomData<fn() -> (SendWrapper<T>, S)>,
}

impl<T, S> Copy for ArenaItem<T, S> {}

impl<T, S> Clone for ArenaItem<T, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, S> PartialEq for ArenaItem<T, S> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T, S> Eq for ArenaItem<T, S> {}

impl<T, S> Hash for ArenaItem<T, S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.node.hash(state);
    }
}

impl<T, S> ArenaItem<T, S>
where
    T: 'static,
    S: Storage<T>,
{
    /// Stores the given value in the arena allocator.
    ///
    /// With `sandboxed-arenas`, on a thread where no arena is active (no owner was set),
    /// there is nowhere to store it: the value is dropped, the item is created disposed, and
    /// that is logged once.
    #[track_caller]
    pub fn new_with_storage(value: T) -> Self {
        let at = Location::caller();
        let entry: ArenaEntry = Arc::new(S::wrap(value));
        let Some(node) = Arena::try_with_mut(|arena| arena.insert(entry))
        else {
            NO_ARENA.report(|| GraphError::NoArena {
                at,
                instead: "the value is dropped and the reactive value is \
                          created disposed",
            });
            return Self::disposed();
        };
        OWNER.with(|o| {
            if let Some(owner) = o.borrow().as_ref().and_then(|o| o.upgrade()) {
                owner.register(node);
            }
        });

        Self {
            node,
            ty: PhantomData,
        }
    }
}

impl<T, S> ArenaItem<T, S> {
    /// An item whose value is gone: for a handle derived from a disposed one.
    pub(crate) fn disposed() -> Self {
        Self {
            node: NodeId::default(),
            ty: PhantomData,
        }
    }
}

impl<T, S> Default for ArenaItem<T, S>
where
    T: Default + 'static,
    S: Storage<T>,
{
    #[track_caller] // Default trait is not annotated with #[track_caller]
    fn default() -> Self {
        Self::new_with_storage(Default::default())
    }
}

impl<T> ArenaItem<T>
where
    T: Send + Sync + 'static,
{
    /// Stores the given value in the arena allocator.
    #[track_caller]
    pub fn new(value: T) -> Self {
        ArenaItem::new_with_storage(value)
    }
}

impl<T> ArenaItem<T, LocalStorage>
where
    T: 'static,
{
    /// Stores the given value in the arena allocator.
    #[track_caller]
    pub fn new_local(value: T) -> Self {
        ArenaItem::new_with_storage(value)
    }
}

impl<T, S: Storage<T>> ArenaItem<T, S> {
    /// Applies a function to a reference to the stored value and returns the result, or `None` if it has already been disposed.
    ///
    /// The arena is not locked while `fun` runs, so `fun` may use any reactive value,
    /// including this one.
    #[track_caller]
    pub fn try_with_value<U>(&self, fun: impl FnOnce(&T) -> U) -> Option<U> {
        S::try_with(self.node, fun)
    }

    /// Applies a function to a mutable reference to the stored value and returns the result, or `None` if it has already been disposed.
    ///
    /// Also `None` while the value is being used elsewhere (through
    /// [`ArenaItem::try_with_value`] on another thread). `fun` runs while the arena is
    /// locked: it must not use reactive values.
    #[track_caller]
    pub fn try_update_value<U>(
        &self,
        fun: impl FnOnce(&mut T) -> U,
    ) -> Option<U> {
        S::try_with_mut(self.node, fun)
    }

    /// Replaces the stored value; the previous one is dropped once nothing is using it.
    /// Returns the value back if the item has been disposed.
    pub(crate) fn replace_value(&self, value: T) -> Option<T> {
        S::try_set(self.node, value)
    }
}

impl<T: Clone, S: Storage<T>> ArenaItem<T, S> {
    /// Returns a clone of the stored value, or `None` if it has already been disposed.
    #[track_caller]
    pub fn try_get_value(&self) -> Option<T> {
        S::try_with(self.node, Clone::clone)
    }
}

impl<T, S> IsDisposed for ArenaItem<T, S> {
    fn is_disposed(&self) -> bool {
        Arena::try_with(|arena| !arena.contains_key(self.node)).unwrap_or(true)
    }
}

impl<T, S> Dispose for ArenaItem<T, S> {
    fn dispose(self) {
        // dropped once the arena's lock is released: its `Drop` may use the graph
        let removed = Arena::try_with_mut(|arena| arena.remove(self.node));
        drop(removed);
    }
}

impl<T, S: Storage<T>> IntoInner for ArenaItem<T, S> {
    type Value = T;

    #[inline(always)]
    fn into_inner(self) -> Option<Self::Value> {
        S::take(self.node)
    }
}
