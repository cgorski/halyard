use crate::server::{
    error::{warn_disposed, DisposedUse},
    initial_value, FromEncodedStr, IntoEncodedString,
    IS_SUPPRESSING_RESOURCE_LOAD,
};
use codee::{
    string::{FromToStringCodec, JsonSerdeCodec},
    Decoder, Encoder,
};
use core::{fmt::Debug, marker::PhantomData};
use futures::{Future, FutureExt};
use halyard_reactive_graph::or_poisoned::OrPoisoned;
use halyard_reactive_graph::{
    computed::{
        suspense::SuspenseContext, AsyncDerivedReadyFuture, ScopedFuture,
    },
    diagnostics::{SpecialNonReactiveFuture, SpecialNonReactiveZone},
    graph::{AnySource, ToAnySource},
    owner::{use_context, ArenaItem, Owner},
    prelude::*,
    signal::{
        guards::{Plain, ReadGuard},
        ArcTrigger,
    },
};
use std::{
    future::IntoFuture,
    mem,
    panic::Location,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    task::{Context, Poll, Waker},
};

/// A reference-counted resource that only loads once.
///
/// Resources allow asynchronously loading data and serializing it from the server to the client,
/// so that it loads on the server, and is then deserialized on the client. This improves
/// performance by beginning data loading on the server when the request is made, rather than
/// beginning it on the client after WASM has been loaded.
///
/// You can access the value of the resource either synchronously using `.get()` or asynchronously
/// using `.await`.
#[derive(Debug)]
pub struct ArcOnceResource<T, Ser = JsonSerdeCodec> {
    trigger: ArcTrigger,
    value: Arc<RwLock<Option<T>>>,
    wakers: Arc<RwLock<Vec<Waker>>>,
    suspenses: Arc<RwLock<Vec<SuspenseContext>>>,
    loading: Arc<AtomicBool>,
    ser: PhantomData<fn() -> Ser>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T, Ser> Clone for ArcOnceResource<T, Ser> {
    fn clone(&self) -> Self {
        Self {
            trigger: self.trigger.clone(),
            value: self.value.clone(),
            wakers: self.wakers.clone(),
            suspenses: self.suspenses.clone(),
            loading: self.loading.clone(),
            ser: self.ser,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
        }
    }
}

impl<T, Ser> ArcOnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: Encoder<T> + Decoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Error: Debug,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding `Ser`. If `blocking` is `true`, this is a blocking
    /// resource.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_with_options(
        fut: impl Future<Output = T> + Send + 'static,
        #[allow(unused)] // this is used with `feature = "ssr"`
        blocking: bool,
    ) -> Self {
        let created_at = Location::caller();
        let shared_context = Owner::current_shared_context();
        let id = shared_context
            .as_ref()
            .map(|sc| sc.next_id())
            .unwrap_or_default();

        let initial =
            initial_value::<T, Ser>(&id, shared_context.as_ref(), created_at);
        let is_ready = initial.is_some();
        let value = Arc::new(RwLock::new(initial));
        let wakers = Arc::new(RwLock::new(Vec::<Waker>::new()));
        let suspenses = Arc::new(RwLock::new(Vec::<SuspenseContext>::new()));
        let loading = Arc::new(AtomicBool::new(!is_ready));
        let trigger = ArcTrigger::new();

        let fut = ScopedFuture::new(fut);

        if !is_ready && !IS_SUPPRESSING_RESOURCE_LOAD.load(Ordering::Relaxed) {
            let value = Arc::clone(&value);
            let wakers = Arc::clone(&wakers);
            let loading = Arc::clone(&loading);
            let trigger = trigger.clone();
            halyard_reactive_graph::spawn(async move {
                let loaded = fut.await;
                *value.write().or_poisoned() = Some(loaded);
                loading.store(false, Ordering::Relaxed);
                for waker in mem::take(&mut *wakers.write().or_poisoned()) {
                    waker.wake();
                }
                trigger.notify();
            });
        }

        let data = Self {
            trigger,
            value: value.clone(),
            loading,
            wakers,
            suspenses,
            ser: PhantomData,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: created_at,
        };

        #[cfg(feature = "ssr")]
        if let Some(shared_context) = shared_context {
            let value = Arc::clone(&value);
            let ready_fut = data.ready();

            if blocking {
                shared_context.defer_stream(Box::pin(data.ready()));
            }

            if shared_context.get_is_hydrating() {
                use crate::server::hydration_data::{encode, for_the_page};

                let for_id = id.clone();
                shared_context.write_async(
                    id,
                    Box::pin(async move {
                        ready_fut.await;
                        // a value that cannot be serialized is left out and logged: the
                        // browser loads it itself
                        let value = value.read().or_poisoned();
                        for_the_page(encode::<T, Ser>(
                            value.as_ref(),
                            &for_id,
                            created_at,
                        ))
                    }),
                );
            }
        }

        data
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    #[track_caller]
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U>
    where
        T: Send + Sync + 'static,
    {
        self.try_with(|n| n.as_ref().map(f))?
    }
}

