use crate::server::{
    error::{warn, warn_disposed, DisposedUse, ResourceError},
    resource::never_loads,
};
use halyard_reactive_graph::{
    computed::{
        suspense::LocalResourceNotifier, ArcAsyncDerived, AsyncDerived,
        AsyncDerivedFuture,
    },
    graph::{
        AnySource, AnySubscriber, ReactiveNode, Source, Subscriber,
        ToAnySource, ToAnySubscriber,
    },
    owner::use_context,
    send_wrapper_ext::SendOption,
    signal::{
        guards::{AsyncPlain, Mapped, ReadGuard},
        ArcRwSignal, RwSignal,
    },
    traits::{
        DefinedAt, IsDisposed, Notify, ReadUntracked, Track, UntrackableGuard,
        Update, With, Write,
    },
};
use std::{
    future::{pending, Future, IntoFuture},
    ops::{Deref, DerefMut},
    panic::Location,
};

/// A reference-counted resource that only loads its data locally on the client.
pub struct ArcLocalResource<T> {
    data: ArcAsyncDerived<T>,
    refetch: ArcRwSignal<usize>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T> Clone for ArcLocalResource<T> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            refetch: self.refetch.clone(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: self.defined_at,
        }
    }
}

impl<T> Deref for ArcLocalResource<T> {
    type Target = ArcAsyncDerived<T>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> ArcLocalResource<T> {
    /// Creates the resource.
    ///
    /// This will only begin loading data if you are on the client (i.e., if you do not have the
    /// `ssr` feature activated).
    #[track_caller]
    pub fn new<Fut>(fetcher: impl Fn() -> Fut + 'static) -> Self
    where
        T: 'static,
        Fut: Future<Output = T> + 'static,
    {
        let fetcher = move || {
            let fut = fetcher();
            async move {
                // in SSR mode, this will simply always be pending
                // if we try to read from it, we will trigger Suspense automatically to fall back
                // so this will never need to return anything
                if cfg!(feature = "ssr") {
                    pending().await
                } else {
                    // LocalResources that are immediately available can cause a hydration error,
                    // because the future *looks* like it is already ready (and therefore would
                    // already have been rendered to html on the server), but in fact was ignored
                    // on the server. the simplest way to avoid this is to ensure that we always
                    // wait a tick before resolving any value for a localresource.
                    halyard_reactive_graph::executor::Executor::tick().await;
                    fut.await
                }
            }
        };
        let refetch = ArcRwSignal::new(0);

        Self {
            data: if cfg!(feature = "ssr") {
                ArcAsyncDerived::new_mock(fetcher)
            } else {
                let refetch = refetch.clone();
                ArcAsyncDerived::new_unsync(move || {
                    refetch.track();
                    fetcher()
                })
            },
            refetch,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
        }
    }

    /// Re-runs the async function.
    pub fn refetch(&self) {
        // wrapping, not saturating: every refetch must change the value it tracks
        self.refetch.try_update(|n| *n = n.wrapping_add(1));
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    #[track_caller]
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U>
    where
        T: 'static,
    {
        self.data.try_with(|n| n.as_ref().map(f))?
    }
}

impl<T, E> ArcLocalResource<Result<T, E>>
where
    T: 'static,
    E: Clone + 'static,
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

/// Tells the enclosing `<Suspense/>` or `<Transition/>` that a local resource is being read,
/// so that on the server it renders its fallback and leaves the rest to the browser. On the
/// server, outside such a boundary, nothing can render in its place: that is logged, and
/// the await stays pending, as a local resource always is on the server.
///
/// This used to panic, but the panic did not save the response: it ended the task that was
/// awaiting (a resource's loader, a streamed `Suspend`), and the response waited for it all
/// the same. Awaiting `T` cannot answer with an error, so the log is the report.
fn notify_local_read(
    awaited_at: &'static Location<'static>,
    created_at: Option<&'static Location<'static>>,
) {
    if let Some(mut notifier) = use_context::<LocalResourceNotifier>() {
        notifier.notify();
    } else if cfg!(feature = "ssr") {
        warn(&ResourceError::LocalResourceAwaitedOnServer {
            awaited_at,
            created_at,
        });
    }
}

impl<T> IntoFuture for ArcLocalResource<T>
where
    T: Clone + 'static,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    /// On the server, this never finishes: local resources load only in the browser.
    #[track_caller]
    fn into_future(self) -> Self::IntoFuture {
        notify_local_read(Location::caller(), self.defined_at());
        self.data.into_future()
    }
}

impl<T> DefinedAt for ArcLocalResource<T> {
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

impl<T> Notify for ArcLocalResource<T>
where
    T: 'static,
{
    fn notify(&self) {
        self.data.notify()
    }
}

impl<T> Write for ArcLocalResource<T>
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

impl<T> ReadUntracked for ArcLocalResource<T>
where
    T: 'static,
{
    type Value =
        ReadGuard<Option<T>, Mapped<AsyncPlain<SendOption<T>>, Option<T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        if let Some(mut notifier) = use_context::<LocalResourceNotifier>() {
            notifier.notify();
        }
        self.data.try_read_untracked()
    }
}

impl<T: 'static> IsDisposed for ArcLocalResource<T> {
    #[inline(always)]
    fn is_disposed(&self) -> bool {
        false
    }
}

impl<T: 'static> ToAnySource for ArcLocalResource<T> {
    fn to_any_source(&self) -> AnySource {
        self.data.to_any_source()
    }
}

