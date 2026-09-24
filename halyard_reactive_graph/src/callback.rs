//! Callbacks define a standard way to store functions and closures. They are useful
//! for component properties, because they can be used to define optional callback functions,
//! which generic props don’t support.
//!
//! The callback types implement [`Copy`], so they can easily be moved into and out of other closures, just like signals.
//!
//! # Types
//! This modules implements 4 callback types:
//! - [`Callback`](crate::callback::Callback) and
//!   [`UnsyncCallback`](crate::callback::UnsyncCallback): `Copy` arena (weak) handles, like
//!   signals. [`try_run`](Callable::try_run) gives `None` once the callback is gone;
//!   [`run`](Run::run) exists only for callbacks that return `()`, and does nothing
//!   (reported once) once the callback is gone.
//! - [`ArcCallback`](crate::callback::ArcCallback) and
//!   [`ArcUnsyncCallback`](crate::callback::ArcUnsyncCallback): reference-counted (strong)
//!   callbacks, which keep their function alive: [`run`](Run::run) is total.
//!
//! Use `UnsyncCallback` if the function is not `Sync` and `Send`.

use crate::{
    gone::{report_gone, Attempt},
    owner::{LocalStorage, StoredValue},
    traits::{DefinedAt, Dispose, TryGetValue, TryWithValue},
    IntoReactiveValue,
};
use std::{fmt, panic::Location, rc::Rc, sync::Arc};

/// A wrapper trait for calling callbacks.
///
/// The callback's function runs with nothing of the reactive system borrowed or locked, so
/// it may run this same callback again, or dispose of it.
pub trait Callable<In: 'static, Out: 'static = ()> {
    /// calls the callback with the specified argument.
    ///
    /// Returns None if the callback has been disposed
    fn try_run(&self, input: In) -> Option<Out>;
}

/// Calls a callback that always runs or has nothing to return: a strong callback
/// ([`ArcCallback`], [`ArcUnsyncCallback`]), or a weak one ([`Callback`], [`UnsyncCallback`])
/// that returns `()`, which does nothing (reported once) when it is gone.
pub trait Run<In: 'static, Out: 'static = ()> {
    /// Calls the callback with the specified argument.
    #[track_caller]
    fn run(&self, input: In) -> Out;
}

/// A callback type that is not required to be [`Send`] or [`Sync`].
///
/// # Example
/// ```
/// # use halyard_reactive_graph::prelude::*; use halyard_reactive_graph::callback::*;  let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// let _: UnsyncCallback<()> = UnsyncCallback::new(|_| {});
/// let _: UnsyncCallback<(i32, i32)> = (|_x: i32, _y: i32| {}).into();
/// let cb: UnsyncCallback<i32, String> = UnsyncCallback::new(|x: i32| x.to_string());
/// assert_eq!(cb.try_run(42), Some("42".to_string()));
/// ```
pub struct UnsyncCallback<In: 'static, Out: 'static = ()>(
    StoredValue<Rc<dyn Fn(In) -> Out>, LocalStorage>,
);

impl<In> fmt::Debug for UnsyncCallback<In> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        fmt.write_str("Callback")
    }
}

impl<In, Out> Copy for UnsyncCallback<In, Out> {}

impl<In, Out> Clone for UnsyncCallback<In, Out> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<In, Out> Dispose for UnsyncCallback<In, Out> {
    fn dispose(self) {
        self.0.dispose();
    }
}

impl<In, Out> UnsyncCallback<In, Out> {
    /// Creates a new callback from the given function.
    pub fn new<F>(f: F) -> UnsyncCallback<In, Out>
    where
        F: Fn(In) -> Out + 'static,
    {
        Self(StoredValue::new_local(Rc::new(f)))
    }

    /// Returns `true` if both callbacks wrap the same underlying function pointer.
    ///
    /// A disposed callback matches nothing.
    #[inline]
    pub fn matches(&self, other: &Self) -> bool {
        self.0
            .try_with_value(|self_value| {
                other.0.try_with_value(|other_value| {
                    Rc::ptr_eq(self_value, other_value)
                })
            })
            .flatten()
            .unwrap_or(false)
    }
}

impl<In: 'static, Out: 'static> Callable<In, Out> for UnsyncCallback<In, Out> {
    fn try_run(&self, input: In) -> Option<Out> {
        // a clone of the function, so that nothing is borrowed while it runs
        let fun = self.0.try_get_value()?;
        Some(fun(input))
    }
}

