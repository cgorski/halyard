//! A series of traits to implement the behavior of reactive primitive, especially signals.
//!
//! ## Principles
//! 1. **Composition**: Most of the traits are implemented as combinations of more primitive base traits,
//!    and blanket implemented for all types that implement those traits.
//! 2. **Fallibility**: Most traits includes a `try_` variant, which returns `None` if the method
//!    fails (e.g., if signals are arena allocated and this can't be found, or if an `RwLock` is
//!    poisoned).
//!
//! ## Metadata Traits
//! - [`DefinedAt`] is used for debugging in the case of errors and should be implemented for all
//!   signal types.
//! - [`IsDisposed`] checks whether a signal is currently accessible.
//!
//! ## Base Traits
//! | Trait             | Mode  | Description                                                                           |
//! |-------------------|-------|---------------------------------------------------------------------------------------|
//! | [`Track`]         | —     | Tracks changes to this value, adding it as a source of the current reactive observer. |
//! | [`Notify`]       | —      | Notifies subscribers that this value has changed.                                     |
//! | [`TryReadUntracked`] | Guard | Gives immutable access to the value of this signal.                                   |
//! | [`Write`]     | Guard | Gives mutable access to the value of this signal.
//!
//! ## Derived Traits
//!
//! ### Access
//! | Trait             | Mode          | Composition                   | Description
//! |-------------------|---------------|-------------------------------|------------
//! | [`TryWithUntracked`] | `fn(&T) -> U` | [`TryReadUntracked`]                  | Applies closure to the current value of the signal and returns result.
//! | [`With`]          | `fn(&T) -> U` | [`TryReadUntracked`] + [`Track`]      | Applies closure to the current value of the signal and returns result, with reactive tracking.
//! | [`TryGetUntracked`]  | `T`           | [`TryWithUntracked`] + [`Clone`] | Clones the current value of the signal.
//! | [`Get`]           | `T`           | [`TryGetUntracked`] + [`Track`]  | Clones the current value of the signal, with reactive tracking.
//!
//! ### Update
//! | Trait               | Mode          | Composition                       | Description
//! |---------------------|---------------|-----------------------------------|------------
//! | [`UpdateUntracked`] | `fn(&mut T)`  | [`Write`]                     | Applies closure to the current value to update it, but doesn't notify subscribers.
//! | [`Update`]          | `fn(&mut T)`  | [`UpdateUntracked`] + [`Notify`] | Applies closure to the current value to update it, and notifies subscribers.
//! | [`Set`]             | `T`           | [`Update`]                        | Sets the value to a new value, and notifies subscribers.
//!
//! ## Re-entry
//!
//! A signal can be reached again from code that is already using it: from inside its own
//! `with` or `update`, while a guard of it is alive, or through a memo or effect that depends
//! on it. That never waits for a lock this thread holds (a deadlock natively, an abort in the
//! browser), and never panics:
//! - [`Update::update`] runs its closure on a copy of the committed value, outside every
//!   lock, then commits the result. Reads of the signal inside the closure (directly, or
//!   through memos and derived signals) see the last committed value.
//! - A write to the signal ([`Set::set`], [`Update::update`], [`Notify::notify`], the drop
//!   of a [`Write`] guard) made while this thread is using it is deferred, and applied in
//!   order when this thread's outermost use of the signal ends. A deferred `update` runs its
//!   closure at once, on the value it will be committed over (the committed value and the
//!   writes deferred before it), and defers the result. So an `update` nested in an `update`
//!   of the same signal starts from the same committed value as the outer one, and is
//!   committed after it: its result replaces the outer one (the first time, that is logged).
//! - The tracked [`Write`] guard of a signal holds a copy of the value, committed (or
//!   deferred, as above) when it is dropped: no lock is held while it is alive.
//! - The in-place forms ([`Update::try_update`], [`UpdateUntracked`],
//!   [`Write::try_write_untracked`]) hold the value's lock while the closure runs or the
//!   guard lives. Started while this thread is using the signal, they return `None`; while
//!   they run, reading the signal on this thread gives `None` from the `try_*` forms (the
//!   first time, that is logged).
//! - Writes from other threads wait for their turn: updates of one signal serialize, none is
//!   lost. Reads never wait for an update's closure.
//!
//! Stored values ([`TryWithValue`], [`UpdateValue`], [`TryReadValue`], [`WriteValue`]) are not
//! signals: their closures run on the borrowed value, and reaching the same value again from
//! there is refused (`None` from the `try_*` forms) and logged.
//!
//! ## Using the Traits
//!
//! These traits are designed so that you can implement as few as possible, and the rest will be
//! implemented automatically.
//!
//! For example, if you have a struct for which you can implement [`TryReadUntracked`] and [`Track`], then
//! [`TryWithUntracked`] and [`With`] will be implemented automatically (as will [`TryGetUntracked`] and
//! [`Get`] for `Clone` types). But if you cannot implement [`TryReadUntracked`] (because, for example,
//! there isn't an `RwLock` so you can't wrap in a [`ReadGuard`](crate::signal::guards::ReadGuard),
//! but you can still implement [`TryWithUntracked`] and [`Track`], the same traits will still be implemented.

