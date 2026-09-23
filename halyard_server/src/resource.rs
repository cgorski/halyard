use crate::{
    error::{warn_disposed, DisposedUse},
    FromEncodedStr, IntoEncodedString,
};
#[cfg(feature = "rkyv")]
use codee::binary::RkyvCodec;
#[cfg(feature = "serde-wasm-bindgen")]
use codee::string::JsonSerdeWasmCodec;
#[cfg(feature = "miniserde")]
use codee::string::MiniserdeCodec;
#[cfg(feature = "serde-lite")]
use codee::SerdeLite;
use codee::{
    string::{FromToStringCodec, JsonSerdeCodec},
    Decoder, Encoder,
};
use core::{fmt::Debug, marker::PhantomData};
use futures::Future;
use halyard_hydration_context::{SerializedDataId, SharedContext};
use halyard_reactive_graph::{
    computed::{
        ArcAsyncDerived, ArcMemo, AsyncDerived, AsyncDerivedFuture,
        AsyncDerivedRefFuture,
    },
    graph::{Source, ToAnySubscriber},
    owner::Owner,
    prelude::*,
    signal::{ArcRwSignal, RwSignal},
};
use std::{
    future::{pending, IntoFuture},
    ops::{Deref, DerefMut},
    panic::Location,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub(crate) static IS_SUPPRESSING_RESOURCE_LOAD: AtomicBool =
    AtomicBool::new(false);

/// Used to prevent resources from actually loading, in environments (like server route generation)
/// where they are not needed.
pub struct SuppressResourceLoad;

impl SuppressResourceLoad {
    /// Prevents resources from loading until this is dropped.
    pub fn new() -> Self {
        IS_SUPPRESSING_RESOURCE_LOAD.store(true, Ordering::Relaxed);
        Self
    }
}

impl Default for SuppressResourceLoad {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SuppressResourceLoad {
    fn drop(&mut self) {
        IS_SUPPRESSING_RESOURCE_LOAD.store(false, Ordering::Relaxed);
    }
}

/// A reference-counted asynchronous resource.
///
/// Resources allow asynchronously loading data and serializing it from the server to the client,
/// so that it loads on the server, and is then deserialized on the client. This improves
/// performance by beginning data loading on the server when the request is made, rather than
/// beginning it on the client after WASM has been loaded.
///
/// You can access the value of the resource either synchronously using `.get()` or asynchronously
/// using `.await`.
pub struct ArcResource<T, Ser = JsonSerdeCodec> {
    ser: PhantomData<Ser>,
    refetch: ArcRwSignal<usize>,
    data: ArcAsyncDerived<T>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T, Ser> Debug for ArcResource<T, Ser> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("ArcResource");
        d.field("ser", &self.ser).field("data", &self.data);
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        d.field("defined_at", self.defined_at);
        d.finish_non_exhaustive()
    }
}

impl<T, Ser> From<ArcResource<T, Ser>> for Resource<T, Ser>
where
    T: Send + Sync,
{
    #[track_caller]
    fn from(arc_resource: ArcResource<T, Ser>) -> Self {
        Resource {
            ser: PhantomData,
            data: arc_resource.data.into(),
            refetch: arc_resource.refetch.into(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
        }
    }
}

impl<T, Ser> From<Resource<T, Ser>> for ArcResource<T, Ser>
where
    T: Send + Sync,
{
    #[track_caller]
    fn from(resource: Resource<T, Ser>) -> Self {
        if resource.data.is_disposed() || resource.refetch.is_disposed() {
            warn_disposed(
                DisposedUse::IntoArc,
                Location::caller(),
                resource.defined_at(),
            );
            return ArcResource {
                ser: PhantomData,
                data: never_loads(),
                refetch: ArcRwSignal::new(0),
                #[cfg(any(debug_assertions, halyard_debuginfo))]
                defined_at: Location::caller(),
            };
        }
        ArcResource {
            ser: PhantomData,
            data: resource.data.into(),
            refetch: resource.refetch.into(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
        }
    }
}

impl<T, Ser> DefinedAt for ArcResource<T, Ser> {
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

impl<T, Ser> Clone for ArcResource<T, Ser> {
    fn clone(&self) -> Self {
        Self {
            ser: self.ser,
            refetch: self.refetch.clone(),
            data: self.data.clone(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
        }
    }
}

impl<T, Ser> Deref for ArcResource<T, Ser> {
    type Target = ArcAsyncDerived<T>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, Ser> Track for ArcResource<T, Ser>
where
    T: 'static,
{
    fn track(&self) {
        self.data.track();
    }
}

impl<T, Ser> Notify for ArcResource<T, Ser>
where
    T: 'static,
{
    fn notify(&self) {
        self.data.notify()
    }
}

impl<T, Ser> Write for ArcResource<T, Ser>
where
    T: 'static,
{
    type Value = Option<T>;

    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>> {
        self.data.try_write()
    }

    fn try_write_untracked(
        &self,
    ) -> Option<impl DerefMut<Target = Self::Value>> {
        self.data.try_write_untracked()
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static RESOURCE_SOURCE_SIGNAL_ACTIVE: AtomicBool = const { AtomicBool::new(false) };
}

#[cfg(debug_assertions)]
/// Returns whether the current thread is currently running a resource source signal.
pub fn in_resource_source_signal() -> bool {
    RESOURCE_SOURCE_SIGNAL_ACTIVE
        .with(|scope| scope.load(std::sync::atomic::Ordering::Relaxed))
}

/// Set a static to true whilst running the given function.
/// [`is_in_effect_scope`] will return true whilst the function is running.
fn run_in_resource_source_signal<T>(fun: impl FnOnce() -> T) -> T {
    #[cfg(debug_assertions)]
    {
        // For the theoretical nested case, set back to initial value rather than false:
        let initial = RESOURCE_SOURCE_SIGNAL_ACTIVE.with(|scope| {
            scope.swap(true, std::sync::atomic::Ordering::Relaxed)
        });
        let result = fun();
        RESOURCE_SOURCE_SIGNAL_ACTIVE.with(|scope| {
            scope.store(initial, std::sync::atomic::Ordering::Relaxed)
        });
        result
    }
    #[cfg(not(debug_assertions))]
    {
        fun()
    }
}

impl<T, Ser> ReadUntracked for ArcResource<T, Ser>
where
    T: 'static,
{
    type Value = <ArcAsyncDerived<T> as ReadUntracked>::Value;

    #[track_caller]
    fn try_read_untracked(&self) -> Option<Self::Value> {
        #[cfg(all(feature = "hydration", debug_assertions))]
        {
            use halyard_reactive_graph::{
                computed::suspense::SuspenseContext, effect::in_effect_scope,
                owner::use_context,
            };
            if !in_effect_scope()
                && !in_resource_source_signal()
                && use_context::<SuspenseContext>().is_none()
            {
                let location = std::panic::Location::caller();
                halyard_reactive_graph::log_warning(format_args!(
                    "At {location}, you are reading a resource in `hydrate` \
                     mode outside a <Suspense/> or <Transition/> or effect. \
                     This can cause hydration mismatch errors and loses out \
                     on a significant performance optimization. To fix this \
                     issue, you can either: \n1. Wrap the place where you \
                     read the resource in a <Suspense/> or <Transition/> \
                     component, or \n2. Switch to using \
                     ArcLocalResource::new(), which will wait to load the \
                     resource until the app is hydrated on the client side. \
                     (This will have worse performance in most cases.)",
                ));
            }
        }
        self.data.try_read_untracked()
    }
}

impl<T, Ser> ArcResource<T, Ser>
where
    Ser: Encoder<T> + Decoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Error: Debug,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding `Ser`.
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// If `blocking` is `true`, this is a blocking resource.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_with_options<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
        #[allow(unused)] // this is used with `feature = "ssr"`
        blocking: bool,
    ) -> ArcResource<T, Ser>
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let created_at = Location::caller();
        let shared_context = Owner::current_shared_context();
        let id = shared_context
            .as_ref()
            .map(|sc| sc.next_id())
            .unwrap_or_default();

        let initial =
            initial_value::<T, Ser>(&id, shared_context.as_ref(), created_at);
        let is_ready = initial.is_some();

        let refetch = ArcRwSignal::new(0);
        let source = ArcMemo::new({
            let refetch = refetch.clone();
            move |_| (refetch.get(), run_in_resource_source_signal(&source))
        });
        let fun = {
            let source = source.clone();
            move || {
                let (_, source) = source.get();
                let fut = fetcher(source);
                async move {
                    if IS_SUPPRESSING_RESOURCE_LOAD.load(Ordering::Relaxed) {
                        pending().await
                    } else {
                        fut.await
                    }
                }
            }
        };

        let data = ArcAsyncDerived::new_with_manual_dependencies(
            initial, fun, &source,
        );
        if is_ready {
            source.with_untracked(|_| ());
            source.add_subscriber(data.to_any_subscriber());
        }

        #[cfg(feature = "ssr")]
        if let Some(shared_context) = shared_context {
            let value = data.clone();
            let ready_fut = data.ready();

            if blocking {
                shared_context.defer_stream(Box::pin(data.ready()));
            }

            if shared_context.get_is_hydrating() {
                use crate::hydration_data::{encode, for_the_page};

                let for_id = id.clone();
                shared_context.write_async(
                    id,
                    Box::pin(async move {
                        ready_fut.await;
                        // a value that cannot be sent (unserializable, or cleared after it
                        // loaded) is left out and logged: the browser loads it itself
                        let encoded = value
                            .try_with_untracked(|value| {
                                encode::<T, Ser>(
                                    value.as_ref(),
                                    &for_id,
                                    created_at,
                                )
                            })
                            .unwrap_or_else(|| {
                                encode::<T, Ser>(None, &for_id, created_at)
                            });
                        for_the_page(encoded)
                    }),
                );
            }
        }

        ArcResource {
            ser: PhantomData,
            data,
            refetch,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: created_at,
        }
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    #[track_caller]
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U>
    where
        T: Send + Sync + 'static,
    {
        self.data.try_with(|n| n.as_ref().map(f))?
    }

    /// Re-runs the async function with the current source data.
    pub fn refetch(&self) {
        // wrapping, not saturating: every refetch must change the value the source compares
        self.refetch.try_update(|n| *n = n.wrapping_add(1));
    }
}

/// A resource's value from the server, read from the page while hydrating: `None` if the
/// page has none that can be used for it (the reason is logged), and the resource then
/// loads in the browser.
#[inline(always)]
#[allow(unused)]
pub(crate) fn initial_value<T, Ser>(
    id: &SerializedDataId,
    shared_context: Option<&Arc<dyn SharedContext + Send + Sync>>,
    created_at: &'static Location<'static>,
) -> Option<T>
where
    Ser: Encoder<T> + Decoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Error: Debug,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
{
    #[cfg(feature = "hydration")]
    {
        // no data at all is not an error: e.g. a resource created after hydration
        let data = shared_context?.read_data(id)?;
        match crate::hydration_data::decode::<T, Ser>(&data, id, created_at) {
            Ok(value) => return Some(value),
            Err(error) => crate::error::warn(&error),
        }
    }
    None
}

/// A resource that never loads: what awaiting or converting a resource whose reactive owner
/// is gone gives (docs/no-panics.md: a value that is gone is "do nothing", not a panic).
pub(crate) fn never_loads<T: 'static>() -> ArcAsyncDerived<T> {
    ArcAsyncDerived::new_mock(pending::<T>)
}

impl<T, E, Ser> ArcResource<Result<T, E>, Ser>
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
    /// This is useful when used with features like server functions, in conjunction
    /// with `<ErrorBoundary/>` and `<Suspense/>`, when these other components are
    /// left to handle the `None` and `Err(_)` states.
    #[track_caller]
    pub fn and_then<U>(&self, f: impl FnOnce(&T) -> U) -> Option<Result<U, E>> {
        self.map(|data| data.as_ref().map(f).map_err(|e| e.clone()))
    }
}

impl<T> ArcResource<T, JsonSerdeCodec>
where
    JsonSerdeCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeCodec as Encoder<T>>::Error: Debug,
    <JsonSerdeCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <JsonSerdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`JsonSerdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`JsonSerdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}

impl<T> ArcResource<T, FromToStringCodec>
where
    FromToStringCodec: Encoder<T> + Decoder<T>,
    <FromToStringCodec as Encoder<T>>::Error: Debug, <FromToStringCodec as Decoder<T>>::Error: Debug,
    <<FromToStringCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <FromToStringCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <FromToStringCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`FromToStringCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    pub fn new_str<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`FromToStringCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_str_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "serde-wasm-bindgen")]
