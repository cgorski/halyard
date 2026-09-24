//! Guards that integrate with the reactive system, wrapping references to the values of signals.

pub use super::commit::SignalWriteGuard;
use crate::{
    computed::BlockingLock,
    error::{Access, GraphError, ReportOnce},
    reentry::{self, lock_id, Held, SINGLE_THREADED},
    traits::{Notify, UntrackableGuard},
};
use core::fmt::Debug;
use guardian::ArcRwLockReadGuardian;
use std::{
    any::Any,
    borrow::Borrow,
    fmt::Display,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    panic::Location,
    rc::Rc,
    sync::{Arc, PoisonError, RwLock},
};

/// Re-entrant reads and writes are each logged once.
static REENTERED_READ: ReportOnce = ReportOnce::new();
static REENTERED_WRITE: ReportOnce = ReportOnce::new();

pub(crate) fn report_reentered(
    access: Access,
    defined_at: Option<&'static Location<'static>>,
) {
    let once = match access {
        Access::Read => &REENTERED_READ,
        Access::Write => &REENTERED_WRITE,
    };
    once.report(|| GraphError::Reentered { access, defined_at });
}

/// A wrapper type for any kind of guard returned by [`Read`](crate::traits::Read).
///
/// If `Inner` implements `Deref`, so does `ReadGuard<_, Inner>`.
#[derive(Debug)]
pub struct ReadGuard<T, Inner> {
    ty: PhantomData<T>,
    inner: Inner,
}

impl<T, Inner> ReadGuard<T, Inner> {
    /// Creates a new wrapper around another guard type.
    pub fn new(inner: Inner) -> Self {
        Self {
            inner,
            ty: PhantomData,
        }
    }

    /// Returns the inner guard type.
    pub fn into_inner(self) -> Inner {
        self.inner
    }
}

impl<T, Inner> Clone for ReadGuard<T, Inner>
where
    Inner: Clone,
{
    fn clone(&self) -> Self {
        Self {
            ty: self.ty,
            inner: self.inner.clone(),
        }
    }
}

impl<T, Inner> Deref for ReadGuard<T, Inner>
where
    Inner: Deref<Target = T>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

impl<T, Inner> Borrow<T> for ReadGuard<T, Inner>
where
    Inner: Deref<Target = T>,
{
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T, Inner> PartialEq<T> for ReadGuard<T, Inner>
where
    Inner: Deref<Target = T>,
    T: PartialEq,
{
    fn eq(&self, other: &Inner::Target) -> bool {
        self.deref() == other
    }
}

impl<T, Inner> Display for ReadGuard<T, Inner>
where
    Inner: Deref<Target = T>,
    T: Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A guard that provides access to a signal's inner value.
///
/// Natively, nested reads of the same value on one thread share one lock guard: the lock is
/// taken once per thread, and released when the last of them is dropped.
pub struct Plain<T: 'static> {
    guard: ReadHold<T>,
    // after `guard`: the lock is released before it stops being recorded as held
    _held: Held,
}

/// How a [`Plain`] holds its lock.
enum ReadHold<T: 'static> {
    /// With one thread, a nested read takes the lock again: no writer can be waiting.
    Own(ArcRwLockReadGuardian<T>),
    /// Natively, nested reads share the guard: a second read of a std lock may wait forever
    /// for a writer (on another thread) that is itself waiting for the first.
    Shared(Rc<ArcRwLockReadGuardian<T>>),
}

impl<T: 'static> Debug for Plain<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plain").finish()
    }
}