impl<T, E, Ser> ArcOnceResource<Result<T, E>, Ser>
where
    Ser: Encoder<Result<T, E>> + Decoder<Result<T, E>>,
    <Ser as Encoder<Result<T, E>>>::Error: Debug,
    <Ser as Decoder<Result<T, E>>>::Error: Debug,
    <<Ser as Decoder<Result<T, E>>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <Ser as Encoder<Result<T, E>>>::Encoded: IntoEncodedString,
    <Ser as Decoder<Result<T, E>>>::Encoded: FromEncodedStr,
    T: Send + Sync + 'static,
    E: Send + Sync + Clone + 'static,
{
    /// Applies the given function when a resource that returns `Result<T, E>`
    /// has resolved and loaded an `Ok(_)`, rather than requiring nested `.map()`
    /// calls over the `Option<Result<_, _>>` returned by the resource.
    ///
    /// This is useful for a fallible loader, in conjunction with `<ErrorBoundary/>` and
    /// `<Suspense/>`, when these other components are left to handle the `None` and
    /// `Err(_)` states.
    #[track_caller]
    pub fn and_then<U>(&self, f: impl FnOnce(&T) -> U) -> Option<Result<U, E>> {
        self.map(|data| data.as_ref().map(f).map_err(|e| e.clone()))
    }
}

impl<T, Ser> ArcOnceResource<T, Ser> {
    /// Returns a `Future` that is ready when this resource has next finished loading.
    pub fn ready(&self) -> AsyncDerivedReadyFuture {
        AsyncDerivedReadyFuture::new(
            self.to_any_source(),
            &self.loading,
            &self.wakers,
        )
    }
}

impl<T, Ser> DefinedAt for ArcOnceResource<T, Ser> {
    fn defined_at(&self) -> Option<&'static Location<'static>> {
        #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
        {
            None
        }
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        {
            Some(self.defined_at)
        }
    }
}

impl<T, Ser> IsDisposed for ArcOnceResource<T, Ser> {
    #[inline(always)]
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T, Ser> ToAnySource for ArcOnceResource<T, Ser> {
    fn to_any_source(&self) -> AnySource {
        self.trigger.to_any_source()
    }
}

impl<T, Ser> Track for ArcOnceResource<T, Ser> {
    fn track(&self) {
        self.trigger.track();
    }
}

impl<T, Ser> ReadUntracked for ArcOnceResource<T, Ser>
where
    T: 'static,
{
    type Value = ReadGuard<Option<T>, Plain<Option<T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        if let Some(suspense_context) = use_context::<SuspenseContext>() {
            if self.value.read().or_poisoned().is_none() {
                let handle = suspense_context.task_id();
                let mut ready =
                    Box::pin(SpecialNonReactiveFuture::new(self.ready()));
                match ready.as_mut().now_or_never() {
                    Some(_) => drop(handle),
                    None => {
                        halyard_reactive_graph::spawn(async move {
                            ready.await;
                            drop(handle);
                        });
                    }
                }
                self.suspenses.write().or_poisoned().push(suspense_context);
            }
        }
        Plain::try_new(Arc::clone(&self.value)).map(ReadGuard::new)
    }
}

impl<T, Ser> IntoFuture for ArcOnceResource<T, Ser>
where
    T: Clone + 'static,
{
    type Output = T;
    type IntoFuture = OnceResourceFuture<T>;

    fn into_future(self) -> Self::IntoFuture {
        OnceResourceFuture {
            source: self.to_any_source(),
            value: Arc::clone(&self.value),
            wakers: Arc::clone(&self.wakers),
            suspenses: Arc::clone(&self.suspenses),
        }
    }
}