impl<T> ArcResource<T, JsonSerdeWasmCodec>
where
    JsonSerdeWasmCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeWasmCodec as Encoder<T>>::Error: Debug, <JsonSerdeWasmCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeWasmCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <JsonSerdeWasmCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeWasmCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`JsonSerdeWasmCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new_serde_wb<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`JsonSerdeWasmCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_serde_wb_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}
#[cfg(feature = "miniserde")]
impl<T> ArcResource<T, MiniserdeCodec>
where
    MiniserdeCodec: Encoder<T> + Decoder<T>,
    <MiniserdeCodec as Encoder<T>>::Error: Debug,
    <MiniserdeCodec as Decoder<T>>::Error: Debug,
    <<MiniserdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <MiniserdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <MiniserdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`MiniserdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new_miniserde<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`MiniserdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_miniserde_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "serde-lite")]
impl<T> ArcResource<T, SerdeLite<JsonSerdeCodec>>
where
    SerdeLite<JsonSerdeCodec>: Encoder<T> + Decoder<T>,
    <SerdeLite<JsonSerdeCodec> as Encoder<T>>::Error: Debug, <SerdeLite<JsonSerdeCodec> as Decoder<T>>::Error: Debug,
    <<SerdeLite<JsonSerdeCodec> as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <SerdeLite<JsonSerdeCodec> as Encoder<T>>::Encoded: IntoEncodedString,
    <SerdeLite<JsonSerdeCodec> as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`SerdeLite`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new_serde_lite<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`SerdeLite`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_serde_lite_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "rkyv")]
