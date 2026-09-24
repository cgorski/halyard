use super::inner::{MemoFn, MemoInner};
use crate::{
    error::{GraphError, ReportOnce},
    graph::{
        AnySource, AnySubscriber, ReactiveNode, Source, Subscriber,
        ToAnySource, ToAnySubscriber,
    },
    owner::{Storage, StorageAccess, SyncStorage},
    reentry::{self, computing_here, lock_id, Refusal},
    signal::{
        guards::{Mapped, Plain, ReadGuard},
        ArcReadSignal, ArcRwSignal,
    },
    traits::{DefinedAt, Get, IsDisposed, TryReadUntracked},
};
use core::fmt::Debug;
use std::{
    hash::Hash,
    panic::Location,
    sync::{Arc, Weak},
};

/// An efficient derived reactive value based on other reactive values.
///
/// This is a reference-counted memo, which is `Clone` but not `Copy`.
/// For arena-allocated `Copy` memos, use [`Memo`](super::Memo).
///
/// Unlike a "derived signal," a memo comes with two guarantees:
/// 1. The memo will only run *once* per change, no matter how many times you
///    access its value.
/// 2. The memo will only notify its dependents if the value of the computation changes.
///
/// This makes a memo the perfect tool for expensive computations.
///
/// Memos have a certain overhead compared to derived signals. In most cases, you should
/// create a derived signal. But if the derivation calculation is expensive, you should
/// create a memo.
///
/// As with an [`Effect`](crate::effect::Effect), the argument to the memo function is the previous value,
/// i.e., the current value of the memo, which will be `None` for the initial calculation.
///
/// A read of the memo from inside its own function (a cycle) gives that previous value, and
/// is reported once. During the first computation there is none: a strong read there has no
/// possible value, and aborts (see [`Strong`](crate::traits::Strong)).
///
/// ## Examples
/// ```
/// # use halyard_reactive_graph::prelude::*; let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// # use halyard_reactive_graph::computed::*;
/// # use halyard_reactive_graph::signal::signal;
/// # fn really_expensive_computation(value: i32) -> i32 { value };
/// # use halyard_reactive_graph::signal::arc_signal;
/// // an `ArcMemo` is strong: it reads strong handles, which keep their values alive
/// let (value, set_value) = arc_signal(0);
///
/// // 🆗 we could create a derived signal with a simple function
/// let double_value = {
///     let value = value.clone();
///     move || value.get() * 2
/// };
/// set_value.set(2);
/// assert_eq!(double_value(), 4);
///
/// // but imagine the computation is really expensive
/// let expensive = {
///     let value = value.clone();
///     move || really_expensive_computation(value.get()) // lazy: doesn't run until called
/// };
/// // 🆗 run #1: calls `really_expensive_computation` the first time
/// println!("expensive = {}", expensive());
/// // ❌ run #2: this calls `really_expensive_computation` a second time!
/// let some_value = expensive();
///
/// // instead, we create a memo
/// // 🆗 run #1: the calculation runs once immediately
/// let memoized = ArcMemo::new(move |_| really_expensive_computation(value.get()));
/// // 🆗 reads the current value of the memo
/// println!("memoized = {}", memoized.get());
/// // ✅ reads the current value **without re-running the calculation**
/// let some_value = memoized.get();
/// ```
///
/// ## Core Trait Implementations
/// - [`.get()`](crate::traits::Get) clones the current value of the memo.
///   If you call it within an effect, it will cause that effect to subscribe
///   to the memo, and to re-run whenever the value of the memo changes.
///   - [`.get_untracked()`](crate::traits::TryGetUntracked) clones the value of
///     the memo without reactively tracking it.
/// - [`.read()`](crate::traits::Read) returns a guard that allows accessing the
///   value of the memo by reference. If you call it within an effect, it will
///   cause that effect to subscribe to the memo, and to re-run whenever the
///   value of the memo changes.
///   - [`.read_untracked()`](crate::traits::TryReadUntracked) gives access to the
///     current value of the memo without reactively tracking it.
/// - [`.with()`](crate::traits::With) allows you to reactively access the memo’s
///   value without cloning by applying a callback function.
///   - [`.with_untracked()`](crate::traits::TryWithUntracked) allows you to access
///     the memo’s value by applying a callback function without reactively
///     tracking it.
/// - [`.to_stream()`](crate::traits::ToStream) converts the memo to an `async`
///   stream of values.
/// - [`::from_stream()`](crate::traits::FromStream) converts an `async` stream
///   of values into a memo containing the latest value.
pub struct ArcMemo<T, S = SyncStorage>
where
    S: Storage<T>,
{
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
    pub(crate) inner: Arc<MemoInner<T, S>>,
}