impl<T: 'static> Plain<T> {
    /// Takes a reference-counted read guard on the given lock. A lock poisoned by a panic
    /// still gives its value.
    ///
    /// Natively this waits while another thread writes the value. It returns `None` (and
    /// logs that, once) if this thread holds the value's write lock, which would never end;
    /// and in the browser, if the lock is busy at all (only this thread could hold it).
    /// No lock is held for writing while other code runs (a write swaps a new value in), so
    /// neither happens in practice.
    pub fn try_new(inner: Arc<RwLock<T>>) -> Option<Self> {
        Self::try_new_at(inner, None)
    }

    /// [`Plain::try_new`], naming where the value was created if the read is re-entrant.
    pub(crate) fn try_new_at(
        inner: Arc<RwLock<T>>,
        defined_at: Option<&'static Location<'static>>,
    ) -> Option<Self> {
        let lock = lock_id(&*inner);
        let state = reentry::state(lock);
        if state.is_some_and(|state| state.write_locks > 0) {
            report_reentered(Access::Read, defined_at);
            reentry::refuse_here(reentry::Refusal {
                defined_at,
                why: "this thread holds its write lock",
            });
            return None;
        }
        let held = Held::new(lock);
        // a read already alive on this thread: share its guard
        if let Some(shared) = reentry::shared_read(lock).and_then(|shared| {
            shared.downcast::<ArcRwLockReadGuardian<T>>().ok()
        }) {
            return Some(Plain {
                guard: ReadHold::Shared(shared),
                _held: held,
            });
        }
        let taken = match ArcRwLockReadGuardian::try_take(Arc::clone(&inner)) {
            Some(taken) => taken,
            // written by another thread: a signal's write holds the lock only to swap the new
            // value in, so wait for it (not if this thread uses the value already: a writer
            // may be waiting for this thread)
            None if !SINGLE_THREADED
                && state.is_some_and(|state| !state.in_use()) =>
            {
                ArcRwLockReadGuardian::take(inner)
            }
            None => {
                report_reentered(Access::Read, defined_at);
                if SINGLE_THREADED {
                    reentry::refuse_here(reentry::Refusal {
                        defined_at,
                        why: "its lock is held by the code that is running now",
                    });
                }
                return None;
            }
        };
        let guard = taken.unwrap_or_else(PoisonError::into_inner);
        let guard = if SINGLE_THREADED {
            ReadHold::Own(guard)
        } else {
            let guard = Rc::new(guard);
            let shared: Rc<dyn Any> = guard.clone();
            reentry::share_read(lock, &shared);
            ReadHold::Shared(guard)
        };
        Some(Plain { guard, _held: held })
    }
}

impl<T> Deref for Plain<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &self.guard {
            ReadHold::Own(guard) => guard.deref(),
            ReadHold::Shared(guard) => guard.deref().deref(),
        }
    }
}

impl<T: PartialEq> PartialEq for Plain<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: PartialEq> PartialEq<T> for Plain<T> {
    fn eq(&self, other: &T) -> bool {
        **self == *other
    }
}

impl<T: Display> Display for Plain<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A guard that provides access to an async signal's value.
pub struct AsyncPlain<T: 'static> {
    guard: async_lock::RwLockReadGuardArc<T>,
    // after `guard`: the lock is released before it stops being recorded as held
    _held: Held,
}

impl<T: 'static> Debug for AsyncPlain<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncPlain").finish()
    }
}

impl<T: 'static> AsyncPlain<T> {
    /// Takes a reference-counted async read guard on the given lock.
    ///
    /// Natively this waits for a writer to finish. In the browser, where a busy lock can only
    /// be held by the code that is running now, this returns `None`, and that is logged once.
    pub fn try_new(inner: &Arc<async_lock::RwLock<T>>) -> Option<Self> {
        let guard = inner.blocking_read_arc();
        if guard.is_none() {
            report_reentered(Access::Read, None);
            let write_locked_here = reentry::state(lock_id(&**inner))
                .is_some_and(|state| state.write_locks > 0);
            if SINGLE_THREADED || write_locked_here {
                reentry::refuse_here(reentry::Refusal {
                    defined_at: None,
                    why: "its lock is held by the code that is running now",
                });
            }
        }
        guard.map(|guard| Self::recorded(guard, inner))
    }

    /// Wraps a read guard taken on `lock`, recording it as held by this thread.
    pub(crate) fn recorded(
        guard: async_lock::RwLockReadGuardArc<T>,
        lock: &Arc<async_lock::RwLock<T>>,
    ) -> Self {
        Self {
            guard,
            _held: Held::new(lock_id(&**lock)),
        }
    }
}