#[cfg_attr(docsrs, doc(cfg(feature = "rkyv")))]
impl<T> ArcResource<T, RkyvCodec>
where
    RkyvCodec: Encoder<T> + Decoder<T>,
    <RkyvCodec as Encoder<T>>::Error: Debug,
    <RkyvCodec as Decoder<T>>::Error: Debug,
    <<RkyvCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <RkyvCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <RkyvCodec as Decoder<T>>::Encoded: FromEncodedStr,
{
    /// Creates a new resource with the encoding [`RkyvCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new_rkyv<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`RkyvCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_rkyv_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        ArcResource::new_with_options(source, fetcher, true)
    }
}

impl<T, Ser> IntoFuture for ArcResource<T, Ser>
where
    T: Clone + 'static,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    fn into_future(self) -> Self::IntoFuture {
        self.data.into_future()
    }
}

impl<T, Ser> ArcResource<T, Ser>
where
    T: 'static,
{
    /// Returns a new [`Future`] that is ready when the resource has loaded, and accesses its inner
    /// value by reference.
    pub fn by_ref(&self) -> AsyncDerivedRefFuture<T> {
        self.data.by_ref()
    }
}

/// An asynchronous resource.
///
/// Resources allow asynchronously loading data and serializing it from the server to the client,
/// so that it loads on the server, and is then deserialized on the client. This improves
/// performance by beginning data loading on the server when the request is made, rather than
/// beginning it on the client after WASM has been loaded.
///
/// You can access the value of the resource either synchronously using `.get()` or asynchronously
/// using `.await`.
pub struct Resource<T, Ser = JsonSerdeCodec>
where
    T: Send + Sync + 'static,
{
    ser: PhantomData<Ser>,
    data: AsyncDerived<T>,
    refetch: RwSignal<usize>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T, Ser> Debug for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("ArcResource");
        d.field("ser", &self.ser).field("data", &self.data);
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        d.field("defined_at", self.defined_at);
        d.finish_non_exhaustive()
    }
}