crate::impl_strong!([T, S] ArcMemo<T, S> where [S: Storage<T>]);

impl<T, S> ArcMemo<T, S>
where
    S: Storage<T>,
{
    /// Returns a weak (arena) handle to the value: `Copy`, and it does not keep the value
    /// alive (like [`std::sync::Arc::downgrade`]). The reverse is `upgrade` on the weak
    /// handle.
    #[track_caller]
    pub fn downgrade(&self) -> crate::computed::Memo<T, S>
    where
        crate::computed::Memo<T, S>: From<Self>,
    {
        self.clone().into()
    }

    /// A memo made with [`Memo::new_try`](crate::computed::Memo::new_try) whose function gave
    /// no value (a source it reads is gone): it holds nothing, and it is not being computed
    /// by this thread.
    pub(crate) fn has_no_value(&self) -> bool
    where
        T: 'static,
    {
        self.inner.is_fallible()
            && !computing_here(lock_id(&*self.inner.value))
            && matches!(self.inner.value.try_read(), Ok(value) if value.is_none())
    }
}

impl<T: 'static> ArcMemo<T, SyncStorage>
where
    SyncStorage: Storage<T>,
{
    /// Creates a new memo by passing a function that computes the value.
    ///
    /// This is lazy: the function will not be called until the memo's value is read for the first
    /// time.
    #[track_caller]
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", skip_all)
    )]
    pub fn new(fun: impl Fn(Option<&T>) -> T + Send + Sync + 'static) -> Self
    where
        T: PartialEq,
    {
        Self::new_with_compare(fun, |lhs, rhs| lhs.as_ref() != rhs.as_ref())
    }

    /// Creates a new memo by passing a function that computes the value, and a comparison function
    /// that takes the previous value and the new value and returns `true` if the value has
    /// changed.
    ///
    /// This is lazy: the function will not be called until the memo's value is read for the first
    /// time.
    #[track_caller]
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", skip_all)
    )]
    pub fn new_with_compare(
        fun: impl Fn(Option<&T>) -> T + Send + Sync + 'static,
        changed: fn(Option<&T>, Option<&T>) -> bool,
    ) -> Self {
        Self::from_fn(MemoFn::Borrowing(Arc::new(move |prev: Option<&T>| {
            let new_value = fun(prev);
            let changed = changed(prev, Some(&new_value));
            (Some(new_value), changed)
        })))
    }

    /// Creates a new memo by passing a function that computes the value.
    ///
    /// Unlike [`ArcMemo::new`](), this receives ownership of the previous value. As a result, it
    /// must return both the new value and a `bool` that is `true` if the value has changed.
    ///
    /// While the function runs, the memo has no value (the function owns it): a strong read
    /// of the memo from inside its own function has no possible value, and aborts (see
    /// [`Strong`](crate::traits::Strong)).
    ///
    /// This is lazy: the function will not be called until the memo's value is read for the first
    /// time.
    #[track_caller]
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", skip_all)
    )]
    pub fn new_owning(
        fun: impl Fn(Option<T>) -> (T, bool) + Send + Sync + 'static,
    ) -> Self {
        Self::from_fn(MemoFn::Owning(Arc::new(move |prev| {
            let (value, changed) = fun(prev);
            (Some(value), changed)
        })))
    }

    /// A memo whose function may give no value (`None`): then reads give `None` until a
    /// source changes. Only for [`Memo::new_try`](crate::computed::Memo::new_try): a strong
    /// memo always has a value.
    #[track_caller]
    pub(crate) fn new_try(
        fun: impl Fn(Option<&T>) -> (Option<T>, bool) + Send + Sync + 'static,
    ) -> Self {
        Self::from_fn(MemoFn::Borrowing(Arc::new(fun)))
    }

    #[track_caller]
    fn from_fn(fun: MemoFn<T>) -> Self {
        let caller = Location::caller();
        let defined_at =
            cfg!(any(debug_assertions, halyard_debuginfo)).then_some(caller);
        let inner = Arc::new_cyclic(|weak| {
            let subscriber = AnySubscriber(
                weak.as_ptr() as usize,
                Weak::clone(weak) as Weak<dyn Subscriber + Send + Sync>,
            );

            MemoInner::new(fun, subscriber, defined_at)
        });
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
            inner,
        }
    }
}

