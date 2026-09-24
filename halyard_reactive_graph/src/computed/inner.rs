use crate::or_poisoned::OrPoisoned;
use crate::{
    error::{GraphError, ReportOnce},
    graph::{
        AnySource, AnySubscriber, Observer, ReactiveNode, ReactiveNodeState,
        Source, SourceSet, Subscriber, SubscriberSet, WithObserver,
    },
    owner::{Owner, Storage, StorageAccess},
    reentry::{
        computing_here, held_by_this_thread, lock_id, Held, SINGLE_THREADED,
    },
    signal::guards::Plain,
};
use std::{
    fmt::Debug,
    mem,
    panic::Location,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock, RwLockWriteGuard, TryLockError,
    },
};

/// A memo read under a guard on its own value after its sources changed (logged once).
static MEMO_BORROWED: ReportOnce = ReportOnce::new();

/// A memo's function that receives the previous value by reference: gives the new value
/// (`None` for a memo made with `Memo::new_try` whose sources are gone) and whether it changed.
type BorrowingFn<T> = dyn Fn(Option<&T>) -> (Option<T>, bool) + Send + Sync;

/// A memo's function that receives the previous value by value.
type OwningFn<T> = dyn Fn(Option<T>) -> (Option<T>, bool) + Send + Sync;

/// How a memo's function receives the previous value.
pub(crate) enum MemoFn<T> {
    /// By reference (`new`, `new_with_compare`, `new_try`): the previous value stays the
    /// memo's value while the function runs, so a read of the memo from inside it (a
    /// cycle) gives the previous value.
    Borrowing(Arc<BorrowingFn<T>>),
    /// By value (`new_owning`): the memo has no value while the function runs.
    Owning(Arc<OwningFn<T>>),
}

pub struct MemoInner<T, S>
where
    S: Storage<T>,
{
    /// Must always be acquired *after* the reactivity lock
    pub(crate) value: Arc<RwLock<Option<S::Wrapped>>>,
    pub(crate) fun: MemoFn<T>,
    pub(crate) owner: Owner,
    pub(crate) reactivity: RwLock<MemoInnerReactivity>,
    pub(crate) defined_at: Option<&'static Location<'static>>,
    /// Made by `Memo::new_try`: the function may give no value, so there is no strong form.
    fallible: AtomicBool,
}

pub(crate) struct MemoInnerReactivity {
    pub(crate) state: ReactiveNodeState,
    pub(crate) sources: SourceSet,
    pub(crate) subscribers: SubscriberSet,
    pub(crate) any_subscriber: AnySubscriber,
}

impl<T, S> Debug for MemoInner<T, S>
where
    S: Storage<T>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoInner").finish_non_exhaustive()
    }
}