impl<T: 'static> ToAnySubscriber for ArcLocalResource<T> {
    fn to_any_subscriber(&self) -> AnySubscriber {
        self.data.to_any_subscriber()
    }
}

impl<T> Source for ArcLocalResource<T> {
    fn add_subscriber(&self, subscriber: AnySubscriber) {
        self.data.add_subscriber(subscriber)
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.data.remove_subscriber(subscriber);
    }

    fn clear_subscribers(&self) {
        self.data.clear_subscribers();
    }
}

impl<T> ReactiveNode for ArcLocalResource<T> {
    fn mark_dirty(&self) {
        self.data.mark_dirty();
    }

    fn mark_check(&self) {
        self.data.mark_check();
    }

    fn mark_subscribers_check(&self) {
        self.data.mark_subscribers_check();
    }

    fn update_if_necessary(&self) -> bool {
        self.data.update_if_necessary()
    }
}

impl<T> Subscriber for ArcLocalResource<T> {
    fn add_source(&self, source: AnySource) {
        self.data.add_source(source);
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        self.data.clear_sources(subscriber);
    }
}

/// A resource that only loads its data locally on the client.
pub struct LocalResource<T> {
    data: AsyncDerived<T>,
    refetch: RwSignal<usize>,
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    defined_at: &'static Location<'static>,
}