impl<In: 'static> Run<In> for UnsyncCallback<In> {
    #[track_caller]
    fn run(&self, input: In) {
        if self.try_run(input).is_none() {
            report_gone(
                Attempt::Write,
                "UnsyncCallback",
                self.0.defined_at(),
                Location::caller(),
            );
        }
    }
}

impl<In: 'static, Out: 'static> UnsyncCallback<In, Out> {
    /// Returns a strong (reference-counted) callback, which keeps the function alive, or
    /// `None` if it is gone (like [`std::rc::Weak::upgrade`]). The reverse, a downgrade, is
    /// `From<ArcUnsyncCallback>`.
    pub fn upgrade(&self) -> Option<ArcUnsyncCallback<In, Out>> {
        self.0.try_get_value().map(ArcUnsyncCallback)
    }
}

/// A reference-counted callback that is not required to be [`Send`] or [`Sync`]. It keeps
/// its function alive, so [`run`](Run::run) is total.
///
/// ```
/// # use halyard_reactive_graph::callback::*;
/// let cb = ArcUnsyncCallback::new(|x: i32| x.to_string());
/// assert_eq!(cb.run(42), "42".to_string());
/// ```
pub struct ArcUnsyncCallback<In: 'static, Out: 'static = ()>(
    Rc<dyn Fn(In) -> Out>,
);

impl<In, Out> ArcUnsyncCallback<In, Out> {
    /// Creates a new callback from the given function.
    pub fn new(f: impl Fn(In) -> Out + 'static) -> Self {
        Self(Rc::new(f))
    }
}

impl<In, Out> Clone for ArcUnsyncCallback<In, Out> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<In, Out> fmt::Debug for ArcUnsyncCallback<In, Out> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        fmt.write_str("ArcUnsyncCallback")
    }
}

impl<In: 'static, Out: 'static> Callable<In, Out>
    for ArcUnsyncCallback<In, Out>
{
    fn try_run(&self, input: In) -> Option<Out> {
        Some((self.0)(input))
    }
}

impl<In: 'static, Out: 'static> Run<In, Out> for ArcUnsyncCallback<In, Out> {
    fn run(&self, input: In) -> Out {
        (self.0)(input)
    }
}

impl<In, Out> From<ArcUnsyncCallback<In, Out>> for UnsyncCallback<In, Out> {
    #[track_caller]
    fn from(value: ArcUnsyncCallback<In, Out>) -> Self {
        Self(StoredValue::new_local(value.0))
    }
}

macro_rules! impl_unsync_callable_from_fn {
    ($($arg:ident),*) => {
        impl<F, $($arg,)* T, Out> From<F> for UnsyncCallback<($($arg,)*), Out>
        where
            F: Fn($($arg),*) -> T + 'static,
            T: Into<Out> + 'static,
            $($arg: 'static,)*
        {
            fn from(f: F) -> Self {
                pastey::paste!(
                    Self::new(move |($([<$arg:lower>],)*)| f($([<$arg:lower>]),*).into())
                )
            }
        }
    };
}

impl_unsync_callable_from_fn!();
impl_unsync_callable_from_fn!(P1);
impl_unsync_callable_from_fn!(P1, P2);
impl_unsync_callable_from_fn!(P1, P2, P3);
impl_unsync_callable_from_fn!(P1, P2, P3, P4);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10);
impl_unsync_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11);
impl_unsync_callable_from_fn!(
    P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12
);

/// A callback type that is [`Send`] + [`Sync`].
///
/// # Example
/// ```
/// # use halyard_reactive_graph::prelude::*; use halyard_reactive_graph::callback::*;  let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// let _: Callback<()> = Callback::new(|_| {});
/// let _: Callback<(i32, i32)> = (|_x: i32, _y: i32| {}).into();
/// let cb: Callback<i32, String> = Callback::new(|x: i32| x.to_string());
/// assert_eq!(cb.try_run(42), Some("42".to_string()));
/// ```
pub struct Callback<In, Out = ()>(
    StoredValue<Arc<dyn Fn(In) -> Out + Send + Sync>>,
)
where
    In: 'static,
    Out: 'static;

impl<In, Out> fmt::Debug for Callback<In, Out> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        fmt.write_str("SyncCallback")
    }
}

impl<In, Out> Callable<In, Out> for Callback<In, Out> {
    fn try_run(&self, input: In) -> Option<Out> {
        // a clone of the function, so that nothing is borrowed while it runs
        let fun = self.0.try_get_value()?;
        Some(fun(input))
    }
}