use crate::executor::Executor;
pub use crate::map::*;
pub use crate::trait_options::*;
use crate::{
    effect::Effect,
    graph::{Observer, Source, Subscriber, ToAnySource},
    owner::Owner,
    signal::{arc_signal, guards::UntrackedWriteGuard, ArcReadSignal},
};
use futures::{Stream, StreamExt};
use std::{
    ops::{Deref, DerefMut},
    panic::Location,
};

#[doc(hidden)]
pub mod seal {
    /// Seals [`Strong`](super::Strong) and [`Weak`](super::Weak): only halyard's handle
    /// types implement them.
    pub trait Sealed {}
}

/// A reference-counted (strong) handle: it keeps its value alive, like [`std::sync::Arc`],
/// so reading through it is total ([`Get`], [`With`], [`Read`] and their `_untracked` and
/// `Value` forms).
///
/// A read through a strong handle comes back empty only if the value is in use by the code
/// that reads it: read inside its own in-place change (through another handle to the same
/// value), or a memo read inside its own computation. Both are cycles; the read is reported
/// once and waits. On a server, it also waits while another thread recomputes a memo.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a weak handle: its value may be gone, so it has no \
               `get`, `with` or `read`",
    label = "a weak (arena) handle",
    note = "read it with `try_get()`, `try_with()` or `try_read()` (an `Option`)",
    note = "or put the handle itself in the view (`{{count}}`, \
            `prop:value=name`, `<For each=items>`, `<Show when=flag>`), or \
            derive with `.map(...)`",
    note = "or take a strong handle, which keeps the value alive, with \
            `.upgrade()`"
)]
pub trait Strong: seal::Sealed {}

/// A `Copy` arena (weak) handle: it does not keep its value alive, like
/// [`std::sync::Weak`]. Reads return an `Option` (`try_*`), a write to a gone value does
/// nothing (reported once), and only weak handles change a value in place
/// ([`UpdateInPlace`], [`UpdateUntracked`], [`WriteUntracked`]).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a weak handle: in-place changes are only offered \
               through weak handles",
    note = "use `update` (it changes a copy) or `set`, or change the value in \
            place through its weak handle (`.downgrade()`)"
)]
pub trait Weak: seal::Sealed {}

/// Implements [`Strong`] for a type.
#[doc(hidden)]
#[macro_export]
macro_rules! impl_strong {
    ($([$($gen:tt)*] $ty:ty $(where [$($wc:tt)*])?),* $(,)?) => {
        $(
            impl<$($gen)*> $crate::traits::seal::Sealed for $ty
            $(where $($wc)*)? {}
            impl<$($gen)*> $crate::traits::Strong for $ty
            $(where $($wc)*)? {}
        )*
    };
}

/// Implements [`Weak`] for a type.
#[doc(hidden)]
#[macro_export]
macro_rules! impl_weak {
    ($([$($gen:tt)*] $ty:ty $(where [$($wc:tt)*])?),* $(,)?) => {
        $(
            impl<$($gen)*> $crate::traits::seal::Sealed for $ty
            $(where $($wc)*)? {}
            impl<$($gen)*> $crate::traits::Weak for $ty
            $(where $($wc)*)? {}
        )*
    };
}

/// Allows disposing an arena-allocated signal before its owner has been disposed.
pub trait Dispose {
    /// Disposes of the signal. This:
    /// 1. Detaches the signal from the reactive graph, preventing it from triggering
    ///    further updates; and
    /// 2. Drops the value contained in the signal.
    fn dispose(self);
}

/// Allows tracking the value of some reactive data.
pub trait Track {
    /// Subscribes to this signal in the current reactive scope without doing anything with its value.
    #[track_caller]
    fn track(&self);
}

