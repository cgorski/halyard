use crate::or_poisoned::OrPoisoned;
use crate::{
    error::{GraphError, ReportOnce},
    graph::{AnySubscriber, ReactiveNode, ToAnySubscriber},
    owner::on_cleanup,
    traits::{DefinedAt, Dispose},
};
use indexmap::IndexSet;
use std::{
    panic::Location,
    sync::{Arc, Mutex, PoisonError, RwLock, TryLockError},
};

/// Effects run a certain chunk of code whenever the signals they depend on change.
///
/// The effect runs on creation and again as soon as any tracked signal changes.
///
/// NOTE: you probably want use [`Effect`](super::Effect) instead.
/// This is for the few cases where it's important to execute effects immediately and in order.
///
/// [ImmediateEffect]s stop running when dropped.
///
/// NOTE: since effects are executed immediately, they might recurse.
/// Under recursion or parallelism only the last run to start is tracked.
///
/// ## Example
///
/// ```
/// # use halyard_reactive_graph::computed::*;
/// # use halyard_reactive_graph::signal::*; let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// # use halyard_reactive_graph::prelude::*;
/// # use halyard_reactive_graph::effect::ImmediateEffect;
/// # use halyard_reactive_graph::owner::ArenaItem;
/// # let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// let a = RwSignal::new(0);
/// let b = RwSignal::new(0);
///
/// // ✅ use effects to interact between reactive state and the outside world
/// let _drop_guard = ImmediateEffect::new(move || {
///   // on the next “tick” prints "Value: 0" and subscribes to `a`
///   println!("Value: {}", a.try_get().unwrap());
/// });
///
/// // The effect runs immediately and subscribes to `a`, in the process it prints "Value: 0"
/// # assert_eq!(a.try_get(), Some(0));
/// a.set(1);
/// # assert_eq!(a.try_get(), Some(1));
/// // ✅ because it's subscribed to `a`, the effect reruns and prints "Value: 1"
/// ```
/// ## Notes
///
/// 1. **Scheduling**: Effects run immediately, as soon as any tracked signal changes.
/// 2. By default, effects do not run unless the `effects` feature is enabled. If you are using
///    this with a web framework, this generally means that effects **do not run on the server**.
///    and you can call browser-specific APIs within the effect function without causing issues.
///    If you need an effect to run on the server, use [`ImmediateEffect::new_isomorphic`].
#[derive(Debug, Clone)]
pub struct ImmediateEffect {
    inner: StoredEffect,
}

type StoredEffect = Option<Arc<RwLock<inner::EffectInner>>>;

impl Dispose for ImmediateEffect {
    fn dispose(self) {}
}

impl ImmediateEffect {
    /// Creates a new effect which runs immediately, then again as soon as any tracked signal changes.
    /// (Unless [batch] is used.)
    ///
    /// NOTE: this requires a `Fn` function because it might recurse.
    /// Use [Self::new_mut] to pass a `FnMut` function, which skips a recursive run.
    #[track_caller]
    #[must_use]
    pub fn new(fun: impl Fn() + Send + Sync + 'static) -> Self {
        if !cfg!(feature = "effects") {
            return Self { inner: None };
        }

        let inner = inner::EffectInner::new(fun);

        inner.update_if_necessary();

        Self { inner: Some(inner) }
    }
    /// Creates a new effect which runs immediately, then again as soon as any tracked signal changes.
    /// (Unless [batch] is used.)
    ///
    /// A `FnMut` cannot run twice at once: if the effect is triggered while its function is
    /// running (it wrote a signal it reads, or another thread triggered it at the same time),
    /// that run is skipped, and this is logged once. Also see [Self::new].
    #[track_caller]
    #[must_use]
    pub fn new_mut(fun: impl FnMut() + Send + Sync + 'static) -> Self {
        static RETRIGGERED: ReportOnce = ReportOnce::new();
        let defined_at = Location::caller();
        let fun = Mutex::new(fun);
        Self::new(move || match fun.try_lock() {
            Ok(mut fun) => fun(),
            // a run that panicked left the function as it was
            Err(TryLockError::Poisoned(poisoned)) => {
                PoisonError::into_inner(poisoned)()
            }
            Err(TryLockError::WouldBlock) => {
                RETRIGGERED.report(|| GraphError::EffectRetriggered {
                    defined_at: Some(defined_at),
                })
            }
        })
    }
    /// Creates a new effect which runs immediately, then again as soon as any tracked signal changes.
    /// (Unless [batch] is used.)
    ///
    /// NOTE: this requires a `Fn` function because it might recurse.
    /// Use [Self::new_mut_scoped] to pass a `FnMut` function, which skips a recursive run.
    /// NOTE: this effect is automatically cleaned up when the current owner is cleared or disposed.
    #[track_caller]
    pub fn new_scoped(fun: impl Fn() + Send + Sync + 'static) {
        let effect = Self::new(fun);

        on_cleanup(move || effect.dispose());
    }
    /// Creates a new effect which runs immediately, then again as soon as any tracked signal changes.
    /// (Unless [batch] is used.)
    ///
    /// NOTE: this effect is automatically cleaned up when the current owner is cleared or disposed.
    ///
    /// A run triggered while the function is running is skipped, as with [Self::new_mut].
    /// Also see [Self::new_scoped]
    #[track_caller]
    pub fn new_mut_scoped(fun: impl FnMut() + Send + Sync + 'static) {
        let effect = Self::new_mut(fun);

        on_cleanup(move || effect.dispose());
    }