impl<T> Deref for LocalResource<T> {
    type Target = AsyncDerived<T>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> Clone for LocalResource<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for LocalResource<T> {}

impl<T> LocalResource<T> {
    /// Creates the resource.
    ///
    /// This will only begin loading data if you are on the client (i.e., if you do not have the
    /// `ssr` feature activated).
    #[track_caller]
    pub fn new<Fut>(fetcher: impl Fn() -> Fut + 'static) -> Self
    where
        T: 'static,
        Fut: Future<Output = T> + 'static,
    {
        let fetcher = move || {
            let fut = fetcher();
            async move {
                // in SSR mode, this will simply always be pending
                // if we try to read from it, we will trigger Suspense automatically to fall back
                // so this will never need to return anything
                if cfg!(feature = "ssr") {
                    pending().await
                } else {
                    // LocalResources that are immediately available can cause a hydration error,
                    // because the future *looks* like it is already ready (and therefore would
                    // already have been rendered to html on the server), but in fact was ignored
                    // on the server. the simplest way to avoid this is to ensure that we always
                    // wait a tick before resolving any value for a localresource.
                    halyard_reactive_graph::executor::Executor::tick().await;
                    fut.await
                }
            }
        };
        let refetch = RwSignal::new(0);

        Self {
            data: if cfg!(feature = "ssr") {
                AsyncDerived::new_mock(fetcher)
            } else {
                AsyncDerived::new_unsync_threadsafe_storage(move || {
                    refetch.track();
                    fetcher()
                })
            },
            refetch,
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: Location::caller(),
        }
    }

    /// Re-runs the async function.
    pub fn refetch(&self) {
        // wrapping, not saturating: every refetch must change the value it tracks
        self.refetch.try_update(|n| *n = n.wrapping_add(1));
    }

    /// Synchronously, reactively reads the current value of the resource and applies the function
    /// `f` to its value if it is `Some(_)`.
    #[track_caller]
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Option<U>
    where
        T: 'static,
    {
        self.data.try_with(|n| n.as_ref().map(f))?
    }
}

impl<T, E> LocalResource<Result<T, E>>
where
    T: 'static,
    E: Clone + 'static,
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

impl<T> IntoFuture for LocalResource<T>
where
    T: Clone + 'static,
{
    type Output = T;
    type IntoFuture = AsyncDerivedFuture<T>;

    /// On the server, this never finishes: local resources load only in the browser. If the
    /// resource's owner is gone, so is its value, and it never finishes either.
    #[track_caller]
    fn into_future(self) -> Self::IntoFuture {
        let awaited_at = Location::caller();
        notify_local_read(awaited_at, self.defined_at());
        if self.data.is_disposed() {
            warn_disposed(DisposedUse::Await, awaited_at, self.defined_at());
            return never_loads().into_future();
        }
        self.data.into_future()
    }
}

impl<T> DefinedAt for LocalResource<T> {
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

impl<T> Notify for LocalResource<T>
where
    T: 'static,
{
    fn notify(&self) {
        self.data.notify()
    }
}

impl<T> Write for LocalResource<T>
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

impl<T> ReadUntracked for LocalResource<T>
where
    T: 'static,
{
    type Value =
        ReadGuard<Option<T>, Mapped<AsyncPlain<SendOption<T>>, Option<T>>>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        if let Some(mut notifier) = use_context::<LocalResourceNotifier>() {
            notifier.notify();
        }
        self.data.try_read_untracked()
    }
}

impl<T: 'static> IsDisposed for LocalResource<T> {
    fn is_disposed(&self) -> bool {
        self.data.is_disposed()
    }
}

impl<T: 'static> ToAnySource for LocalResource<T>
where
    T: 'static,
{
    fn to_any_source(&self) -> AnySource {
        self.data.to_any_source()
    }
}

impl<T: 'static> ToAnySubscriber for LocalResource<T>
where
    T: 'static,
{
    fn to_any_subscriber(&self) -> AnySubscriber {
        self.data.to_any_subscriber()
    }
}

impl<T> Source for LocalResource<T>
where
    T: 'static,
{
    fn add_subscriber(&self, subscriber: AnySubscriber) {
        self.data.add_subscriber(subscriber)
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.data.remove_subscriber(subscriber);
    }

    fn clear_subscribers(&self) {
        self.data.clear_subscribers();
    }
}

impl<T> ReactiveNode for LocalResource<T>
where
    T: 'static,
{
    fn mark_dirty(&self) {
        self.data.mark_dirty();
    }

    fn mark_check(&self) {
        self.data.mark_check();
    }

    fn mark_subscribers_check(&self) {
        self.data.mark_subscribers_check();
    }

    fn update_if_necessary(&self) -> bool {
        self.data.update_if_necessary()
    }
}

impl<T> Subscriber for LocalResource<T>
where
    T: 'static,
{
    fn add_source(&self, source: AnySource) {
        self.data.add_source(source);
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        self.data.clear_sources(subscriber);
    }
}

impl<T: 'static> From<ArcLocalResource<T>> for LocalResource<T> {
    fn from(arc: ArcLocalResource<T>) -> Self {
        Self {
            data: arc.data.into(),
            refetch: arc.refetch.into(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: arc.defined_at,
        }
    }
}

impl<T: 'static> From<LocalResource<T>> for ArcLocalResource<T> {
    #[track_caller]
    fn from(local: LocalResource<T>) -> Self {
        if local.data.is_disposed() || local.refetch.is_disposed() {
            warn_disposed(
                DisposedUse::IntoArc,
                Location::caller(),
                local.defined_at(),
            );
            return Self {
                data: never_loads(),
                refetch: ArcRwSignal::new(0),
                #[cfg(any(debug_assertions, halyard_debuginfo))]
                defined_at: local.defined_at,
            };
        }
        Self {
            data: local.data.into(),
            refetch: local.refetch.into(),
            #[cfg(any(debug_assertions, halyard_debuginfo))]
            defined_at: local.defined_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::init_executor;
    use futures::FutureExt;
    use halyard_reactive_graph::{owner::Owner, traits::GetUntracked};

    /// `refetch` counts up: at `usize::MAX` it used to overflow, a panic in debug builds.
    #[test]
    fn arc_local_resource_refetch_wraps_instead_of_overflowing() {
        init_executor();
        let resource = ArcLocalResource::new(|| async { 1_u32 });
        resource.refetch.try_update(|n| *n = usize::MAX);

        resource.refetch();

        assert_eq!(resource.refetch.get_untracked(), 0);
    }

    #[test]
    fn local_resource_refetch_wraps_instead_of_overflowing() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| LocalResource::new(|| async { 1_u32 }));
        resource.refetch.try_update(|n| *n = usize::MAX);

        resource.refetch();

        assert_eq!(resource.refetch.get_untracked(), 0);
    }