impl<T, Ser> DefinedAt for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
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

impl<T: Send + Sync + 'static, Ser> Copy for Resource<T, Ser> {}

impl<T: Send + Sync + 'static, Ser> Clone for Resource<T, Ser> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Ser> Deref for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    type Target = AsyncDerived<T>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, Ser> Track for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    fn track(&self) {
        self.data.track();
    }
}

impl<T, Ser> Notify for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    fn notify(&self) {
        self.data.notify()
    }
}

impl<T, Ser> Write for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    type Value = Option<T>;

    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>> {
        self.data.try_write()
    }

    fn try_write_untracked(
        &self,
    ) -> Option<impl DerefMut<Target = Self::Value>> {
        self.data.try_write_untracked()
    }
}

impl<T, Ser> ReadUntracked for Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    type Value = <AsyncDerived<T> as ReadUntracked>::Value;

    #[track_caller]
    fn try_read_untracked(&self) -> Option<Self::Value> {
        #[cfg(all(feature = "hydration", debug_assertions))]
        {
            use halyard_reactive_graph::{
                computed::suspense::SuspenseContext, effect::in_effect_scope,
                owner::use_context,
            };
            if !in_effect_scope()
                && !in_resource_source_signal()
                && use_context::<SuspenseContext>().is_none()
            {
                let location = std::panic::Location::caller();
                halyard_reactive_graph::log_warning(format_args!(
                    "At {location}, you are reading a resource in `hydrate` \
                     mode outside a <Suspense/> or <Transition/> or effect. \
                     This can cause hydration mismatch errors and loses out \
                     on a significant performance optimization. To fix this \
                     issue, you can either: \n1. Wrap the place where you \
                     read the resource in a <Suspense/> or <Transition/> \
                     component, or \n2. Switch to using LocalResource::new(), \
                     which will wait to load the resource until the app is \
                     hydrated on the client side. (This will have worse \
                     performance in most cases.)",
                ));
            }
        }
        self.data.try_read_untracked()
    }
}

