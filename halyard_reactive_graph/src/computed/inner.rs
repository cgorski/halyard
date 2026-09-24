use crate::or_poisoned::OrPoisoned;
use crate::{
    error::{GraphError, ReportOnce},
    graph::{
        AnySource, AnySubscriber, Observer, ReactiveNode, ReactiveNodeState,
        Source, SourceSet, Subscriber, SubscriberSet, WithObserver,
    },
    owner::{Owner, Storage, StorageAccess},
    reentry::{held_by_this_thread, lock_id},
};
use std::{
    fmt::Debug,
    panic::Location,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock, RwLockWriteGuard,
    },
};

/// A memo read under a guard on its own value after its sources changed (logged once).
static MEMO_BORROWED: ReportOnce = ReportOnce::new();

pub struct MemoInner<T, S>
where
    S: Storage<T>,
{
    /// Must always be acquired *after* the reactivity lock
    pub(crate) value: Arc<RwLock<Option<S::Wrapped>>>,
    #[allow(clippy::type_complexity)]
    pub(crate) fun: Arc<dyn Fn(Option<T>) -> (Option<T>, bool) + Send + Sync>,
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
    #[allow(clippy::type_complexity)]
    pub fn new(
        fun: Arc<dyn Fn(Option<T>) -> (Option<T>, bool) + Send + Sync>,
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
            // A guard on the value alive on this thread (the memo is read inside its own
            // `with`, or while its `read` guard lives) would make the writes below wait
            // forever. The memo keeps its previous value, and stays dirty so that it
            // recomputes on the next read.
            if held_by_this_thread(lock_id(&*self.value)) {
                MEMO_BORROWED.report(|| GraphError::MemoBorrowed {
                    defined_at: self.defined_at,
                });
                return false;
            }

            // No deadlock risk, because we only hold the value lock.
            let value = self.value.write().or_poisoned().take();

            /// codegen optimisation:
            fn inner_1(
                reactivity: &RwLock<MemoInnerReactivity>,
            ) -> AnySubscriber {
                let any_subscriber =
                    reactivity.read().or_poisoned().any_subscriber.clone();
                any_subscriber.clear_sources(&any_subscriber);
                any_subscriber
            }
            let any_subscriber = inner_1(&self.reactivity);

            let (new_value, changed) = self.owner.with_cleanup(|| {
                any_subscriber.with_observer(|| {
                    (self.fun)(value.map(StorageAccess::into_taken))
                })
            });

            // Two locks are acquired, so order matters.
            let reactivity_lock = self.reactivity.write().or_poisoned();
            {
                // Safety: Can block endlessly if the user is has a ReadGuard on the value
                let mut value_lock = self.value.write().or_poisoned();
                // `None` from a memo made with `Memo::new_try`: a source is gone, and so
                // is the memo's value until a source changes
                *value_lock = new_value.map(S::wrap);
            }

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