    /// Creates a new effect which runs immediately, then again as soon as any tracked signal changes.
    ///
    /// This will run whether the `effects` feature is enabled or not.
    #[track_caller]
    #[must_use]
    pub fn new_isomorphic(fun: impl Fn() + Send + Sync + 'static) -> Self {
        let inner = inner::EffectInner::new(fun);

        inner.update_if_necessary();

        Self { inner: Some(inner) }
    }
}

impl ToAnySubscriber for ImmediateEffect {
    /// Without the `effects` feature (unless it was made with `new_isomorphic`) the effect
    /// does not run: this is then a subscriber that tracks nothing, and that is logged once.
    fn to_any_subscriber(&self) -> AnySubscriber {
        static NOT_RUNNING: ReportOnce = ReportOnce::new();
        match &self.inner {
            Some(inner) => inner.to_any_subscriber(),
            None => {
                NOT_RUNNING.report(|| GraphError::NotRunning {
                    what: "an ImmediateEffect",
                });
                AnySubscriber::inert()
            }
        }
    }
}

impl DefinedAt for ImmediateEffect {
    fn defined_at(&self) -> Option<&'static Location<'static>> {
        self.inner.as_ref()?.read().or_poisoned().defined_at()
    }
}

/// Defers any [ImmediateEffect]s from running until the end of the function.
///
/// NOTE: this affects only [ImmediateEffect]s, not other effects.
///
/// NOTE: this is rarely needed, but it is useful for example when multiple signals
/// need to be updated atomically (for example a double-bound signal tree).
///
/// A batch belongs to the thread that runs it: effects triggered on other threads meanwhile
/// run as usual, on their own threads.
pub fn batch<T>(f: impl FnOnce() -> T) -> T {
    struct ExecuteOnDrop;
    impl Drop for ExecuteOnDrop {
        fn drop(&mut self) {
            // only the outermost batch holds this, and only it takes the set it created;
            // the effects run after the set is released
            let effects = inner::BATCH
                .try_with(|batch| {
                    batch
                        .try_borrow_mut()
                        .ok()
                        .and_then(|mut batch| batch.take())
                })
                .ok()
                .flatten()
                .unwrap_or_default();
            // TODO: Should we skip the effects if it's panicking?
            for effect in effects {
                effect.update_if_necessary();
            }
        }
    }
    // Nested batching has no effect.
    let outermost = inner::BATCH
        .try_with(|batch| match batch.try_borrow_mut() {
            Ok(mut batch) if batch.is_none() => {
                *batch = Some(IndexSet::new());
                true
            }
            _ => false,
        })
        .unwrap_or(false);
    // made only by the outermost batch: dropping one runs the batched effects
    let execute_on_drop = if outermost { Some(ExecuteOnDrop) } else { None };
    let ret = f();
    drop(execute_on_drop);
    ret
}

mod inner {
    use crate::or_poisoned::OrPoisoned;
    use crate::{
        graph::{
            AnySource, AnySubscriber, ReactiveNode, ReactiveNodeState,
            SourceSet, Subscriber, ToAnySubscriber, WithObserver,
        },
        log_warning,
        owner::Owner,
        traits::DefinedAt,
    };
    use indexmap::IndexSet;
    use std::{
        cell::RefCell,
        panic::Location,
        sync::{Arc, RwLock, Weak},
        thread::{self, ThreadId},
    };

    thread_local! {
        /// The effects deferred by the [super::batch] running on this thread, if any. Only
        /// `batch` sets and takes it; the effects add themselves to it.
        pub(super) static BATCH: RefCell<Option<IndexSet<AnySubscriber>>> =
            const { RefCell::new(None) };
    }