/// A reactive source that never changes: what a resource whose owner is gone subscribes to.
fn never_changes() -> AnySource {
    ArcTrigger::new().to_any_source()
}

/// A [`Future`] that is ready when an
/// [`ArcAsyncDerived`](halyard_reactive_graph::computed::ArcAsyncDerived) is finished loading or reloading,
/// and contains its value. `.await`ing this clones the value `T`.
pub struct OnceResourceFuture<T> {
    source: AnySource,
    value: Arc<RwLock<Option<T>>>,
    wakers: Arc<RwLock<Vec<Waker>>>,
    suspenses: Arc<RwLock<Vec<SuspenseContext>>>,
}

impl<T> OnceResourceFuture<T> {
    /// A future for a resource whose value is gone (its owner was disposed): it never
    /// finishes.
    fn never() -> Self {
        Self {
            source: never_changes(),
            value: Arc::new(RwLock::new(None)),
            wakers: Arc::default(),
            suspenses: Arc::default(),
        }
    }
}

impl<T> Future for OnceResourceFuture<T>
where
    T: Clone + 'static,
{
    type Output = T;

    #[track_caller]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        let _guard = SpecialNonReactiveZone::enter();
        let waker = cx.waker();
        self.source.track();

        if let Some(suspense_context) = use_context::<SuspenseContext>() {
            self.suspenses.write().or_poisoned().push(suspense_context);
        }

        // Ready once there is a value (the loader stores it, then takes and wakes the
        // wakers), rather than once `loading` is cleared and then expecting a value.
        if let Some(value) = self.value.read().or_poisoned().clone() {
            return Poll::Ready(value);
        }
        self.wakers.write().or_poisoned().push(waker.clone());
        // a value stored between the check and the push would not wake this: look again
        if let Some(value) = self.value.read().or_poisoned().clone() {
            return Poll::Ready(value);
        }
        Poll::Pending
    }
}

impl<T> ArcOnceResource<T, JsonSerdeCodec>
where
    T: Send + Sync + 'static,
    JsonSerdeCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeCodec as Encoder<T>>::Error: Debug,
    <JsonSerdeCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <JsonSerdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a resource using [`JsonSerdeCodec`] for encoding/decoding the value.
    #[track_caller]
    pub fn new(fut: impl Future<Output = T> + Send + 'static) -> Self {
        ArcOnceResource::new_with_options(fut, false)
    }

    /// Creates a blocking resource using [`JsonSerdeCodec`] for encoding/decoding the value.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_blocking(fut: impl Future<Output = T> + Send + 'static) -> Self {
        ArcOnceResource::new_with_options(fut, true)
    }
}

impl<T> ArcOnceResource<T, FromToStringCodec>
where
T: Send + Sync + 'static,
    FromToStringCodec: Encoder<T> + Decoder<T>,
    <FromToStringCodec as Encoder<T>>::Error: Debug, <FromToStringCodec as Decoder<T>>::Error: Debug,
    <<FromToStringCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <FromToStringCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <FromToStringCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a resource using [`FromToStringCodec`] for encoding/decoding the value.
    pub fn new_str(
        fut: impl Future<Output = T> + Send + 'static
    ) -> Self
    {
        ArcOnceResource::new_with_options(fut, false)
    }

    /// Creates a blocking resource using [`FromToStringCodec`] for encoding/decoding the value.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_str_blocking(
        fut: impl Future<Output = T> + Send + 'static
    ) -> Self
    {
        ArcOnceResource::new_with_options(fut, true)
    }
}

/// A resource that only loads once.
///
/// Resources allow asynchronously loading data and serializing it from the server to the client,
/// so that it loads on the server, and is then deserialized on the client. This improves
/// performance by beginning data loading on the server when the request is made, rather than
/// beginning it on the client after WASM has been loaded.
///
/// You can access the value of the resource either synchronously using `.get()` or asynchronously
/// using `.await`.
#[derive(Debug)]
pub struct OnceResource<T, Ser = JsonSerdeCodec> {
    inner: ArenaItem<ArcOnceResource<T, Ser>>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T, Ser> Clone for OnceResource<T, Ser> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Ser> Copy for OnceResource<T, Ser> {}

impl<T, Ser> OnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: Encoder<T> + Decoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Error: Debug,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding `Ser`. If `blocking` is `true`, this is a blocking
    /// resource.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_with_options(
        fut: impl Future<Output = T> + Send + 'static,
        blocking: bool,
    ) -> Self {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        let defined_at = Location::caller();
        Self {
            inner: ArenaItem::new(ArcOnceResource::new_with_options(
                fut, blocking,
            )),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at,
        }
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U> {
        self.try_with(|n| n.as_ref().map(|n| Some(f(n))))?.flatten()
    }
}