impl<T: Source + ToAnySource + DefinedAt> Track for T {
    #[track_caller]
    fn track(&self) {
        if self.is_disposed() {
            return;
        }

        if let Some(subscriber) = Observer::get() {
            subscriber.add_source(self.to_any_source());
            self.add_subscriber(subscriber);
        } else {
            #[cfg(all(debug_assertions, feature = "effects"))]
            {
                use crate::diagnostics::SpecialNonReactiveZone;

                if !SpecialNonReactiveZone::is_inside() {
                    let called_at = Location::caller();
                    let ty = std::any::type_name::<T>();
                    let defined_at = self
                        .defined_at()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| String::from("{unknown}"));
                    crate::log_warning(format_args!(
                        "At {called_at}, you access a {ty} (defined at \
                         {defined_at}) outside a reactive tracking context. \
                         This might mean your app is not responding to \
                         changes in signal values in the way you \
                         expect.\n\nHere’s how to fix it:\n\n1. If this is \
                         inside a `view!` macro, make sure you are passing a \
                         function, not a value.\n  ❌ NO  <p>{{x.get() * \
                         2}}</p>\n  ✅ YES <p>{{move || x.get() * \
                         2}}</p>\n\n2. If it’s in the body of a component, \
                         try wrapping this access in a closure: \n  ❌ NO  \
                         let y = x.get() * 2\n  ✅ YES let y = move || \
                         x.get() * 2.\n\n3. If you’re *trying* to access the \
                         value without tracking, use `.get_untracked()` or \
                         `.with_untracked()` instead."
                    ));
                }
            }
        }
    }
}

/// Give read-only access to a signal's value by reference through a guard type,
/// without tracking the value reactively.
pub trait TryReadUntracked: Sized + DefinedAt {
    /// The guard type that will be returned, which can be dereferenced to the value.
    type Value: Deref;

    /// Returns the guard, or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_read_untracked(&self) -> Option<Self::Value>;

    /// This is a backdoor to allow overriding the [`TryRead::try_read`] implementation despite it being auto implemented.
    ///
    /// If your type contains a [`Signal`](crate::wrappers::read::Signal),
    /// call it's [`TryReadUntracked::custom_try_read`] here, else return `None`.
    #[track_caller]
    fn custom_try_read(&self) -> Option<Option<Self::Value>> {
        None
    }
}

/// Give read-only access to a signal's value by reference through a guard type,
/// and subscribes the active reactive observer (an effect or computed) to changes in its value.
pub trait TryRead: DefinedAt {
    /// The guard type that will be returned, which can be dereferenced to the value.
    type Value: Deref;

    /// Subscribes to the signal, and returns the guard, or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_read(&self) -> Option<Self::Value>;
}

impl<T> TryRead for T
where
    T: Track + TryReadUntracked,
{
    type Value = T::Value;

    #[track_caller]
    fn try_read(&self) -> Option<Self::Value> {
        // The [`TryRead`] trait is auto implemented for types that implement [`TryReadUntracked`] + [`Track`]. The [`TryRead`] trait then auto implements the [`TryWith`] and [`TryGet`] traits too.
        //
        // This is a problem for e.g. the [`Signal`](crate::wrappers::read::Signal) type,
        // this type must use a custom [`Read::try_read`] implementation to avoid an unnecessary clone.
        //
        // This is a backdoor to allow overriding the [`Read::try_read`] implementation despite it being auto implemented.
        if let Some(custom) = self.custom_try_read() {
            custom
        } else {
            self.track();
            self.try_read_untracked()
        }
    }
}

/// A reactive, mutable guard that can be untracked to prevent it from notifying subscribers when
/// it is dropped.
pub trait UntrackableGuard: DerefMut {
    /// Removes the notifier from the guard, such that it will no longer notify subscribers when it is dropped.
    fn untrack(&mut self);
}

impl<T> UntrackableGuard for Box<dyn UntrackableGuard<Target = T>> {
    fn untrack(&mut self) {
        (**self).untrack();
    }
}

/// Gives mutable access to a signal's value through a guard type. When the guard is dropped, the
/// signal's subscribers will be notified.
///
/// For signals, the tracked guard ([`try_write`](Write::try_write)) holds a copy of the
/// value, committed when it is dropped, so no lock is held while it is alive; the untracked
/// guard of a weak handle ([`WriteUntracked`]) changes the value in place (see the module
/// docs, "Re-entry").
pub trait Write: Sized + DefinedAt + Notify {
    /// The type of the signal's value.
    type Value: Sized + 'static;

    /// Returns the guard, or `None` if the signal has already been disposed.
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        Self::Value: Clone;

    /// Returns a guard that changes the value in place and does not notify subscribers when
    /// dropped, or `None` if the signal has already been disposed. The implementation hook
    /// of [`WriteUntracked::try_write_untracked`], which only weak handles offer.
    #[doc(hidden)]
    fn try_write_in_place(&self)
        -> Option<impl DerefMut<Target = Self::Value>>;

    /// Replaces the value and notifies subscribers ([`Set`] is built on it). Gives the value
    /// back if it could not be written (the signal was disposed).
    ///
    /// The default writes through [`try_write_in_place`](Write::try_write_in_place);
    /// signals defer the write while this thread is using the signal.
    #[doc(hidden)]
    fn try_commit_value(&self, value: Self::Value) -> Option<Self::Value> {
        match self.try_write_in_place() {
            Some(mut guard) => {
                *guard = value;
                drop(guard);
                self.notify();
                None
            }
            None => Some(value),
        }
    }