impl<T> Resource<T, FromToStringCodec>
where
    FromToStringCodec: Encoder<T> + Decoder<T>,
    <FromToStringCodec as Encoder<T>>::Error: Debug, <FromToStringCodec as Decoder<T>>::Error: Debug,
    <<FromToStringCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <FromToStringCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <FromToStringCodec as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`FromToStringCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new_str<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`FromToStringCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_str_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

impl<T> Resource<T, JsonSerdeCodec>
where
    JsonSerdeCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeCodec as Encoder<T>>::Error: Debug,
    <JsonSerdeCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <JsonSerdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`JsonSerdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    #[track_caller]
    pub fn new<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`JsonSerdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "serde-wasm-bindgen")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde-wasm-bindgen")))]
impl<T> Resource<T, JsonSerdeWasmCodec>
where
    JsonSerdeWasmCodec: Encoder<T> + Decoder<T>,
    <JsonSerdeWasmCodec as Encoder<T>>::Error: Debug, <JsonSerdeWasmCodec as Decoder<T>>::Error: Debug,
    <<JsonSerdeWasmCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <JsonSerdeWasmCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <JsonSerdeWasmCodec as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`JsonSerdeWasmCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    pub fn new_serde_wb<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`JsonSerdeWasmCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_serde_wb_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "miniserde")]
#[cfg_attr(docsrs, doc(cfg(feature = "miniserde")))]
impl<T> Resource<T, MiniserdeCodec>
where
    MiniserdeCodec: Encoder<T> + Decoder<T>,
    <MiniserdeCodec as Encoder<T>>::Error: Debug,
    <MiniserdeCodec as Decoder<T>>::Error: Debug,
    <<MiniserdeCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <MiniserdeCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <MiniserdeCodec as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`MiniserdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    pub fn new_miniserde<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`MiniserdeCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_miniserde_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "serde-lite")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde-lite")))]
impl<T> Resource<T, SerdeLite<JsonSerdeCodec>>
where
    SerdeLite<JsonSerdeCodec>: Encoder<T> + Decoder<T>,
    <SerdeLite<JsonSerdeCodec> as Encoder<T>>::Error: Debug, <SerdeLite<JsonSerdeCodec> as Decoder<T>>::Error: Debug,
    <<SerdeLite<JsonSerdeCodec> as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <SerdeLite<JsonSerdeCodec> as Encoder<T>>::Encoded: IntoEncodedString,
    <SerdeLite<JsonSerdeCodec> as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`SerdeLite`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    pub fn new_serde_lite<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`SerdeLite`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_serde_lite_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

