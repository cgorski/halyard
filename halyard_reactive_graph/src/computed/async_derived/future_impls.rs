use super::{inner::ArcAsyncDerivedInner, ArcAsyncDerived, AsyncDerived};
use crate::or_poisoned::OrPoisoned;
use crate::{
    computed::suspense::SuspenseContext,
    diagnostics::SpecialNonReactiveZone,
    graph::{AnySource, ToAnySource},
    owner::{use_context, Storage},
    send_wrapper_ext::SendOption,
    signal::guards::{AsyncAwaited, Mapped, ReadGuard},
    traits::Track,
};
use futures::pin_mut;
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    task::{Context, Poll, Waker},
};

/// A read guard that holds access to an async derived resource.
///
/// Implements [`Deref`](std::ops::Deref) to access the inner value. This should not be held longer
/// than it is needed, as it prevents updates to the inner value.
pub type AsyncDerivedGuard<T> =
    ReadGuard<T, Mapped<AsyncAwaited<SendOption<T>>, T>>;

/// A [`Future`] that is ready when an [`ArcAsyncDerived`] is finished loading or reloading,
/// but does not contain its value.
pub struct AsyncDerivedReadyFuture {
    pub(crate) source: AnySource,
    pub(crate) loading: Arc<AtomicBool>,
    pub(crate) wakers: Arc<RwLock<Vec<Waker>>>,
}

impl AsyncDerivedReadyFuture {
    /// Creates a new [`Future`] that will be ready when the given resource is ready.
    pub fn new(
        source: AnySource,
        loading: &Arc<AtomicBool>,
        wakers: &Arc<RwLock<Vec<Waker>>>,
    ) -> Self {
        AsyncDerivedReadyFuture {
            source,
            loading: Arc::clone(loading),
            wakers: Arc::clone(wakers),
        }
    }
}

impl Future for AsyncDerivedReadyFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(debug_assertions)]
        let _guard = SpecialNonReactiveZone::enter();
        let waker = cx.waker();
        self.source.track();
        if self.loading.load(Ordering::Relaxed) {
            self.wakers.write().or_poisoned().push(waker.clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl<T> IntoFuture for ArcAsyncDerived<T>
where
    T: Clone + 'static,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    fn into_future(self) -> Self::IntoFuture {
        AsyncDerivedFuture {
            source: self.to_any_source(),
            value: Arc::clone(&self.value),
            loading: Arc::clone(&self.loading),
            wakers: Arc::clone(&self.wakers),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, S> IntoFuture for AsyncDerived<T, S>
where
    T: Clone + 'static,
    S: Storage<ArcAsyncDerived<T>>,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    #[track_caller]
    fn into_future(self) -> Self::IntoFuture {
        self.inner_or_pending().into_future()
    }
}

/// A [`Future`] that is ready when an [`ArcAsyncDerived`] is finished loading or reloading,
/// and contains its value. `.await`ing this clones the value `T`.
pub struct AsyncDerivedFuture<T> {
    source: AnySource,
    value: Arc<async_lock::RwLock<SendOption<T>>>,
    loading: Arc<AtomicBool>,
    wakers: Arc<RwLock<Vec<Waker>>>,
    inner: Arc<RwLock<ArcAsyncDerivedInner>>,
}

impl<T> Future for AsyncDerivedFuture<T>
where
    T: Clone + 'static,
{
    type Output = T;

    #[track_caller]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(debug_assertions)]
        let _guard = SpecialNonReactiveZone::enter();
        let waker = cx.waker();
        self.source.track();
        let value = self.value.read_arc();

        if let Some(suspense_context) = use_context::<SuspenseContext>() {
            self.inner
                .write()
                .or_poisoned()
                .suspenses
                .push(suspense_context);
        }

        pin_mut!(value);
        match (self.loading.load(Ordering::Relaxed), value.poll(cx)) {
            (true, _) => {
                self.wakers.write().or_poisoned().push(waker.clone());
                Poll::Pending
            }
            (_, Poll::Pending) => Poll::Pending,
            (_, Poll::Ready(guard)) => match guard.as_ref() {
                Some(value) => Poll::Ready(value.clone()),
                // emptied (`set(None)`) after it loaded: ready with the next value (a write
                // or a reload wakes the wakers)
                None => {
                    self.wakers.write().or_poisoned().push(waker.clone());
                    Poll::Pending
                }
            },
        }
    }
}

impl<T: 'static> ArcAsyncDerived<T> {
    /// Returns a `Future` that resolves when the computation is finished, and accesses the inner
    /// value by reference rather than by cloning it.
    #[track_caller]
    pub fn by_ref(&self) -> AsyncDerivedRefFuture<T> {
        AsyncDerivedRefFuture {
            source: self.to_any_source(),
            value: Arc::clone(&self.value),
            loading: Arc::clone(&self.loading),
            wakers: Arc::clone(&self.wakers),
        }
    }
}