impl<T, E, Ser> OnceResource<Result<T, E>, Ser>
where
    Ser: Encoder<Result<T, E>> + Decoder<Result<T, E>>,
    <Ser as Encoder<Result<T, E>>>::Error: Debug,
    <Ser as Decoder<Result<T, E>>>::Error: Debug,
    <<Ser as Decoder<Result<T, E>>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <Ser as Encoder<Result<T, E>>>::Encoded: IntoEncodedString,
    <Ser as Decoder<Result<T, E>>>::Encoded: FromEncodedStr,
    T: Send + Sync + 'static,
    E: Send + Sync + Clone + 'static,
{
    /// Applies the given function when a resource that returns `Result<T, E>`
    /// has resolved and loaded an `Ok(_)`, rather than requiring nested `.map()`
    /// calls over the `Option<Result<_, _>>` returned by the resource.
    ///
    /// This is useful for a fallible loader, in conjunction with `<ErrorBoundary/>` and
    /// `<Suspense/>`, when these other components are left to handle the `None` and
    /// `Err(_)` states.
    #[track_caller]
    pub fn and_then<U>(&self, f: impl FnOnce(&T) -> U) -> Option<Result<U, E>> {
        self.map(|data| data.as_ref().map(f).map_err(|e| e.clone()))
    }
}

impl<T, Ser> OnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: 'static,
{
    /// Returns a `Future` that is ready when this resource has next finished loading.
    ///
    /// If the resource's owner is gone, so is its value, and it is never ready.
    #[track_caller]
    pub fn ready(&self) -> AsyncDerivedReadyFuture {
        let used_at = Location::caller();
        self.inner
            .try_with_value(|inner| inner.ready())
            .unwrap_or_else(|| {
                warn_disposed(DisposedUse::Ready, used_at, self.defined_at());
                AsyncDerivedReadyFuture::new(
                    never_changes(),
                    &Arc::new(AtomicBool::new(true)),
                    &Arc::default(),
                )
            })
    }
}

impl<T, Ser> DefinedAt for OnceResource<T, Ser> {
    fn defined_at(&self) -> Option<&'static Location<'static>> {
        #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
        {
            None
        }
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        {
            Some(self.defined_at)
        }
    }
}

impl<T, Ser> IsDisposed for OnceResource<T, Ser> {
    #[inline(always)]
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T, Ser> ToAnySource for OnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: 'static,
{
    /// If the resource's owner is gone, this is a source that never changes.
    #[track_caller]
    fn to_any_source(&self) -> AnySource {
        let used_at = Location::caller();
        self.inner
            .try_with_value(|inner| inner.to_any_source())
            .unwrap_or_else(|| {
                warn_disposed(
                    DisposedUse::Subscribe,
                    used_at,
                    self.defined_at(),
                );
                never_changes()
            })
    }
}

impl<T, Ser> Track for OnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: 'static,
{
    fn track(&self) {
        if let Some(inner) = self.inner.try_get_value() {
            inner.track();
        }
    }
}

impl<T, Ser> ReadUntracked for OnceResource<T, Ser>
where
    T: Send + Sync + 'static,
    Ser: 'static,
{
    type Value = ReadGuard<Option<T>, Plain<Option<T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        self.inner
            .try_with_value(|inner| inner.try_read_untracked())
            .flatten()
    }
}

impl<T, Ser> IntoFuture for OnceResource<T, Ser>
where
    T: Clone + Send + Sync + 'static,
    Ser: 'static,
{
    type Output = T;
    type IntoFuture = OnceResourceFuture<T>;

    /// If the resource's owner is gone, so is its value, and the future never finishes.
    #[track_caller]
    fn into_future(self) -> Self::IntoFuture {
        let used_at = Location::caller();
        self.inner.try_get_value().map_or_else(
            || {
                warn_disposed(DisposedUse::Await, used_at, self.defined_at());
                OnceResourceFuture::never()
            },
            IntoFuture::into_future,
        )
    }
}