impl<T, S> Clone for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn clone(&self) -> Self {
        Self {
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, S> DefinedAt for ArcMemo<T, S>
where
    S: Storage<T>,
{
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

impl<T, S> Debug for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArcMemo")
            .field("type", &std::any::type_name::<T>())
            .field("data", &Arc::as_ptr(&self.inner))
            .finish()
    }
}

impl<T, S> PartialEq for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl<T, S> Eq for ArcMemo<T, S> where S: Storage<T> {}

impl<T, S> Hash for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(&Arc::as_ptr(&self.inner), state);
    }
}

impl<T: 'static, S> ReactiveNode for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn mark_dirty(&self) {
        self.inner.mark_dirty();
    }

    fn mark_check(&self) {
        self.inner.mark_check();
    }

    fn mark_subscribers_check(&self) {
        self.inner.mark_subscribers_check();
    }

    fn update_if_necessary(&self) -> bool {
        self.inner.update_if_necessary()
    }
}

impl<T: 'static, S> IsDisposed for ArcMemo<T, S>
where
    S: Storage<T>,
{
    #[inline(always)]
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T: 'static, S> ToAnySource for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn to_any_source(&self) -> AnySource {
        AnySource(
            Arc::as_ptr(&self.inner) as usize,
            Arc::downgrade(&self.inner) as Weak<dyn Source + Send + Sync>,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            self.defined_at,
        )
    }
}

impl<T: 'static, S> Source for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn add_subscriber(&self, subscriber: AnySubscriber) {
        self.inner.add_subscriber(subscriber);
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.inner.remove_subscriber(subscriber);
    }

    fn clear_subscribers(&self) {
        self.inner.clear_subscribers();
    }
}

impl<T: 'static, S> ToAnySubscriber for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn to_any_subscriber(&self) -> AnySubscriber {
        AnySubscriber(
            Arc::as_ptr(&self.inner) as usize,
            Arc::downgrade(&self.inner) as Weak<dyn Subscriber + Send + Sync>,
        )
    }
}

impl<T: 'static, S> Subscriber for ArcMemo<T, S>
where
    S: Storage<T>,
{
    fn add_source(&self, source: AnySource) {
        self.inner.add_source(source);
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        self.inner.clear_sources(subscriber);
    }
}

/// A memo read inside its own computation (logged once).
static MEMO_READ_ITSELF: ReportOnce = ReportOnce::new();

impl<T: 'static, S> TryReadUntracked for ArcMemo<T, S>
where
    S: Storage<T>,
{
    type Value = ReadGuard<T, Mapped<Plain<Option<S::Wrapped>>, T>>;

    #[track_caller]
    fn try_read_untracked(&self) -> Option<Self::Value> {
        let at = crate::gone::take_strong_read_site()
            .unwrap_or_else(Location::caller);
        // Read inside its own computation on this thread (a cycle): the memo's value is the
        // previous one, which the computation holds a read guard on (shared with this read);
        // during the first computation, or that of a memo made with `new_owning`, there is
        // none.
        let computing = computing_here(lock_id(&*self.inner.value));
        if computing {
            MEMO_READ_ITSELF.report(|| GraphError::MemoReadItself {
                defined_at: self.defined_at(),
                at,
            });
        } else {
            self.update_if_necessary();
        }

        let value = Plain::try_new_at(
            Arc::clone(&self.inner.value),
            self.defined_at(),
        )?;
        if value.is_none() {
            // The value is missing while it is being computed: by this thread, which has no
            // previous value to give (a strong read cannot wait for that: see
            // `gone::wait_for`), or by another thread (a memo made with `new_owning`, or its
            // first computation). A memo made with `Memo::new_try` also has none while its
            // function gives `None`.
            if computing {
                reentry::refuse_here(Refusal {
                    defined_at: self.defined_at(),
                    why: "it is a memo read inside its own computation, which has no \
                          previous value to give (its first computation, or a memo \
                          made with `new_owning`, whose function owns the previous value)",
                });
            }
            return None;
        }
        // The guard just taken keeps the value from being replaced while it lives, so the
        // mapping below always finds it.
        Some(ReadGuard::new(Mapped::new_with_guard(value, |t| {
            t.as_ref().unwrap().as_borrowed()
        })))
    }
}

impl<T> From<ArcReadSignal<T>> for ArcMemo<T, SyncStorage>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    #[track_caller]
    fn from(value: ArcReadSignal<T>) -> Self {
        ArcMemo::new(move |_| value.get())
    }
}

impl<T> From<ArcRwSignal<T>> for ArcMemo<T, SyncStorage>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    #[track_caller]
    fn from(value: ArcRwSignal<T>) -> Self {
        ArcMemo::new(move |_| value.get())
    }
}