    /// Whether a batch is running on this thread.
    fn batching() -> bool {
        BATCH
            .try_with(|batch| {
                batch
                    .try_borrow()
                    .map(|batch| batch.is_some())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Adds an effect to this thread's batch; `false` if no batch is running.
    fn add_to_batch(subscriber: AnySubscriber) -> bool {
        BATCH
            .try_with(|batch| match batch.try_borrow_mut() {
                Ok(mut batch) => match batch.as_mut() {
                    Some(effects) => {
                        effects.insert(subscriber);
                        true
                    }
                    None => false,
                },
                Err(_) => false,
            })
            .unwrap_or(false)
    }

    /// Handles subscription logic for effects.
    ///
    /// To handle parallelism and recursion we assign ordered (1..) ids to each run.
    /// We only keep the sources tracked by the run with the highest id (the last one).
    ///
    /// We do this by:
    /// - Clearing the sources before every run, so the last one clears anything before it.
    /// - We stop tracking sources after the last run has completed.
    ///   (A parent run will start before and end after a recursive child run.)
    /// - To handle parallelism with the last run, we only allow sources to be added by its thread.
    pub(super) struct EffectInner {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        defined_at: &'static Location<'static>,
        owner: Owner,
        state: ReactiveNodeState,
        /// The number of effect runs in this 'batch'.
        /// Cleared when no runs are *ongoing* anymore.
        /// Used to assign ordered ids to each run, and to know when we can clear these values.
        run_count_start: usize,
        /// The number of effect runs that have completed in the current 'batch'.
        /// Cleared when no runs are *ongoing* anymore.
        /// Used to know when we can clear these values.
        run_done_count: usize,
        /// Given ordered ids (1..), the run with the highest id that has completed in this 'batch'.
        /// Cleared when no runs are *ongoing* anymore.
        /// Used to know whether the current run is the latest one.
        run_done_max: usize,
        /// The [ThreadId] of the run with the highest id.
        /// Used to prevent over-subscribing during parallel execution with the last run.
        ///
        /// ```text
        /// Thread 1:
        /// -------------------------
        ///   ---   ---    =======
        ///
        /// Thread 2:
        /// -------------------------
        ///             -----------
        /// ```
        ///
        /// In the parallel example above, we can see why we need this.
        /// The last run is marked using `=`, but another run in the other thread might
        /// also be gathering sources. So we only allow the run from the correct [ThreadId] to push sources.
        last_run_thread_id: ThreadId,
        fun: Arc<dyn Fn() + Send + Sync>,
        sources: SourceSet,
        any_subscriber: AnySubscriber,
    }

    impl EffectInner {
        #[track_caller]
        pub fn new(
            fun: impl Fn() + Send + Sync + 'static,
        ) -> Arc<RwLock<EffectInner>> {
            let owner = Owner::new();
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            let defined_at = Location::caller();

            Arc::new_cyclic(|weak| {
                let any_subscriber = AnySubscriber(
                    weak.as_ptr() as usize,
                    Weak::clone(weak) as Weak<dyn Subscriber + Send + Sync>,
                );

                RwLock::new(EffectInner {
                    #[cfg(any(debug_assertions, halyard_debuginfo))]
                    defined_at,
                    owner,
                    state: ReactiveNodeState::Dirty,
                    run_count_start: 0,
                    run_done_count: 0,
                    run_done_max: 0,
                    last_run_thread_id: thread::current().id(),
                    fun: Arc::new(fun),
                    sources: SourceSet::new(),
                    any_subscriber,
                })
            })
        }
    }

    impl ToAnySubscriber for Arc<RwLock<EffectInner>> {
        fn to_any_subscriber(&self) -> AnySubscriber {
            AnySubscriber(
                Arc::as_ptr(self) as usize,
                Arc::downgrade(self) as Weak<dyn Subscriber + Send + Sync>,
            )
        }
    }

    impl ReactiveNode for RwLock<EffectInner> {
        fn mark_subscribers_check(&self) {}

        fn update_if_necessary(&self) -> bool {
            let state = {
                let guard = self.read().or_poisoned();

                if guard.owner.paused() {
                    return false;
                }

                guard.state
            };

            let needs_update = match state {
                ReactiveNodeState::Clean => false,
                ReactiveNodeState::Check => {
                    let sources = self.read().or_poisoned().sources.clone();
                    sources
                        .into_iter()
                        .any(|source| source.update_if_necessary())
                }
                ReactiveNodeState::Dirty => true,
            };

            if batching() {
                let subscriber =
                    self.read().or_poisoned().any_subscriber.clone();
                if add_to_batch(subscriber) {
                    return needs_update;
                }
            }

            if needs_update {
                let mut guard = self.write().or_poisoned();

                let owner = guard.owner.clone();
                let any_subscriber = guard.any_subscriber.clone();
                let fun = guard.fun.clone();

                // New run has started. (Saturating: the stack is exhausted long before.)
                guard.run_count_start = guard.run_count_start.saturating_add(1);
                // We get a value for this run, the highest value will be what we keep the sources from.
                let recursion_count = guard.run_count_start;
                // We clear the sources before running the effect.
                // Note that this is tied to the ordering of the initial write lock acquisition
                // to ensure the last run is also the last to clear them.
                guard.sources.clear_sources(&any_subscriber);
                // Only this thread will be able to subscribe.
                guard.last_run_thread_id = thread::current().id();

                if recursion_count > 2 {
                    warn_excessive_recursion(&guard);
                }

                drop(guard);

                // We execute the effect.
                // Note that *this could happen in parallel across threads*.
                owner.with_cleanup(|| any_subscriber.with_observer(|| fun()));

                let mut guard = self.write().or_poisoned();

                // This run has completed.
                guard.run_done_count = guard.run_done_count.saturating_add(1);

                // We update the done count.
                // Sources will only be added if recursion_done_max < recursion_count_start.
                // (Meaning the last run is not done yet.)
                guard.run_done_max =
                    Ord::max(recursion_count, guard.run_done_max);

                // The same amount of runs has started and completed,
                // so we can clear everything up for next time.
                if guard.run_count_start == guard.run_done_count {
                    guard.run_count_start = 0;
                    guard.run_done_count = 0;
                    guard.run_done_max = 0;
                    // Can be left unchanged, it'll be set again next time.
                    // guard.last_run_thread_id = thread::current().id();
                }

                guard.state = ReactiveNodeState::Clean;
            }

            needs_update
        }

        fn mark_check(&self) {
            self.write().or_poisoned().state = ReactiveNodeState::Check;
            let any_subscriber =
                self.read().or_poisoned().any_subscriber.clone();
            any_subscriber.with_observer(|| self.update_if_necessary());
        }

        fn mark_dirty(&self) {
            self.write().or_poisoned().state = ReactiveNodeState::Dirty;
            self.update_if_necessary();
        }
    }

    impl Subscriber for RwLock<EffectInner> {
        fn add_source(&self, source: AnySource) {
            let mut guard = self.write().or_poisoned();
            if guard.run_done_max < guard.run_count_start
                && guard.last_run_thread_id == thread::current().id()
            {
                guard.sources.insert(source);
            }
        }

        fn clear_sources(&self, subscriber: &AnySubscriber) {
            self.write().or_poisoned().sources.clear_sources(subscriber);
        }
    }

    impl DefinedAt for EffectInner {
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

    impl std::fmt::Debug for EffectInner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("EffectInner")
                .field("owner", &self.owner)
                .field("state", &self.state)
                .field("sources", &self.sources)
                .field("any_subscriber", &self.any_subscriber)
                .finish()
        }
    }

    fn warn_excessive_recursion(effect: &EffectInner) {
        const MSG: &str = "ImmediateEffect recursed more than once.";
        match effect.defined_at() {
            Some(defined_at) => {
                log_warning(format_args!("{MSG} Defined at: {defined_at}"));
            }
            None => {
                log_warning(format_args!("{MSG}"));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// The run counters used to overflow (a panic in debug builds) at their limit; they
        /// saturate, and the effect still runs.
        #[test]
        fn run_counters_at_their_limit_still_run_the_effect() {
            let owner = Owner::new();
            owner.set();
            let runs = Arc::new(AtomicUsize::new(0));
            let effect = EffectInner::new({
                let runs = Arc::clone(&runs);
                move || {
                    runs.fetch_add(1, Ordering::Relaxed);
                }
            });
            {
                let mut effect = effect.write().or_poisoned();
                effect.run_count_start = usize::MAX;
                effect.run_done_count = usize::MAX;
            }

            effect.update_if_necessary();

            assert_eq!(runs.load(Ordering::Relaxed), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    /// Without the `effects` feature an `ImmediateEffect` has no inner effect: asking for its
    /// subscriber used to panic ("tried to set effect that has been stopped"). It is a
    /// subscriber that tracks nothing.
    #[cfg(not(feature = "effects"))]
    #[test]
    fn an_effect_that_does_not_run_is_a_subscriber_that_tracks_nothing() {
        use super::*;
        use crate::{graph::ReactiveNode, owner::Owner};

        let owner = Owner::new();
        owner.set();
        let effect = ImmediateEffect::new(|| ());

        let subscriber = effect.to_any_subscriber();

        assert!(!subscriber.update_if_necessary());
    }
}