#[cfg(feature = "rkyv")]
#[cfg_attr(docsrs, doc(cfg(feature = "rkyv")))]
impl<T> Resource<T, RkyvCodec>
where
    RkyvCodec: Encoder<T> + Decoder<T>,
    <RkyvCodec as Encoder<T>>::Error: Debug,
    <RkyvCodec as Decoder<T>>::Error: Debug,
    <<RkyvCodec as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <RkyvCodec as Encoder<T>>::Encoded: IntoEncodedString,
    <RkyvCodec as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding [`RkyvCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    pub fn new_rkyv<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, false)
    }

    /// Creates a new blocking resource with the encoding [`RkyvCodec`].
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    pub fn new_rkyv_blocking<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        S: PartialEq + Clone + Send + Sync + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        Resource::new_with_options(source, fetcher, true)
    }
}

impl<T, Ser> Resource<T, Ser>
where
    Ser: Encoder<T> + Decoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Error: Debug,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
    T: Send + Sync,
{
    /// Creates a new resource with the encoding `Ser`.
    ///
    /// This takes a `source` function and a `fetcher`. The resource memoizes and reactively tracks
    /// the value returned by `source`. Whenever that value changes, it will run the `fetcher` to
    /// generate a new [`Future`] to load data.
    ///
    /// On creation, if you are on the server, this will run the `fetcher` once to generate
    /// a `Future` whose value will be serialized from the server to the client. If you are on
    /// the client, the initial value will be deserialized without re-running that async task.
    ///
    /// If `blocking` is `true`, this is a blocking resource.
    ///
    /// Blocking resources prevent any of the HTTP response from being sent until they have loaded.
    /// This is useful if you need their data to set HTML document metadata or information that
    /// needs to appear in HTTP headers.
    #[track_caller]
    pub fn new_with_options<S, Fut>(
        source: impl Fn() -> S + Send + Sync + 'static,
        fetcher: impl Fn(S) -> Fut + Send + Sync + 'static,
        blocking: bool,
    ) -> Resource<T, Ser>
    where
        S: Send + Sync + Clone + PartialEq + 'static,
        T: Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let ArcResource { data, refetch, .. }: ArcResource<T, Ser> =
            ArcResource::new_with_options(source, fetcher, blocking);
        Resource {
            ser: PhantomData,
            data: data.into(),
            refetch: refetch.into(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
        }
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U> {
        self.data
            .try_with(|n| n.as_ref().map(|n| Some(f(n))))?
            .flatten()
    }

    /// Re-runs the async function with the current source data.
    pub fn refetch(&self) {
        // wrapping, not saturating: every refetch must change the value the source compares
        self.refetch.try_update(|n| *n = n.wrapping_add(1));
    }
}

impl<T, E, Ser> Resource<Result<T, E>, Ser>
where
    Ser: Encoder<Result<T, E>> + Decoder<Result<T, E>>,
    <Ser as Encoder<Result<T, E>>>::Error: Debug,
    <Ser as Decoder<Result<T, E>>>::Error: Debug,
    <<Ser as Decoder<Result<T, E>>>::Encoded as FromEncodedStr>::DecodingError:
        Debug,
    <Ser as Encoder<Result<T, E>>>::Encoded: IntoEncodedString,
    <Ser as Decoder<Result<T, E>>>::Encoded: FromEncodedStr,
    T: Send + Sync,
    E: Send + Sync + Clone,
{
    /// Applies the given function when a resource that returns `Result<T, E>`
    /// has resolved and loaded an `Ok(_)`, rather than requiring nested `.map()`
    /// calls over the `Option<Result<_, _>>` returned by the resource.
    ///
    /// This is useful when used with features like server functions, in conjunction
    /// with `<ErrorBoundary/>` and `<Suspense/>`, when these other components are
    /// left to handle the `None` and `Err(_)` states.
    #[track_caller]
    pub fn and_then<U>(&self, f: impl FnOnce(&T) -> U) -> Option<Result<U, E>> {
        self.map(|data| data.as_ref().map(f).map_err(|e| e.clone()))
    }
}

impl<T, Ser> IntoFuture for Resource<T, Ser>
where
    T: Clone + Send + Sync + 'static,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    /// If the resource's owner is gone, so is its value: the future never finishes (and
    /// whatever awaits it is dropped with its own owner).
    #[track_caller]
    fn into_future(self) -> Self::IntoFuture {
        if self.data.is_disposed() {
            warn_disposed(
                DisposedUse::Await,
                Location::caller(),
                self.defined_at(),
            );
            return never_loads().into_future();
        }
        self.data.into_future()
    }
}

impl<T, Ser> Resource<T, Ser>
where
    T: Send + Sync + 'static,
{
    /// Returns a new [`Future`] that is ready when the resource has loaded, and accesses its inner
    /// value by reference.
    ///
    /// If the resource's owner is gone, so is its value, and the future never finishes.
    #[track_caller]
    pub fn by_ref(&self) -> AsyncDerivedRefFuture<T> {
        if self.data.is_disposed() {
            warn_disposed(
                DisposedUse::Await,
                Location::caller(),
                self.defined_at(),
            );
            return never_loads().by_ref();
        }
        self.data.by_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_executor;
    use futures::FutureExt;

    /// `refetch` counts up: at `usize::MAX` it used to overflow, a panic in debug builds.
    /// It wraps, which still changes the value the source compares, so it still refetches.
    #[test]
    fn arc_resource_refetch_wraps_instead_of_overflowing() {
        init_executor();
        let resource = ArcResource::new(|| (), |()| async { 1_u32 });
        resource.refetch.set(usize::MAX);

        resource.refetch();

        assert_eq!(resource.refetch.get_untracked(), 0);
    }

    #[test]
    fn resource_refetch_wraps_instead_of_overflowing() {
        init_executor();
        let owner = Owner::new();
        let resource =
            owner.with(|| Resource::new(|| (), |()| async { 1_u32 }));
        resource.refetch.set(usize::MAX);

        resource.refetch();

        assert_eq!(resource.refetch.get_untracked(), 0);
    }

    /// A `Suspend` (or a task) can outlive the owner of the resource it awaits, e.g. on a
    /// page being left. Awaiting the resource then used to panic ("Tried to access a
    /// reactive value that has already been disposed"). Its value is gone, so the await
    /// never finishes, and whatever was waiting is dropped with its own owner.
    #[test]
    fn awaiting_a_disposed_resource_stays_pending() {
        init_executor();
        let owner = Owner::new();
        let resource =
            owner.with(|| Resource::new(|| (), |()| async { 1_u32 }));
        owner.cleanup();

        assert!(resource.into_future().now_or_never().is_none());
    }

    #[test]
    fn awaiting_a_disposed_resource_by_reference_stays_pending() {
        init_executor();
        let owner = Owner::new();
        let resource =
            owner.with(|| Resource::new(|| (), |()| async { 1_u32 }));
        owner.cleanup();

        assert!(resource.by_ref().now_or_never().is_none());
    }

    /// Converting a disposed resource into an `ArcResource` used to panic too. It becomes
    /// a resource that never loads.
    #[test]
    fn a_disposed_resource_converts_to_an_arc_resource_that_never_loads() {
        init_executor();
        let owner = Owner::new();
        let resource =
            owner.with(|| Resource::new(|| (), |()| async { 1_u32 }));
        owner.cleanup();

        let arc = ArcResource::from(resource);

        assert_eq!(arc.get_untracked(), None);
        assert!(arc.into_future().now_or_never().is_none());
    }

    /// On the server, each resource's value is sent to the browser in the page.
    #[cfg(feature = "ssr")]
    mod server {
        use super::*;
        use crate::{
            hydration_data::NOT_SENT,
            test_support::{page_data, server_request},
        };
        use std::collections::HashMap;

        /// What the page carries for resource 0.
        fn sent_for_resource_0(value: &str) -> String {
            format!("__RESOLVED_RESOURCES[0] = {value:?};")
        }

        #[test]
        fn a_value_is_sent_in_the_page() {
            init_executor();
            let (owner, context) = server_request();
            let _resource =
                owner.with(|| ArcResource::new(|| (), |()| async { 1_u32 }));

            let data = page_data(&context);

            assert!(data.contains(&sent_for_resource_0("1")), "{data}");
        }

        /// JSON object keys must be strings, so this map cannot be serialized. That used
        /// to panic while the page's data was streamed, failing the response. The page
        /// says that no value was sent, and the browser loads the resource itself.
        #[test]
        fn a_value_that_cannot_be_serialized_is_left_out_of_the_page() {
            init_executor();
            let (owner, context) = server_request();
            let _resource = owner.with(|| {
                ArcResource::new(
                    || (),
                    |()| async { HashMap::from([((1_u8, 2_u8), 3_u8)]) },
                )
            });

            let data = page_data(&context);

            assert!(data.contains(&sent_for_resource_0(NOT_SENT)), "{data}");
        }

        /// A resource cleared after it loaded (`set(None)`) has no value to send: that
        /// was an `unreachable!()`. It is left out of the page in the same way.
        #[test]
        fn a_resource_without_a_value_is_left_out_of_the_page() {
            init_executor();
            let (owner, context) = server_request();
            let resource =
                owner.with(|| ArcResource::new(|| (), |()| async { 1_u32 }));
            resource.try_update(|value| *value = None);

            let data = page_data(&context);

            assert!(data.contains(&sent_for_resource_0(NOT_SENT)), "{data}");
        }
    }
}