    /// Runs `fun` on a copy of the value, outside every lock, and commits the result,
    /// notifying subscribers if `fun` returns `(true, _)` ([`Update::update`] is built on
    /// it). `None` if the signal was disposed.
    ///
    /// The default changes the value in place, like
    /// [`try_update_in_place`](Write::try_update_in_place).
    #[doc(hidden)]
    fn try_update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> (bool, U),
    ) -> Option<U>
    where
        Self::Value: Clone,
    {
        self.try_update_in_place(fun)
    }

    /// Runs `fun` on the value in place, notifying subscribers if it returns `(true, _)`
    /// ([`Update::try_update`] is built on it). `None` if the signal was disposed or this
    /// thread is using it already.
    #[doc(hidden)]
    fn try_update_in_place<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> (bool, U),
    ) -> Option<U> {
        let mut guard = self.try_write_in_place()?;
        let (changed, out) = fun(&mut *guard);
        drop(guard);
        if changed {
            self.notify();
        }
        Some(out)
    }
}

/// Give read-only access to a signal's value by reference inside a closure,
/// without tracking the value reactively.
pub trait TryWithUntracked: DefinedAt {
    /// The type of the value contained in the signal.
    type Value: ?Sized;

    /// Applies the closure to the value, and returns the result,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_with_untracked<U>(
        &self,
        fun: impl FnOnce(&Self::Value) -> U,
    ) -> Option<U>;
}

impl<T> TryWithUntracked for T
where
    T: DefinedAt + TryReadUntracked,
{
    type Value = <<Self as TryReadUntracked>::Value as Deref>::Target;

    #[track_caller]
    fn try_with_untracked<U>(
        &self,
        fun: impl FnOnce(&Self::Value) -> U,
    ) -> Option<U> {
        self.try_read_untracked().map(|value| fun(&value))
    }
}

/// Give read-only access to a signal's value by reference inside a closure,
/// and subscribes the active reactive observer (an effect or computed) to changes in its value.
pub trait TryWith: DefinedAt {
    /// The type of the value contained in the signal.
    type Value: ?Sized;

    /// Subscribes to the signal, applies the closure to the value, and returns the result,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_with<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> Option<U>;
}

impl<T> TryWith for T
where
    T: TryRead,
{
    type Value = <<T as TryRead>::Value as Deref>::Target;

    #[track_caller]
    fn try_with<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> Option<U> {
        self.try_read().map(|val| fun(&val))
    }
}

/// Clones the value of the signal, without tracking the value reactively.
pub trait TryGetUntracked: DefinedAt {
    /// The type of the value contained in the signal.
    type Value;

    /// Clones and returns the value of the signal,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_get_untracked(&self) -> Option<Self::Value>;
}

impl<T> TryGetUntracked for T
where
    T: TryWithUntracked,
    T::Value: Clone,
{
    type Value = <Self as TryWithUntracked>::Value;

    #[track_caller]
    fn try_get_untracked(&self) -> Option<Self::Value> {
        self.try_with_untracked(Self::Value::clone)
    }
}

/// Clones the value of the signal, without tracking the value reactively.
/// and subscribes the active reactive observer (an effect or computed) to changes in its value.
pub trait TryGet: DefinedAt {
    /// The type of the value contained in the signal.
    type Value: Clone;

    /// Subscribes to the signal, then clones and returns the value of the signal,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_get(&self) -> Option<Self::Value>;
}

impl<T> TryGet for T
where
    T: TryWith,
    T::Value: Clone,
{
    type Value = <T as TryWith>::Value;

    #[track_caller]
    fn try_get(&self) -> Option<Self::Value> {
        self.try_with(Self::Value::clone)
    }
}

/// Returns a guard to the value of a strong handle, without tracking it.
pub trait ReadUntracked: TryReadUntracked + Strong {
    /// Returns the guard.
    #[track_caller]
    fn read_untracked(&self) -> <Self as TryReadUntracked>::Value;
}

impl<T: TryReadUntracked + Strong> ReadUntracked for T {
    #[track_caller]
    fn read_untracked(&self) -> <Self as TryReadUntracked>::Value {
        crate::gone::wait_for(self.defined_at(), || self.try_read_untracked())
    }
}

/// Subscribes to a strong handle and returns a guard to its value.
pub trait Read: TryRead + Strong {
    /// Subscribes to the value and returns the guard.
    #[track_caller]
    fn read(&self) -> <Self as TryRead>::Value;
}

impl<T: TryRead + Strong> Read for T {
    #[track_caller]
    fn read(&self) -> <Self as TryRead>::Value {
        crate::gone::wait_for(self.defined_at(), || self.try_read())
    }
}

/// Applies a closure to the value of a strong handle, without tracking it.
pub trait WithUntracked: Strong {
    /// The type of the value.
    type Value: ?Sized;