impl<T> Deref for AsyncPlain<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

/// A guard on an async value, taken by awaiting it (`by_ref().await`).
///
/// Unlike [`AsyncPlain`], it may be held across `.await`s and moved between threads, so it is
/// not recorded as held by one thread: a synchronous write to the same value from the thread
/// that holds it waits for it to be dropped.
pub struct AsyncAwaited<T: 'static> {
    pub(crate) guard: async_lock::RwLockReadGuardArc<T>,
}

impl<T: 'static> Debug for AsyncAwaited<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncAwaited").finish()
    }
}

impl<T> Deref for AsyncAwaited<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<T: PartialEq> PartialEq for AsyncPlain<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: PartialEq> PartialEq<T> for AsyncPlain<T> {
    fn eq(&self, other: &T) -> bool {
        **self == *other
    }
}

impl<T: Display> Display for AsyncPlain<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A guard that maps over another guard.
#[derive(Debug)]
pub struct Mapped<Inner, U>
where
    Inner: Deref,
{
    inner: Inner,
    map_fn: fn(&Inner::Target) -> &U,
}

impl<T: 'static, U> Mapped<Plain<T>, U> {
    /// Creates a mapped read guard from the inner lock.
    pub fn try_new(
        inner: Arc<RwLock<T>>,
        map_fn: fn(&T) -> &U,
    ) -> Option<Self> {
        let inner = Plain::try_new(inner)?;
        Some(Self { inner, map_fn })
    }
}

impl<Inner, U> Mapped<Inner, U>
where
    Inner: Deref,
{
    /// Creates a mapped read guard from the inner guard.
    pub fn new_with_guard(
        inner: Inner,
        map_fn: fn(&Inner::Target) -> &U,
    ) -> Self {
        Self { inner, map_fn }
    }
}

impl<Inner, U> Deref for Mapped<Inner, U>
where
    Inner: Deref,
{
    type Target = U;

    fn deref(&self) -> &Self::Target {
        (self.map_fn)(self.inner.deref())
    }
}

impl<Inner, U: PartialEq> PartialEq for Mapped<Inner, U>
where
    Inner: Deref,
{
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<Inner, U: PartialEq> PartialEq<U> for Mapped<Inner, U>
where
    Inner: Deref,
{
    fn eq(&self, other: &U) -> bool {
        **self == *other
    }
}

impl<Inner, U: Display> Display for Mapped<Inner, U>
where
    Inner: Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A guard that provides mutable access to a signal's value, triggering some reactive change
/// when it is dropped.
#[derive(Debug)]
pub struct WriteGuard<S, G>
where
    S: Notify,
{
    // Fields are dropped in the order they are declared: the inner guard first (releasing
    // the lock), then the notifier, so that subscribers run with the value unlocked.
    pub(crate) guard: G,
    pub(crate) triggerable: NotifyOnDrop<S>,
}

/// Notifies its signal's subscribers when dropped, unless it was untracked.
#[derive(Debug)]
pub(crate) struct NotifyOnDrop<S: Notify>(Option<S>);

impl<S: Notify> Drop for NotifyOnDrop<S> {
    fn drop(&mut self) {
        if let Some(triggerable) = self.0.as_ref() {
            triggerable.notify();
        }
    }
}

impl<S, G> WriteGuard<S, G>
where
    S: Notify,
{
    /// Creates a new guard from the inner mutable guard type, and the signal that should be
    /// triggered on drop.
    pub fn new(triggerable: S, guard: G) -> Self {
        Self {
            guard,
            triggerable: NotifyOnDrop(Some(triggerable)),
        }
    }
}

impl<S, G> UntrackableGuard for WriteGuard<S, G>
where
    S: Notify,
    G: DerefMut,
{
    /// Removes the triggerable type, so that it is no longer notifies when dropped.
    fn untrack(&mut self) {
        self.triggerable.0.take();
    }
}

impl<S, G> Deref for WriteGuard<S, G>
where
    S: Notify,
    G: Deref,
{
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<S, G> DerefMut for WriteGuard<S, G>
where
    S: Notify,
    G: DerefMut,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.deref_mut()
    }
}

