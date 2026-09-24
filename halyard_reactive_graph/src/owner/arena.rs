use crate::or_poisoned::OrPoisoned;
use slotmap::{new_key_type, SlotMap};
#[cfg(feature = "sandboxed-arenas")]
use std::cell::RefCell;
#[cfg(not(feature = "sandboxed-arenas"))]
use std::sync::OnceLock;
#[cfg(feature = "sandboxed-arenas")]
use std::sync::Weak;
use std::{
    any::Any,
    hash::Hash,
    sync::{Arc, RwLock},
};

new_key_type! {
    /// Unique identifier for an item stored in the arena.
    pub struct NodeId;
}

pub struct Arena;

/// An arena entry. It is reference-counted so that a value can be used through a clone of
/// its entry after the arena's lock is released: no application code (a stored closure, a
/// setter, a value's `Drop`) ever runs while the arena is locked, so none can re-enter it
/// (docs/no-panics.md, "Structural changes" 2).
pub type ArenaEntry = Arc<dyn Any + Send + Sync>;

pub type ArenaMap = SlotMap<NodeId, ArenaEntry>;

#[cfg(not(feature = "sandboxed-arenas"))]
static MAP: OnceLock<RwLock<ArenaMap>> = OnceLock::new();
#[cfg(feature = "sandboxed-arenas")]
thread_local! {
    pub(crate) static MAP: RefCell<Option<Weak<RwLock<ArenaMap>>>> = RefCell::new(Some(Default::default()));
}

impl Arena {
    #[inline(always)]
    #[allow(unused)]
    pub fn set(arena: &Arc<RwLock<ArenaMap>>) {
        #[cfg(feature = "sandboxed-arenas")]
        {
            let new_arena = Arc::downgrade(arena);
            MAP.with_borrow_mut(|arena| {
                *arena = Some(new_arena);
            })
        }
    }

    /// Runs `fun` on the arena, locked for reading; `None` if no arena is active (with
    /// `sandboxed-arenas`, on a thread that has not set an owner). `fun` must only look
    /// entries up: application code must never run under this lock.
    #[track_caller]
    pub fn try_with<U>(fun: impl FnOnce(&ArenaMap) -> U) -> Option<U> {
        #[cfg(not(feature = "sandboxed-arenas"))]
        {
            Some(fun(&MAP.get_or_init(Default::default).read().or_poisoned()))
        }
        #[cfg(feature = "sandboxed-arenas")]
        {
            MAP.with_borrow(|arena| {
                arena
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .map(|n| fun(&n.read().or_poisoned()))
            })
        }
    }

    /// A clone of the entry for `node`, if it exists (and an arena is active).
    #[track_caller]
    pub fn get(node: NodeId) -> Option<ArenaEntry> {
        Arena::try_with(|arena| arena.get(node).cloned()).flatten()
    }

    /// Runs `fun` on the arena, locked for writing; `None` if no arena is active. `fun` must
    /// only insert, remove or replace entries, and hand removed entries back to be dropped
    /// once the lock is released (a value's `Drop` may use the graph).
    #[track_caller]
    pub fn try_with_mut<U>(fun: impl FnOnce(&mut ArenaMap) -> U) -> Option<U> {
        #[cfg(not(feature = "sandboxed-arenas"))]
        {
            Some(fun(&mut MAP
                .get_or_init(Default::default)
                .write()
                .or_poisoned()))
        }
        #[cfg(feature = "sandboxed-arenas")]
        {
            MAP.with_borrow(|arena| {
                arena
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .map(|n| fun(&mut n.write().or_poisoned()))
            })
        }
    }
}

/// Removes `nodes` from `arena`, and drops their values once its lock is released: a value's
/// `Drop` (a stored closure's captures, an owner) may use the graph.
pub(crate) fn remove_nodes(arena: &RwLock<ArenaMap>, nodes: Vec<NodeId>) {
    let removed = {
        let mut arena = arena.write().or_poisoned();
        nodes
            .into_iter()
            .filter_map(|node| arena.remove(node))
            .collect::<Vec<_>>()
    };
    drop(removed);
}

/// Removes `nodes` from the active arena, as [`remove_nodes`] does.
#[cfg(not(feature = "sandboxed-arenas"))]
pub(crate) fn remove_from_active_arena(nodes: Vec<NodeId>) {
    remove_nodes(MAP.get_or_init(Default::default), nodes);
}

#[cfg(feature = "sandboxed-arenas")]
pub mod sandboxed {
    use super::{Arena, ArenaMap, MAP};
    use futures::Stream;
    use pin_project_lite::pin_project;
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, RwLock, Weak},
        task::{Context, Poll},
    };

    pin_project! {
        /// A [`Future`] that restores its associated arena as the current arena whenever it is
        /// polled.
        ///
        /// Sandboxed arenas are used to ensure that data created in response to e.g., different
        /// HTTP requests can be handled separately, while providing stable identifiers for their
        /// stored values. Wrapping a `Future` in `Sandboxed` ensures that it will always use the
        /// same arena that it was created under.
        pub struct Sandboxed<T> {
            arena: Option<Arc<RwLock<ArenaMap>>>,
            #[pin]
            inner: T,
        }
    }

    impl<T> Sandboxed<T> {
        /// Wraps the given [`Future`], ensuring that any [`ArenaItem`][item] created while it is
        /// being polled will be associated with the same arena that was active when this was
        /// called.
        ///
        /// [item]:[crate::owner::ArenaItem]
        #[track_caller]
        pub fn new(inner: T) -> Self {
            let arena = MAP.with_borrow(|n| n.as_ref().and_then(Weak::upgrade));
            Self { arena, inner }
        }
    }

    impl<Fut> Future for Sandboxed<Fut>
    where
        Fut: Future,
    {
        type Output = Fut::Output;

        fn poll(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Self::Output> {
            if let Some(arena) = self.arena.as_ref() {
                Arena::set(arena);
            }
            let this = self.project();
            this.inner.poll(cx)
        }
    }

    impl<T> Stream for Sandboxed<T>
    where
        T: Stream,
    {
        type Item = T::Item;

        fn poll_next(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            if let Some(arena) = self.arena.as_ref() {
                Arena::set(arena);
            }
            let this = self.project();
            this.inner.poll_next(cx)
        }
    }
}