impl<T: 'static, S> MemoInner<T, S>
where
    S: Storage<T>,
{
    pub(crate) fn new(
        fun: MemoFn<T>,
        any_subscriber: AnySubscriber,
        defined_at: Option<&'static Location<'static>>,
    ) -> Self {
        Self {
            value: Arc::new(RwLock::new(None)),
            fun,
            owner: Owner::new(),
            reactivity: RwLock::new(MemoInnerReactivity {
                state: ReactiveNodeState::Dirty,
                sources: Default::default(),
                subscribers: SubscriberSet::new(),
                any_subscriber,
            }),
            defined_at,
            fallible: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_fallible(&self) {
        self.fallible.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_fallible(&self) -> bool {
        self.fallible.load(Ordering::Relaxed)
    }

    /// The memo's identity in the graph (as a source and as a subscriber): its address.
    fn address(&self) -> usize {
        (self as *const Self).cast::<()>() as usize
    }
}

impl<T: 'static, S> ReactiveNode for MemoInner<T, S>
where
    S: Storage<T>,
{
    fn mark_dirty(&self) {
        let subs = {
            let mut lock = self.reactivity.write().or_poisoned();
            lock.state = ReactiveNodeState::Dirty;
            lock.subscribers.clone()
        };

        for sub in subs {
            sub.mark_check();
        }
    }

    fn mark_check(&self) {
        /// codegen optimisation:
        fn inner(reactivity: &RwLock<MemoInnerReactivity>) {
            let subs = {
                let mut lock = reactivity.write().or_poisoned();
                if lock.state != ReactiveNodeState::Dirty {
                    lock.state = ReactiveNodeState::Check;
                }
                lock.subscribers.clone()
            };

            for sub in subs {
                sub.mark_check();
            }
        }
        inner(&self.reactivity);
    }

    fn mark_subscribers_check(&self) {
        let subs = self.reactivity.read().or_poisoned().subscribers.clone();
        for sub in subs {
            sub.mark_check();
        }
    }

    fn update_if_necessary(&self) -> bool {
        /// codegen optimisation:
        fn needs_update(reactivity: &RwLock<MemoInnerReactivity>) -> bool {
            let (state, sources) = {
                let inner = reactivity.read().or_poisoned();
                (inner.state, inner.sources.clone())
            };
            match state {
                ReactiveNodeState::Clean => false,
                ReactiveNodeState::Dirty => true,
                ReactiveNodeState::Check => {
                    (&sources).into_iter().any(|source| {
                        source.update_if_necessary()
                            || reactivity.read().or_poisoned().state
                                == ReactiveNodeState::Dirty
                    })
                }
            }
        }

        if needs_update(&self.reactivity) {
            let id = lock_id(&*self.value);
            // This thread is using the value: computing it (the memo is read inside its own
            // function, a cycle: the read gives the previous value, and reports that), or
            // holding a guard on it (the memo is read inside its own `with`, or while its
            // `read` guard lives), which would make the write below wait forever. The memo
            // keeps its previous value, and stays dirty so that it recomputes on the next
            // read.
            if held_by_this_thread(id) {
                if !computing_here(id) {
                    MEMO_BORROWED.report(|| GraphError::MemoBorrowed {
                        defined_at: self.defined_at,
                    });
                }
                return false;
            }

            /// codegen optimisation:
            fn inner_1(
                reactivity: &RwLock<MemoInnerReactivity>,
            ) -> AnySubscriber {
                let any_subscriber =
                    reactivity.read().or_poisoned().any_subscriber.clone();
                any_subscriber.clear_sources(&any_subscriber);
                any_subscriber
            }

            let (new_value, changed) = match &self.fun {
                MemoFn::Borrowing(fun) => {
                    // A read guard on the previous value, taken before the computation is
                    // recorded (so that it waits for another thread's write, and for nothing
                    // of this thread's): reads of the memo from inside its function share it.
                    let Some(previous) = Plain::try_new_at(
                        Arc::clone(&self.value),
                        self.defined_at,
                    ) else {
                        return false;
                    };
                    let _computing = Held::compute(id);
                    let any_subscriber = inner_1(&self.reactivity);
                    self.owner.with_cleanup(|| {
                        any_subscriber.with_observer(|| {
                            fun(previous
                                .as_ref()
                                .map(StorageAccess::as_borrowed))
                        })
                    })
                }
                MemoFn::Owning(fun) => {
                    // No deadlock risk, because we only hold the value lock.
                    let value = self.value.write().or_poisoned().take();
                    let _computing = Held::compute(id);
                    let any_subscriber = inner_1(&self.reactivity);
                    self.owner.with_cleanup(|| {
                        any_subscriber.with_observer(|| {
                            fun(value.map(StorageAccess::into_taken))
                        })
                    })
                }
            };

            // Two locks are acquired, so order matters.
            let reactivity_lock = self.reactivity.write().or_poisoned();
            // A guard on the value that the function kept alive (stored somewhere) would make
            // the write wait forever: the memo keeps its previous value and stays dirty.
            if held_by_this_thread(id) {
                drop(reactivity_lock);
                MEMO_BORROWED.report(|| GraphError::MemoBorrowed {
                    defined_at: self.defined_at,
                });
                return false;
            }
            let value_lock = if SINGLE_THREADED {
                match self.value.try_write() {
                    Ok(guard) => Some(guard),
                    Err(TryLockError::Poisoned(poisoned)) => {
                        Some(poisoned.into_inner())
                    }
                    Err(TryLockError::WouldBlock) => None,
                }
            } else {
                // waits while other threads read the value
                Some(self.value.write().or_poisoned())
            };
            let Some(mut value_lock) = value_lock else {
                drop(reactivity_lock);
                return false;
            };
            // `None` from a memo made with `Memo::new_try`: a source is gone, and so is the
            // memo's value until a source changes
            let previous =
                mem::replace(&mut *value_lock, new_value.map(S::wrap));
            drop(value_lock);

            /// codegen optimisation:
            fn inner_2(
                changed: bool,
                mut reactivity_lock: RwLockWriteGuard<'_, MemoInnerReactivity>,
            ) {
                reactivity_lock.state = ReactiveNodeState::Clean;

                if changed {
                    let subs = reactivity_lock.subscribers.clone();
                    drop(reactivity_lock);
                    for sub in subs {
                        // don't trigger reruns of effects/memos
                        // basically: if one of the observers has triggered this memo to
                        // run, it doesn't need to be re-triggered because of this change
                        if !Observer::is(&sub) {
                            sub.mark_dirty();
                        }
                    }
                } else {
                    drop(reactivity_lock);
                }
            }
            inner_2(changed, reactivity_lock);
            // the previous value, dropped once no lock is held
            drop(previous);

            changed
        } else {
            /// codegen optimisation:
            fn inner(reactivity: &RwLock<MemoInnerReactivity>) -> bool {
                let mut lock = reactivity.write().or_poisoned();
                lock.state = ReactiveNodeState::Clean;
                false
            }
            inner(&self.reactivity)
        }
    }
}

impl<T: 'static, S> Source for MemoInner<T, S>
where
    S: Storage<T>,
{
    fn add_subscriber(&self, subscriber: AnySubscriber) {
        // A memo read inside its own computation (a cycle) does not subscribe to itself: it
        // would be marked again by every change it is marked for, without end.
        if subscriber.0 == self.address() {
            return;
        }
        let mut lock = self.reactivity.write().or_poisoned();
        lock.subscribers.subscribe(subscriber);
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.reactivity
            .write()
            .or_poisoned()
            .subscribers
            .unsubscribe(subscriber);
    }

    fn clear_subscribers(&self) {
        self.reactivity.write().or_poisoned().subscribers.take();
    }
}

impl<T: 'static, S> Subscriber for MemoInner<T, S>
where
    S: Storage<T>,
{
    fn add_source(&self, source: AnySource) {
        // not itself (see `add_subscriber`)
        if source.0 == self.address() {
            return;
        }
        self.reactivity.write().or_poisoned().sources.insert(source);
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        self.reactivity
            .write()
            .or_poisoned()
            .sources
            .clear_sources(subscriber);
    }
}
