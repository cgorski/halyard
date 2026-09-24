use halyard_reactive_graph::or_poisoned::OrPoisoned;
use halyard_tachys::either::*;
use halyard_tachys::view::any_view::{AnyView, IntoAny};
use std::{
    future::Future,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

pub trait ChooseView
where
    Self: Send + Clone + 'static,
{
    fn choose(self) -> impl Future<Output = AnyView>;

    fn preload(&self) -> impl Future<Output = ()>;
}

impl<F, View> ChooseView for F
where
    F: Fn() -> View + Send + Clone + 'static,
    View: IntoAny,
{
    async fn choose(self) -> AnyView {
        self().into_any()
    }

    async fn preload(&self) {}
}

impl<T> ChooseView for Lazy<T>
where
    T: Send + Sync + LazyRoute,
{
    async fn choose(self) -> AnyView {
        let preloaded = self.data.lock().or_poisoned().take();
        let data = preloaded.unwrap_or_else(T::data);
        T::view(data).await
    }

    async fn preload(&self) {
        let data = T::data();
        let replaced = self.data.lock().or_poisoned().replace(data);
        // dropped once the lock is released
        drop(replaced);
        T::preload().await;
    }
}

pub trait LazyRoute: Send + 'static {
    fn data() -> Self;

    fn view(this: Self) -> impl Future<Output = AnyView>;

    fn preload() -> impl Future<Output = ()> {
        async {}
    }
}

#[derive(Debug)]
pub struct Lazy<T> {
    ty: PhantomData<T>,
    /// The route's data, made by `preload` and taken by `choose`. It is not `Clone`, so it is
    /// moved in and out of a private slot (no other code runs under its lock) rather than
    /// kept in a stored value, whose writes need a copy.
    data: Arc<Mutex<Option<T>>>,
}

impl<T> Clone for Lazy<T> {
    fn clone(&self) -> Self {
        Self {
            ty: self.ty,
            data: Arc::clone(&self.data),
        }
    }
}

impl<T> Lazy<T> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> Default for Lazy<T> {
    fn default() -> Self {
        Self {
            ty: Default::default(),
            data: Arc::new(Mutex::new(None)),
        }
    }
}

impl ChooseView for () {
    async fn choose(self) -> AnyView {
        ().into_any()
    }

    async fn preload(&self) {}
}

impl<A, B> ChooseView for Either<A, B>
where
    A: ChooseView,
    B: ChooseView,
{
    async fn choose(self) -> AnyView {
        match self {
            Either::Left(f) => f.choose().await.into_any(),
            Either::Right(f) => f.choose().await.into_any(),
        }
    }

    async fn preload(&self) {
        match self {
            Either::Left(f) => f.preload().await,
            Either::Right(f) => f.preload().await,
        }
    }
}

macro_rules! tuples {
    ($either:ident => $($ty:ident),*) => {
        impl<$($ty,)*> ChooseView for $either<$($ty,)*>
        where
            $($ty: ChooseView,)*
        {
            async fn choose(self) -> AnyView {
                match self {
                    $(
                        $either::$ty(f) => f.choose().await.into_any(),
                    )*
                }
            }

            async fn preload(&self) {
                match self {
                    $($either::$ty(f) => f.preload().await,)*
                }
            }
        }
    };
}

tuples!(EitherOf3 => A, B, C);
tuples!(EitherOf4 => A, B, C, D);
tuples!(EitherOf5 => A, B, C, D, E);
tuples!(EitherOf6 => A, B, C, D, E, F);
tuples!(EitherOf7 => A, B, C, D, E, F, G);
tuples!(EitherOf8 => A, B, C, D, E, F, G, H);
tuples!(EitherOf9 => A, B, C, D, E, F, G, H, I);
tuples!(EitherOf10 => A, B, C, D, E, F, G, H, I, J);
tuples!(EitherOf11 => A, B, C, D, E, F, G, H, I, J, K);
tuples!(EitherOf12 => A, B, C, D, E, F, G, H, I, J, K, L);
tuples!(EitherOf13 => A, B, C, D, E, F, G, H, I, J, K, L, M);
tuples!(EitherOf14 => A, B, C, D, E, F, G, H, I, J, K, L, M, N);
tuples!(EitherOf15 => A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
tuples!(EitherOf16 => A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

/// A version of [`IntoMaybeErased`] for the [`ChooseView`] trait.
pub trait IntoChooseViewMaybeErased {
    /// The type of the erased view.
    type Output: IntoChooseViewMaybeErased;

    /// Erase the type of the view.
    fn into_maybe_erased(self) -> Self::Output;
}

impl<T> IntoChooseViewMaybeErased for T
where
    T: ChooseView + Send + Clone + 'static,
{
    #[cfg(erase_components)]
    type Output = crate::router::matching::any_choose_view::AnyChooseView;

    #[cfg(not(erase_components))]
    type Output = Self;

    fn into_maybe_erased(self) -> Self::Output {
        #[cfg(erase_components)]
        {
            crate::router::matching::any_choose_view::AnyChooseView::new(self)
        }
        #[cfg(not(erase_components))]
        {
            self
        }
    }
}