/// A write guard over a copy of a value: the copy is changed while the guard lives, and
/// committed (by replacing the value) when it is dropped. No lock is held while it is alive,
/// so the value can still be read (giving the committed value); nothing is ever lent out for
/// a change in place. Returned by [`Write::try_write`](crate::traits::Write::try_write) for
/// values that are not signals, and by
/// [`WriteValue::try_write_value`](crate::traits::WriteValue::try_write_value).
///
/// Whether the commit notifies subscribers depends on the value (a stored value has none);
/// [`untrack`](UntrackableGuard::untrack) turns the notification off. A guard dropped by a
/// panic commits nothing: the value stays as it was.
pub struct CopyWriteGuard<T: 'static> {
    // dropped after the commit: then the previous value
    value: T,
    notify: bool,
    commit: Option<Box<Commit<T>>>,
}

/// Swaps the copy in (it then holds the previous value), notifying if asked; says whether it
/// could.
type Commit<T> = dyn FnOnce(&mut T, bool) -> bool;

impl<T: 'static> CopyWriteGuard<T> {
    /// A guard over `value` (a copy of the committed value); `commit` swaps it in when the
    /// guard is dropped (`value` then holds the previous value, dropped last), notifying if
    /// its second argument is `true`, and says whether it could.
    pub(crate) fn new(
        value: T,
        commit: impl FnOnce(&mut T, bool) -> bool + 'static,
    ) -> Self {
        Self {
            value,
            notify: true,
            commit: Some(Box::new(commit)),
        }
    }

    /// Commits now, as dropping the guard does, and says whether the value could be stored.
    pub(crate) fn commit(mut self) -> bool {
        self.commit
            .take()
            .is_some_and(|commit| commit(&mut self.value, self.notify))
    }
}

impl<T: 'static> Debug for CopyWriteGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyWriteGuard")
            .field("notify", &self.notify)
            .finish_non_exhaustive()
    }
}

impl<T> Deref for CopyWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for CopyWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T> UntrackableGuard for CopyWriteGuard<T> {
    fn untrack(&mut self) {
        self.notify = false;
    }
}

impl<T> Drop for CopyWriteGuard<T> {
    fn drop(&mut self) {
        // dropped by a panic, the change may be half made: the value stays as it was
        if std::thread::panicking() {
            return;
        }
        if let Some(commit) = self.commit.take() {
            _ = commit(&mut self.value, self.notify);
        }
    }
}

/// A mutable guard that maps over an inner mutable guard.
#[derive(Debug)]
pub struct MappedMut<Inner, U>
where
    Inner: Deref,
{
    inner: Inner,
    map_fn: fn(&Inner::Target) -> &U,
    map_fn_mut: fn(&mut Inner::Target) -> &mut U,
}

impl<Inner, U> UntrackableGuard for MappedMut<Inner, U>
where
    Inner: UntrackableGuard,
{
    fn untrack(&mut self) {
        self.inner.untrack();
    }
}

impl<Inner, U> MappedMut<Inner, U>
where
    Inner: DerefMut,
{
    /// Creates a new writable guard from the inner guard.
    pub fn new(
        inner: Inner,
        map_fn: fn(&Inner::Target) -> &U,
        map_fn_mut: fn(&mut Inner::Target) -> &mut U,
    ) -> Self {
        Self {
            inner,
            map_fn,
            map_fn_mut,
        }
    }
}

impl<Inner, U> Deref for MappedMut<Inner, U>
where
    Inner: Deref,
{
    type Target = U;

    fn deref(&self) -> &Self::Target {
        (self.map_fn)(self.inner.deref())
    }
}

impl<Inner, U> DerefMut for MappedMut<Inner, U>
where
    Inner: DerefMut,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        (self.map_fn_mut)(self.inner.deref_mut())
    }
}