    /// Applies the closure to the value and returns the result.
    #[track_caller]
    fn with_untracked<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U;
}

impl<T: TryReadUntracked + Strong> WithUntracked for T {
    type Value = <<T as TryReadUntracked>::Value as Deref>::Target;

    #[track_caller]
    fn with_untracked<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U {
        fun(&self.read_untracked())
    }
}

/// Subscribes to a strong handle and applies a closure to its value.
pub trait With: Strong {
    /// The type of the value.
    type Value: ?Sized;

    /// Subscribes to the value, applies the closure to it and returns the result.
    #[track_caller]
    fn with<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U;
}

impl<T: TryRead + Strong> With for T {
    type Value = <<T as TryRead>::Value as Deref>::Target;

    #[track_caller]
    fn with<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U {
        fun(&self.read())
    }
}

/// Clones the value of a strong handle, without tracking it.
pub trait GetUntracked: Strong {
    /// The type of the value.
    type Value: Clone;

    /// Clones and returns the value.
    #[track_caller]
    fn get_untracked(&self) -> Self::Value;
}

impl<T> GetUntracked for T
where
    T: TryReadUntracked + Strong,
    <<T as TryReadUntracked>::Value as Deref>::Target: Clone,
{
    type Value = <<T as TryReadUntracked>::Value as Deref>::Target;

    #[track_caller]
    fn get_untracked(&self) -> Self::Value {
        self.with_untracked(Clone::clone)
    }
}

/// Subscribes to a strong handle and clones its value.
pub trait Get: Strong {
    /// The type of the value.
    type Value: Clone;

    /// Subscribes to the value, clones and returns it.
    #[track_caller]
    fn get(&self) -> Self::Value;
}

impl<T> Get for T
where
    T: TryRead + Strong,
    <<T as TryRead>::Value as Deref>::Target: Clone,
{
    type Value = <<T as TryRead>::Value as Deref>::Target;

    #[track_caller]
    fn get(&self) -> Self::Value {
        self.with(Clone::clone)
    }
}

/// Notifies subscribers of a change in this signal.
pub trait Notify {
    /// Notifies subscribers of a change in this signal.
    #[track_caller]
    fn notify(&self);
}

/// Gives a guard to the value of a strong handle, through which it can be changed; the
/// change is committed, and subscribers notified, when the guard is dropped. For signals, the
/// guard holds a copy of the value (see the module docs, "Re-entry").
pub trait StrongWrite: Write + Strong {
    /// Returns the guard.
    #[track_caller]
    fn write(&self) -> impl UntrackableGuard<Target = <Self as Write>::Value>
    where
        <Self as Write>::Value: Clone;
}

impl<T: Write + Strong> StrongWrite for T {
    #[track_caller]
    fn write(&self) -> impl UntrackableGuard<Target = <Self as Write>::Value>
    where
        <Self as Write>::Value: Clone,
    {
        crate::gone::wait_for(self.defined_at(), || self.try_write())
    }
}

/// Updates the value of a signal by applying a function that updates it in place,
/// without notifying subscribers. Only weak handles change a value in place (see [`Weak`]).
pub trait UpdateUntracked: DefinedAt {
    /// The type of the value contained in the signal.
    type Value;

    /// Updates the value by applying a function, returning the value returned by that function,
    /// or `None` if the signal has already been disposed.
    /// Does not notify subscribers that the signal has changed.
    fn try_update_untracked<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U>;
}

impl<T> UpdateUntracked for T
where
    T: Write + Weak,
{
    type Value = <Self as Write>::Value;

    #[track_caller]
    fn try_update_untracked<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U> {
        let mut guard = self.try_write_in_place()?;
        Some(fun(&mut *guard))
    }
}

/// Gives a guard that changes the value in place without notifying subscribers when it is
/// dropped. Only weak handles change a value in place (see [`Weak`]).
pub trait WriteUntracked: Write + Weak {
    /// Returns the guard, or `None` if the value is gone or this thread is using it already.
    #[track_caller]
    fn try_write_untracked(
        &self,
    ) -> Option<impl DerefMut<Target = <Self as Write>::Value>>;
}

impl<T: Write + Weak> WriteUntracked for T {
    #[track_caller]
    fn try_write_untracked(
        &self,
    ) -> Option<impl DerefMut<Target = <Self as Write>::Value>> {
        self.try_write_in_place()
    }
}

/// Updates the value of a signal by applying a function that changes it, notifying its
/// subscribers that the value has changed.
///
/// [`update`](Update::update) and [`maybe_update`](Update::maybe_update) run the closure
/// on a copy of the committed value, outside every lock, then commit the result. Inside
/// the closure, reading the same signal gives its last committed value; writing it is
/// deferred until the update has committed. They need `Value: Clone`. Through a weak handle
/// whose value is gone, they do nothing (reported once).
pub trait Update {
    /// The type of the value contained in the signal.
    type Value;