impl<In: 'static> Run<In> for Callback<In> {
    #[track_caller]
    fn run(&self, input: In) {
        if self.try_run(input).is_none() {
            report_gone(
                Attempt::Write,
                "Callback",
                self.0.defined_at(),
                Location::caller(),
            );
        }
    }
}

impl<In: 'static, Out: 'static> Callback<In, Out> {
    /// Returns a strong (reference-counted) callback, which keeps the function alive, or
    /// `None` if it is gone (like [`std::sync::Weak::upgrade`]). The reverse, a downgrade,
    /// is `From<ArcCallback>`.
    pub fn upgrade(&self) -> Option<ArcCallback<In, Out>> {
        self.0.try_get_value().map(ArcCallback)
    }
}

/// A reference-counted callback that is [`Send`] + [`Sync`]. It keeps its function alive,
/// so [`run`](Run::run) is total.
///
/// ```
/// # use halyard_reactive_graph::callback::*;
/// let cb = ArcCallback::new(|x: i32| x.to_string());
/// assert_eq!(cb.run(42), "42".to_string());
/// ```
pub struct ArcCallback<In: 'static, Out: 'static = ()>(
    Arc<dyn Fn(In) -> Out + Send + Sync>,
);

impl<In, Out> ArcCallback<In, Out> {
    /// Creates a new callback from the given function.
    pub fn new(f: impl Fn(In) -> Out + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
}

impl<In, Out> Clone for ArcCallback<In, Out> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<In, Out> fmt::Debug for ArcCallback<In, Out> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        fmt.write_str("ArcCallback")
    }
}

impl<In: 'static, Out: 'static> Callable<In, Out> for ArcCallback<In, Out> {
    fn try_run(&self, input: In) -> Option<Out> {
        Some((self.0)(input))
    }
}

impl<In: 'static, Out: 'static> Run<In, Out> for ArcCallback<In, Out> {
    fn run(&self, input: In) -> Out {
        (self.0)(input)
    }
}

impl<In, Out> From<ArcCallback<In, Out>> for Callback<In, Out> {
    #[track_caller]
    fn from(value: ArcCallback<In, Out>) -> Self {
        Self(StoredValue::new(value.0))
    }
}

impl<In, Out> Clone for Callback<In, Out> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<In, Out> Dispose for Callback<In, Out> {
    fn dispose(self) {
        self.0.dispose();
    }
}

impl<In, Out> Copy for Callback<In, Out> {}

macro_rules! impl_callable_from_fn {
    ($($arg:ident),*) => {
        impl<F, $($arg,)* T, Out> From<F> for Callback<($($arg,)*), Out>
        where
            F: Fn($($arg),*) -> T + Send + Sync + 'static,
            T: Into<Out> + 'static,
            $($arg: Send + Sync + 'static,)*
        {
            fn from(f: F) -> Self {
                pastey::paste!(
                    Self::new(move |($([<$arg:lower>],)*)| f($([<$arg:lower>]),*).into())
                )
            }
        }
    };
}

impl_callable_from_fn!();
impl_callable_from_fn!(P1);
impl_callable_from_fn!(P1, P2);
impl_callable_from_fn!(P1, P2, P3);
impl_callable_from_fn!(P1, P2, P3, P4);
impl_callable_from_fn!(P1, P2, P3, P4, P5);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11);
impl_callable_from_fn!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12);

impl<In: 'static, Out: 'static> Callback<In, Out> {
    /// Creates a new callback from the given function.
    #[track_caller]
    pub fn new<F>(fun: F) -> Self
    where
        F: Fn(In) -> Out + Send + Sync + 'static,
    {
        Self(StoredValue::new(Arc::new(fun)))
    }

    /// Returns `true` if both callbacks wrap the same underlying function pointer.
    #[inline]
    pub fn matches(&self, other: &Self) -> bool {
        self.0
            .try_with_value(|self_value| {
                other.0.try_with_value(|other_value| {
                    Arc::ptr_eq(self_value, other_value)
                })
            })
            .flatten()
            .unwrap_or(false)
    }
}

#[doc(hidden)]
pub struct __IntoReactiveValueMarkerCallbackSingleParam;

#[doc(hidden)]
pub struct __IntoReactiveValueMarkerCallbackStrOutputToString;

impl<I, O, F>
    IntoReactiveValue<
        Callback<I, O>,
        __IntoReactiveValueMarkerCallbackSingleParam,
    > for F
where
    F: Fn(I) -> O + Send + Sync + 'static,
{
    #[track_caller]
    fn into_reactive_value(self) -> Callback<I, O> {
        Callback::new(self)
    }
}