impl<T> OnceResource<T, JsonSerdeCodec>
where
    T: Send + Sync + 'static,
    JsonSerdeCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeCodec as Encoder<T>>::Error: Debug,
    <JsonSerdeCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <JsonSerdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a resource using [`JsonSerdeCodec`] for encoding/decoding the value.
    #[track_caller]
    pub fn new(fut: impl Future<Output = T> + Send + 'static) -> Self {
        OnceResource::new_with_options(fut, false)
    }

    /// Creates a blocking resource using [`JsonSerdeCodec`] for encoding/decoding the value.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_blocking(fut: impl Future<Output = T> + Send + 'static) -> Self {
        OnceResource::new_with_options(fut, true)
    }
}

impl<T> OnceResource<T, FromToStringCodec>
where
T: Send + Sync + 'static,
    FromToStringCodec: Encoder<T> + Decoder<T>,
    <FromToStringCodec as Encoder<T>>::Error: Debug, <FromToStringCodec as Decoder<T>>::Error: Debug,
    <<FromToStringCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <FromToStringCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <FromToStringCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a resource using [`FromToStringCodec`] for encoding/decoding the value.
    pub fn new_str(
        fut: impl Future<Output = T> + Send + 'static
    ) -> Self
    {
        OnceResource::new_with_options(fut, false)
    }

    /// Creates a blocking resource using [`FromToStringCodec`] for encoding/decoding the value.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_str_blocking(
        fut: impl Future<Output = T> + Send + 'static
    ) -> Self
    {
        OnceResource::new_with_options(fut, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::init_executor;
    use futures::executor::block_on;

    /// Awaiting the resource gives the value it loaded.
    #[test]
    fn awaiting_a_once_resource_gives_its_value() {
        init_executor();
        let resource = ArcOnceResource::new(async { 7_u32 });

        assert_eq!(block_on(resource.clone().into_future()), 7);
        assert_eq!(block_on(resource.into_future()), 7);
    }

    /// The resource's owner is gone: awaiting it, waiting for it to be ready and
    /// subscribing to it used to panic ("Tried to access a reactive value that has already
    /// been disposed"). Its value is gone, so the await never finishes.
    #[test]
    fn awaiting_a_disposed_once_resource_stays_pending() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| OnceResource::new(async { 7_u32 }));
        owner.cleanup();

        assert!(resource.into_future().now_or_never().is_none());
    }

    #[test]
    fn a_disposed_once_resource_is_never_ready() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| OnceResource::new(async { 7_u32 }));
        owner.cleanup();

        assert!(resource.ready().now_or_never().is_none());
    }

    #[test]
    fn a_disposed_once_resource_is_a_source_that_never_changes() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| OnceResource::new(async { 7_u32 }));
        owner.cleanup();

        let source = resource.to_any_source();

        source.track();
        assert_eq!(resource.try_get_untracked(), None);
    }

    /// On the server, the resource's value is sent to the browser in the page.
    #[cfg(feature = "ssr")]
    mod server {
        use super::*;
        use crate::server::{
            hydration_data::NOT_SENT,
            test_support::{page_data, server_request},
        };
        use std::collections::HashMap;

        fn sent_for_resource_0(value: &str) -> String {
            format!("__RESOLVED_RESOURCES[0] = {value:?};")
        }

        #[test]
        fn a_value_is_sent_in_the_page() {
            init_executor();
            let (owner, context) = server_request();
            let _resource =
                owner.with(|| ArcOnceResource::new(async { 7_u32 }));

            let data = page_data(&context);

            assert!(data.contains(&sent_for_resource_0("7")), "{data}");
        }

        /// JSON object keys must be strings, so this map cannot be serialized: that used to
        /// panic while the page's data was streamed, failing the response. The page says
        /// that no value was sent, and the browser loads the resource itself.
        #[test]
        fn a_value_that_cannot_be_serialized_is_left_out_of_the_page() {
            init_executor();
            let (owner, context) = server_request();
            let _resource = owner.with(|| {
                ArcOnceResource::new(async {
                    HashMap::from([((1_u8, 2_u8), 3_u8)])
                })
            });

            let data = page_data(&context);

            assert!(data.contains(&sent_for_resource_0(NOT_SENT)), "{data}");
        }
    }
}