    /// Awaiting a local resource whose owner is gone used to panic ("Tried to access a
    /// reactive value that has already been disposed"). The await never finishes.
    #[test]
    fn awaiting_a_disposed_local_resource_stays_pending() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| LocalResource::new(|| async { 1_u32 }));
        owner.cleanup();

        assert!(resource.into_future().now_or_never().is_none());
    }

    #[test]
    fn a_disposed_local_resource_converts_to_one_that_never_loads() {
        init_executor();
        let owner = Owner::new();
        let resource = owner.with(|| LocalResource::new(|| async { 1_u32 }));
        owner.cleanup();

        let arc = ArcLocalResource::from(resource);

        assert!(arc.into_future().now_or_never().is_none());
    }

    /// On the server, local resources never load.
    #[cfg(feature = "ssr")]
    mod server {
        use super::*;
        use futures::channel::oneshot;
        use halyard_reactive_graph::owner::provide_context;

        /// Under `<Suspense/>` or `<Transition/>`, which provide the notifier, awaiting a
        /// local resource tells the boundary, which renders its fallback on the server
        /// and leaves the rest to the browser. (Unchanged.)
        #[test]
        fn awaiting_a_local_resource_under_suspense_notifies_it() {
            let (notifier, mut notified) = oneshot::channel();
            let owner = Owner::new();
            owner.with(|| {
                provide_context(LocalResourceNotifier::from(notifier));
                let resource = LocalResource::new(|| async { 1_u32 });
                assert!(resource.into_future().now_or_never().is_none());
            });

            assert_eq!(notified.try_recv(), Ok(Some(())));
        }

        /// Outside a boundary there is nobody to tell, and this used to panic, failing the
        /// request. A local resource never resolves on the server, so the await stays
        /// pending (and says why in the log).
        #[test]
        fn awaiting_a_local_resource_outside_suspense_stays_pending() {
            let owner = Owner::new();
            let resource =
                owner.with(|| LocalResource::new(|| async { 1_u32 }));

            assert!(resource.into_future().now_or_never().is_none());
        }

        #[test]
        fn awaiting_an_arc_local_resource_outside_suspense_stays_pending() {
            let resource = ArcLocalResource::new(|| async { 1_u32 });

            assert!(resource.into_future().now_or_never().is_none());
        }
    }
}