impl<Inner, U: PartialEq> PartialEq for MappedMut<Inner, U>
where
    Inner: Deref,
{
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<Inner, U: Display> Display for MappedMut<Inner, U>
where
    Inner: Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A mapped read guard in which the mapping function is a closure. If the mapping function is a
/// function pointer, use [`Mapped`].
pub struct MappedArc<Inner, U>
where
    Inner: Deref,
{
    inner: Inner,
    #[allow(clippy::type_complexity)]
    map_fn: Arc<dyn Fn(&Inner::Target) -> &U>,
}

impl<Inner, U> Clone for MappedArc<Inner, U>
where
    Inner: Clone + Deref,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            map_fn: self.map_fn.clone(),
        }
    }
}

impl<Inner, U> Debug for MappedArc<Inner, U>
where
    Inner: Debug + Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedArc")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<Inner, U> MappedArc<Inner, U>
where
    Inner: Deref,
{
    /// Creates a new mapped guard from the inner guard and the map function.
    pub fn new(
        inner: Inner,
        map_fn: impl Fn(&Inner::Target) -> &U + 'static,
    ) -> Self {
        Self {
            inner,
            map_fn: Arc::new(map_fn),
        }
    }
}

impl<Inner, U> Deref for MappedArc<Inner, U>
where
    Inner: Deref,
{
    type Target = U;

    fn deref(&self) -> &Self::Target {
        (self.map_fn)(self.inner.deref())
    }
}

impl<Inner, U: PartialEq> PartialEq for MappedArc<Inner, U>
where
    Inner: Deref,
{
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<Inner, U: Display> Display for MappedArc<Inner, U>
where
    Inner: Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A mapped write guard in which the mapping function is a closure. If the mapping function is a
/// function pointer, use [`MappedMut`].
pub struct MappedMutArc<Inner, U>
where
    Inner: Deref,
{
    inner: Inner,
    #[allow(clippy::type_complexity)]
    map_fn: Arc<dyn Fn(&Inner::Target) -> &U>,
    #[allow(clippy::type_complexity)]
    map_fn_mut: Arc<dyn Fn(&mut Inner::Target) -> &mut U>,
}

impl<Inner, U> Clone for MappedMutArc<Inner, U>
where
    Inner: Clone + Deref,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            map_fn: self.map_fn.clone(),
            map_fn_mut: self.map_fn_mut.clone(),
        }
    }
}

impl<Inner, U> Debug for MappedMutArc<Inner, U>
where
    Inner: Debug + Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedMutArc")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<Inner, U> UntrackableGuard for MappedMutArc<Inner, U>
where
    Inner: UntrackableGuard,
{
    fn untrack(&mut self) {
        self.inner.untrack();
    }
}

impl<Inner, U> MappedMutArc<Inner, U>
where
    Inner: Deref,
{
    /// Creates the new mapped mutable guard from the inner guard and mapping functions.
    pub fn new(
        inner: Inner,
        map_fn: impl Fn(&Inner::Target) -> &U + 'static,
        map_fn_mut: impl Fn(&mut Inner::Target) -> &mut U + 'static,
    ) -> Self {
        Self {
            inner,
            map_fn: Arc::new(map_fn),
            map_fn_mut: Arc::new(map_fn_mut),
        }
    }
}

impl<Inner, U> Deref for MappedMutArc<Inner, U>
where
    Inner: Deref,
{
    type Target = U;

    fn deref(&self) -> &Self::Target {
        (self.map_fn)(self.inner.deref())
    }
}

impl<Inner, U> DerefMut for MappedMutArc<Inner, U>
where
    Inner: DerefMut,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        (self.map_fn_mut)(self.inner.deref_mut())
    }
}

impl<Inner, U: PartialEq> PartialEq for MappedMutArc<Inner, U>
where
    Inner: Deref,
{
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<Inner, U: Display> Display for MappedMutArc<Inner, U>
where
    Inner: Deref,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// A wrapper that implements [`Deref`] and [`Borrow`] for itself.
pub struct Derefable<T>(pub T);

impl<T> Clone for Derefable<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Derefable(self.0.clone())
    }
}

impl<T> std::ops::Deref for Derefable<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> Borrow<T> for Derefable<T> {
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T> PartialEq<T> for Derefable<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        self.deref() == other
    }
}

impl<T> Display for Derefable<T>
where
    T: Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}