impl<I, O, F>
    IntoReactiveValue<
        UnsyncCallback<I, O>,
        __IntoReactiveValueMarkerCallbackSingleParam,
    > for F
where
    F: Fn(I) -> O + 'static,
{
    #[track_caller]
    fn into_reactive_value(self) -> UnsyncCallback<I, O> {
        UnsyncCallback::new(self)
    }
}

impl<I, F>
    IntoReactiveValue<
        Callback<I, String>,
        __IntoReactiveValueMarkerCallbackStrOutputToString,
    > for F
where
    F: Fn(I) -> &'static str + Send + Sync + 'static,
{
    #[track_caller]
    fn into_reactive_value(self) -> Callback<I, String> {
        Callback::new(move |i| self(i).to_string())
    }
}

impl<I, F>
    IntoReactiveValue<
        UnsyncCallback<I, String>,
        __IntoReactiveValueMarkerCallbackStrOutputToString,
    > for F
where
    F: Fn(I) -> &'static str + 'static,
{
    #[track_caller]
    fn into_reactive_value(self) -> UnsyncCallback<I, String> {
        UnsyncCallback::new(move |i| self(i).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Callable;
    use crate::{
        callback::{Callback, UnsyncCallback},
        owner::Owner,
        traits::Dispose,
        IntoReactiveValue,
    };

    struct NoClone {}

    #[test]
    fn clone_callback() {
        let owner = Owner::new();
        owner.set();

        let callback = Callback::new(move |_no_clone: NoClone| NoClone {});
        let _cloned = callback;
    }

    #[test]
    fn clone_unsync_callback() {
        let owner = Owner::new();
        owner.set();

        let callback =
            UnsyncCallback::new(move |_no_clone: NoClone| NoClone {});
        let _cloned = callback;
    }

    #[test]
    fn runback_from() {
        let owner = Owner::new();
        owner.set();

        let _callback: Callback<(), String> = (|| "test").into();
        let _callback: Callback<(i32, String), String> =
            (|num, s| format!("{num} {s}")).into();
        // Single params should work without needing the (foo,) tuple using IntoReactiveValue:
        let _callback: Callback<usize, &'static str> =
            (|_usize| "test").into_reactive_value();
        let _callback: Callback<usize, String> =
            (|_usize| "test").into_reactive_value();
    }

    #[test]
    fn sync_callback_from() {
        let owner = Owner::new();
        owner.set();

        let _callback: UnsyncCallback<(), String> = (|| "test").into();
        let _callback: UnsyncCallback<(i32, String), String> =
            (|num, s| format!("{num} {s}")).into();
        // Single params should work without needing the (foo,) tuple using IntoReactiveValue:
        let _callback: UnsyncCallback<usize, &'static str> =
            (|_usize| "test").into_reactive_value();
        let _callback: UnsyncCallback<usize, String> =
            (|_usize| "test").into_reactive_value();
    }

    #[test]
    fn sync_callback_try_run() {
        let owner = Owner::new();
        owner.set();

        let callback = Callback::new(move |arg| arg);
        assert_eq!(callback.try_run((0,)), Some((0,)));
        callback.dispose();
        assert_eq!(callback.try_run((0,)), None);
    }

    #[test]
    fn unsync_callback_try_run() {
        let owner = Owner::new();
        owner.set();

        let callback = UnsyncCallback::new(move |arg| arg);
        assert_eq!(callback.try_run((0,)), Some((0,)));
        callback.dispose();
        assert_eq!(callback.try_run((0,)), None);
    }

    #[test]
    fn callback_matches_same() {
        let owner = Owner::new();
        owner.set();

        let callback1 = Callback::new(|x: i32| x * 2);
        let callback2 = callback1;
        assert!(callback1.matches(&callback2));
    }

    #[test]
    fn callback_matches_different() {
        let owner = Owner::new();
        owner.set();

        let callback1 = Callback::new(|x: i32| x * 2);
        let callback2 = Callback::new(|x: i32| x + 1);
        assert!(!callback1.matches(&callback2));
    }

    #[test]
    fn unsync_callback_matches_same() {
        let owner = Owner::new();
        owner.set();

        let callback1 = UnsyncCallback::new(|x: i32| x * 2);
        let callback2 = callback1;
        assert!(callback1.matches(&callback2));
    }

    #[test]
    fn unsync_callback_matches_different() {
        let owner = Owner::new();
        owner.set();

        let callback1 = UnsyncCallback::new(|x: i32| x * 2);
        let callback2 = UnsyncCallback::new(|x: i32| x + 1);
        assert!(!callback1.matches(&callback2));
    }
}