    /// Updates the value of the signal and notifies subscribers.
    ///
    /// The closure runs on a copy of the committed value; see the trait docs.
    #[track_caller]
    fn update(&self, fun: impl FnOnce(&mut Self::Value))
    where
        Self::Value: Clone,
    {
        self.maybe_update(|val| {
            fun(val);
            true
        });
    }

    /// Updates the value of the signal, but only notifies subscribers if the function
    /// returns `true`.
    ///
    /// The closure runs on a copy of the committed value; see the trait docs.
    #[track_caller]
    fn maybe_update(&self, fun: impl FnOnce(&mut Self::Value) -> bool)
    where
        Self::Value: Clone;
}

impl<T> Update for T
where
    T: Write + IsDisposed,
{
    type Value = <Self as Write>::Value;

    #[track_caller]
    fn maybe_update(&self, fun: impl FnOnce(&mut Self::Value) -> bool)
    where
        Self::Value: Clone,
    {
        if self.try_update_snapshot(|val| (fun(val), ())).is_none()
            && self.is_disposed()
        {
            crate::gone::report_gone(
                crate::gone::Attempt::Write,
                std::any::type_name::<Self>(),
                self.defined_at(),
                Location::caller(),
            );
        }
    }
}

/// Changes the value of a signal in place, for any value (no copy), notifying its
/// subscribers. Only weak handles change a value in place (see [`Weak`]).
///
/// The closure runs on the value itself and its result is returned. They return `None`
/// without running the closure if the value is gone, or if this thread is using the signal
/// already (inside its `with` or `update`, or while a guard of it is alive). While the
/// closure runs, the same signal cannot be read on this thread (its `try_*` reads return
/// `None`).
pub trait UpdateInPlace {
    /// The type of the value contained in the signal.
    type Value;

    /// Updates the value of the signal in place and notifies subscribers, returning the value
    /// that is returned by the update function, or `None` if the signal has already been
    /// disposed or this thread is using it already.
    #[track_caller]
    fn try_update<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U> {
        self.try_maybe_update(|val| (true, fun(val)))
    }

    /// Updates the value of the signal in place, notifying subscribers if the update function
    /// returns `(true, _)`, and returns the value returned by the update function, or `None`
    /// if the signal has already been disposed or this thread is using it already.
    fn try_maybe_update<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> (bool, U),
    ) -> Option<U>;
}

impl<T> UpdateInPlace for T
where
    T: Write + Weak,
{
    type Value = <Self as Write>::Value;

    #[track_caller]
    fn try_maybe_update<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> (bool, U),
    ) -> Option<U> {
        self.try_update_in_place(fun)
    }
}

/// Updates the value of the signal by replacing it.
pub trait Set {
    /// The type of the value contained in the signal.
    type Value;

    /// Updates the value by replacing it, and notifies subscribers that it has changed.
    fn set(&self, value: Self::Value);

    /// Updates the value by replacing it, and notifies subscribers that it has changed.
    ///
    /// If the signal has already been disposed, returns `Some(value)` with the value that was
    /// passed in. Otherwise, returns `None`.
    fn try_set(&self, value: Self::Value) -> Option<Self::Value>;
}

impl<T> Set for T
where
    T: Write + IsDisposed,
{
    type Value = <Self as Write>::Value;

    #[track_caller]
    fn set(&self, value: Self::Value) {
        let failed = self.try_commit_value(value).is_some();

        if failed && self.is_disposed() {
            crate::gone::report_gone(
                crate::gone::Attempt::Write,
                std::any::type_name::<Self>(),
                self.defined_at(),
                Location::caller(),
            );
            return;
        }

        #[cfg(any(debug_assertions, halyard_debuginfo))]
        if failed {
            let called_at = Location::caller();
            let ty = std::any::type_name::<Self::Value>();

            crate::log_warning(format_args!(
                "At {called_at}, you tried to update a {ty}, but the update \
                 failed. This can happen if this thread is using a value that \
                 is not a signal (a resource or an async derived value) inside \
                 its own update."
            ));
        };
    }

    #[track_caller]
    fn try_set(&self, value: Self::Value) -> Option<Self::Value> {
        if self.is_disposed() {
            Some(value)
        } else {
            self.try_commit_value(value)
        }
    }
}

/// Allows converting a signal into an async [`Stream`].
pub trait ToStream<T> {
    /// Generates a [`Stream`] that emits the new value of the signal
    /// whenever it changes.
    ///
    /// Once the signal's value is gone (its owner was disposed), the stream emits nothing
    /// more.
    #[track_caller]
    fn to_stream(&self) -> impl Stream<Item = T> + Send;
}