impl<T, S> AsyncDerived<T, S>
where
    T: 'static,
    S: Storage<ArcAsyncDerived<T>>,
{
    /// Returns a `Future` that resolves when the computation is finished, and accesses the inner
    /// value by reference rather than by cloning it.
    #[track_caller]
    pub fn by_ref(&self) -> AsyncDerivedRefFuture<T> {
        self.inner_or_pending().by_ref()
    }
}

/// A [`Future`] that is ready when an [`ArcAsyncDerived`] is finished loading or reloading,
/// and yields an [`AsyncDerivedGuard`] that dereferences to its value.
pub struct AsyncDerivedRefFuture<T> {
    source: AnySource,
    value: Arc<async_lock::RwLock<SendOption<T>>>,
    loading: Arc<AtomicBool>,
    wakers: Arc<RwLock<Vec<Waker>>>,
}

impl<T> Future for AsyncDerivedRefFuture<T>
where
    T: 'static,
{
    type Output = AsyncDerivedGuard<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(debug_assertions)]
        let _guard = SpecialNonReactiveZone::enter();
        let waker = cx.waker();
        self.source.track();
        let value = self.value.read_arc();
        pin_mut!(value);
        match (self.loading.load(Ordering::Relaxed), value.poll(cx)) {
            (true, _) => {
                self.wakers.write().or_poisoned().push(waker.clone());
                Poll::Pending
            }
            (_, Poll::Pending) => Poll::Pending,
            // emptied (`set(None)`) after it loaded: ready with the next value
            (_, Poll::Ready(guard)) if guard.is_none() => {
                self.wakers.write().or_poisoned().push(waker.clone());
                Poll::Pending
            }
            // The value was just seen to be there, and the read guard keeps it from being
            // emptied while the guard lives, so the mapping always finds it.
            (_, Poll::Ready(guard)) => Poll::Ready(ReadGuard::new(
                Mapped::new_with_guard(AsyncAwaited { guard }, |guard| {
                    guard.as_ref().unwrap()
                }),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{owner::Owner, traits::Set};
    use futures::FutureExt;

    fn emptied() -> ArcAsyncDerived<u32> {
        _ = crate::executor::Executor::init_tokio();
        let derived = ArcAsyncDerived::new_mock(|| async { 1 });
        derived.set(None);
        derived
    }

    /// `set(None)` empties a loaded value: awaiting it used to panic (an `unwrap` of the
    /// missing value). It waits for the next value.
    #[test]
    fn awaiting_an_emptied_value_waits_for_the_next_one() {
        let owner = Owner::new();
        owner.set();
        let derived = emptied();

        assert_eq!(derived.clone().into_future().now_or_never(), None);
    }

    /// The same by reference: the guard it gave panicked when read.
    #[test]
    fn awaiting_an_emptied_value_by_reference_waits_for_the_next_one() {
        let owner = Owner::new();
        owner.set();
        let derived = emptied();

        assert_eq!(derived.by_ref().now_or_never().map(|value| *value), None);
    }
}