impl<S> ToStream<<S as TryGet>::Value> for S
where
    S: Clone + TryGet + Send + Sync + 'static,
    <S as TryGet>::Value: Send + 'static,
{
    fn to_stream(&self) -> impl Stream<Item = <S as TryGet>::Value> + Send {
        let (tx, rx) = futures::channel::mpsc::unbounded();

        let close_channel = tx.clone();

        Owner::on_cleanup(move || close_channel.close_channel());

        Effect::new_isomorphic({
            let this = self.clone();
            move |_| {
                // a signal whose value is gone has nothing more to send
                if let Some(value) = this.try_get() {
                    let _ = tx.unbounded_send(value);
                }
            }
        });

        rx
    }
}

/// Allows creating a signal from an async [`Stream`].
pub trait FromStream<T> {
    /// Creates a signal that contains the latest value of the stream.
    #[track_caller]
    fn from_stream(stream: impl Stream<Item = T> + Send + 'static) -> Self;

    /// Creates a signal that contains the latest value of the stream.
    #[track_caller]
    fn from_stream_unsync(stream: impl Stream<Item = T> + 'static) -> Self;
}

impl<S, T> FromStream<T> for S
where
    S: From<ArcReadSignal<Option<T>>> + Send + Sync,
    T: Send + Sync + 'static,
{
    fn from_stream(stream: impl Stream<Item = T> + Send + 'static) -> Self {
        let (read, write) = arc_signal(None);
        let mut stream = Box::pin(stream);
        crate::spawn(async move {
            while let Some(value) = stream.next().await {
                write.set(Some(value));
            }
        });
        read.into()
    }

    fn from_stream_unsync(stream: impl Stream<Item = T> + 'static) -> Self {
        let (read, write) = arc_signal(None);
        let mut stream = Box::pin(stream);
        Executor::spawn_local(async move {
            while let Some(value) = stream.next().await {
                write.set(Some(value));
            }
        });
        read.into()
    }
}

/// Checks whether a signal has already been disposed.
pub trait IsDisposed {
    /// If `true`, the value is gone: reads through the handle give `None`, writes do
    /// nothing.
    fn is_disposed(&self) -> bool;
}

/// Turns a signal back into a raw value.
pub trait IntoInner {
    /// The type of the value contained in the signal.
    type Value;

    /// Returns the inner value if this is the only reference to the signal.
    /// Otherwise, returns `None` and drops this reference.
    ///
    /// A lock poisoned by a panic still gives its value.
    fn into_inner(self) -> Option<Self::Value>;
}

/// Describes where the signal was defined. This is used for diagnostic warnings and is purely a
/// debug-mode tool.
pub trait DefinedAt {
    /// Returns the location at which the signal was defined. This is usually simply `None` in
    /// release mode.
    fn defined_at(&self) -> Option<&'static Location<'static>>;
}

/// A variation of the [`TryRead`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait TryReadValue: Sized + DefinedAt {
    /// The guard type that will be returned, which can be dereferenced to the value.
    type Value: Deref;

    /// Returns the non-reactive guard, or `None` if the value has already been disposed.
    #[track_caller]
    fn try_read_value(&self) -> Option<Self::Value>;
}

/// A variation of the [`TryWith`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait TryWithValue: DefinedAt {
    /// The type of the value contained in the value.
    type Value: ?Sized;

    /// Applies the closure to the value, non-reactively, and returns the result,
    /// or `None` if the value has already been disposed.
    #[track_caller]
    fn try_with_value<U>(
        &self,
        fun: impl FnOnce(&Self::Value) -> U,
    ) -> Option<U>;
}

impl<T> TryWithValue for T
where
    T: DefinedAt + TryReadValue,
{
    type Value = <<Self as TryReadValue>::Value as Deref>::Target;

    fn try_with_value<U>(
        &self,
        fun: impl FnOnce(&Self::Value) -> U,
    ) -> Option<U> {
        self.try_read_value().map(|value| fun(&value))
    }
}

/// A variation of the [`TryGet`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait TryGetValue: DefinedAt {
    /// The type of the value contained in the value.
    type Value: Clone;

    /// Clones and returns the value of the value, non-reactively,
    /// or `None` if the value has already been disposed.
    #[track_caller]
    fn try_get_value(&self) -> Option<Self::Value>;
}

impl<T> TryGetValue for T
where
    T: TryWithValue,
    T::Value: Clone,
{
    type Value = <Self as TryWithValue>::Value;

    fn try_get_value(&self) -> Option<Self::Value> {
        self.try_with_value(Self::Value::clone)
    }
}

/// A variation of the [`Write`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait WriteValue: Sized + DefinedAt {
    /// The type of the value's value.
    type Value: Sized + 'static;

    /// Returns a non-reactive write guard, or `None` if the value has already been disposed.
    #[track_caller]
    fn try_write_value(&self) -> Option<UntrackedWriteGuard<Self::Value>>;
}

/// A variation of the [`Update`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait UpdateValue: DefinedAt {
    /// The type of the value contained in the value.
    type Value;

    /// Updates the value, returning the value that is
    /// returned by the update function, or `None` if the value has already been disposed.
    #[track_caller]
    fn try_update_value<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U>;

    /// Updates the value. Through a weak handle whose value is gone, does nothing (reported
    /// once).
    #[track_caller]
    fn update_value(&self, fun: impl FnOnce(&mut Self::Value))
    where
        Self: IsDisposed,
    {
        if self.try_update_value(fun).is_none() && self.is_disposed() {
            crate::gone::report_gone(
                crate::gone::Attempt::Write,
                std::any::type_name::<Self>(),
                self.defined_at(),
                Location::caller(),
            );
        }
    }
}

impl<T> UpdateValue for T
where
    T: WriteValue,
{
    type Value = <Self as WriteValue>::Value;

    #[track_caller]
    fn try_update_value<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U> {
        let mut guard = self.try_write_value()?;
        Some(fun(&mut *guard))
    }
}

/// A variation of the [`Set`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
pub trait SetValue: DefinedAt {
    /// The type of the value contained in the value.
    type Value;

    /// Updates the value by replacing it, non-reactively.
    ///
    /// If the value has already been disposed, returns `Some(value)` with the value that was
    /// passed in. Otherwise, returns `None`.
    #[track_caller]
    fn try_set_value(&self, value: Self::Value) -> Option<Self::Value>;

    /// Updates the value by replacing it, non-reactively. Through a weak handle whose value
    /// is gone, does nothing (reported once).
    #[track_caller]
    fn set_value(&self, value: Self::Value)
    where
        Self: IsDisposed,
    {
        if self.try_set_value(value).is_some() && self.is_disposed() {
            crate::gone::report_gone(
                crate::gone::Attempt::Write,
                std::any::type_name::<Self>(),
                self.defined_at(),
                Location::caller(),
            );
        }
    }
}

impl<T> SetValue for T
where
    T: WriteValue,
{
    type Value = <Self as WriteValue>::Value;

    fn try_set_value(&self, value: Self::Value) -> Option<Self::Value> {
        // Unlike most other traits, for these None actually means success:
        if let Some(mut guard) = self.try_write_value() {
            *guard = value;
            None
        } else {
            Some(value)
        }
    }
}

/// Returns a non-reactive write guard to the value of a strong handle
/// ([`ArcStoredValue`](crate::owner::ArcStoredValue)).
pub trait StrongWriteValue: WriteValue + Strong {
    /// Returns the guard.
    #[track_caller]
    fn write_value(&self) -> UntrackedWriteGuard<<Self as WriteValue>::Value>;
}

impl<T: WriteValue + Strong> StrongWriteValue for T {
    #[track_caller]
    fn write_value(&self) -> UntrackedWriteGuard<<Self as WriteValue>::Value> {
        crate::gone::wait_for(self.defined_at(), || self.try_write_value())
    }
}

/// Returns a non-reactive guard to the value of a strong handle
/// ([`ArcStoredValue`](crate::owner::ArcStoredValue)).
pub trait ReadValue: TryReadValue + Strong {
    /// Returns the guard.
    #[track_caller]
    fn read_value(&self) -> <Self as TryReadValue>::Value;
}

impl<T: TryReadValue + Strong> ReadValue for T {
    #[track_caller]
    fn read_value(&self) -> <Self as TryReadValue>::Value {
        crate::gone::wait_for(self.defined_at(), || self.try_read_value())
    }
}

/// Applies a closure to the value of a strong handle, non-reactively.
pub trait WithValue: Strong {
    /// The type of the value.
    type Value: ?Sized;

    /// Applies the closure to the value and returns the result.
    #[track_caller]
    fn with_value<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U;
}

impl<T: TryReadValue + Strong> WithValue for T {
    type Value = <<T as TryReadValue>::Value as Deref>::Target;

    #[track_caller]
    fn with_value<U>(&self, fun: impl FnOnce(&Self::Value) -> U) -> U {
        fun(&self.read_value())
    }
}

/// Clones the value of a strong handle, non-reactively.
pub trait GetValue: Strong {
    /// The type of the value.
    type Value: Clone;

    /// Clones and returns the value.
    #[track_caller]
    fn get_value(&self) -> Self::Value;
}

impl<T> GetValue for T
where
    T: TryReadValue + Strong,
    <<T as TryReadValue>::Value as Deref>::Target: Clone,
{
    type Value = <<T as TryReadValue>::Value as Deref>::Target;

    #[track_caller]
    fn get_value(&self) -> Self::Value {
        self.with_value(Clone::clone)
    }
}
