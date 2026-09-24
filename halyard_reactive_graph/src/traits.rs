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
//! | [`Write`]     | Guard | Gives a guard over a copy of the value, committed when it is dropped; replaces the value.
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
//! | [`Update`]          | `fn(&mut T)`  | [`Write`] + [`Clone`]             | Applies closure to a copy of the value and commits it, notifying subscribers (`update_untracked`: without).
//! | [`Set`]             | `T`           | [`Write`]                         | Replaces the value, and notifies subscribers (`set_untracked`: without).
//!
//! No value is ever lent out for a change in place: every write is a replacement ([`Set`])
//! or a change to a copy committed afterwards ([`Update`], [`Write::try_write`]), which
//! needs `T: Clone`. A value that is not `Clone` is changed by replacing it with `set`.
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
//! - The [`Write`] guard of a signal holds a copy of the value, committed (or deferred, as
//!   above) when it is dropped: no lock is held while it is alive.
//! - Nothing changes a value in place, so a read never finds its value lent out: inside any
//!   write, reading the same value (through any handle to it) gives the committed value.
//! - Writes from other threads wait for their turn: updates of one signal serialize, none is
//!   lost. Reads never wait for an update's closure.
//! - A memo read inside its own computation gives its previous value (reported once as a
//!   cycle). During its first computation it has none: the `try_*` reads give `None`, and a
//!   strong read aborts (see [`Strong`]).
//!
//! Stored values ([`TryWithValue`], [`UpdateValue`], [`TryReadValue`], [`WriteValue`]) are not
//! signals, but are written the same way: `update_value` and the `write_value` guard work on
//! a copy, and `set_value` replaces the value. A write made from inside the same value's
//! `with_value` (while its read guard is alive) is refused and logged.
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
    signal::{arc_signal, guards::CopyWriteGuard, ArcReadSignal},
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
/// No value is ever lent out for a change in place, so a strong read always finds a value:
/// inside an update of the same value (through any handle to it) it gives the committed
/// value, and a memo read inside its own computation gives its previous value. On a server
/// it may wait, briefly, while another thread computes a memo's value.
///
/// # Aborts
///
/// One read has no possible value: a strong read of a memo inside the memo's own **first**
/// computation, when it has no previous value (or inside any computation of a memo made with
/// `new_owning`, whose function owns the previous value). Like unbounded recursion, that is a
/// cycle in the program's logic: it is reported, naming where the memo was created and where
/// it is read, and the process aborts (`std::process::abort`). This is the only abort in
/// halyard (docs/no-panics.md). Read a memo inside its own computation through its weak
/// handle (`try_get`, which gives `None` there), or use the previous value that the memo's
/// function receives.
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
/// [`std::sync::Weak`]. Reads return an `Option` (`try_*`), and a write to a gone value does
/// nothing (reported once).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a weak (arena) handle",
    note = "take a weak handle with `.downgrade()`"
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

/// Gives a guard through which a signal's value can be changed, and the hooks that every
/// write is built on. When the guard is dropped, the change is committed and the signal's
/// subscribers are notified.
///
/// No value is ever lent out for a change in place: the guard holds a copy of the value
/// (so it needs `Value: Clone`), committed when it is dropped, and no lock is held while it
/// is alive; [`Set`] replaces the value, and [`Update`] changes a copy (see the module docs,
/// "Re-entry").
pub trait Write: Sized + DefinedAt + Notify {
    /// The type of the signal's value.
    type Value: Sized + 'static;

    /// Returns the guard, over a copy of the value, or `None` if the value is gone.
    fn try_write(&self) -> Option<impl UntrackableGuard<Target = Self::Value>>
    where
        Self::Value: Clone;

    /// Replaces the value, notifying subscribers if `notify` ([`Set`] is built on it). Gives
    /// the value back if it could not be written (the value is gone, or this thread is using
    /// a value that cannot defer the write).
    #[doc(hidden)]
    fn try_commit_value(
        &self,
        value: Self::Value,
        notify: bool,
    ) -> Option<Self::Value>;

    /// Runs `fun` on a copy of the value, outside every lock, and commits the result,
    /// notifying subscribers if `fun` returns `(true, _)` ([`Update`] is built on it). `None`
    /// if the value is gone or the result could not be committed.
    #[doc(hidden)]
    fn try_update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> (bool, U),
    ) -> Option<U>
    where
        Self::Value: Clone;
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

/// Updates the value of a signal by applying a function that changes it, notifying its
/// subscribers that the value has changed.
///
/// Every form runs its closure on a copy of the committed value, outside every lock, then
/// commits the result: nothing is lent out for a change in place. Inside the closure,
/// reading the same value (through any handle to it) gives its last committed value; writing
/// it is deferred until the update has committed. They need `Value: Clone`; a value that is
/// not `Clone` is changed by replacing it ([`Set`]). Through a weak handle whose value is
/// gone, they do nothing (`update`, `maybe_update` and `update_untracked` report that once;
/// `try_update` returns `None`).
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
    /// returns `true`. The result is committed either way.
    ///
    /// The closure runs on a copy of the committed value; see the trait docs.
    #[track_caller]
    fn maybe_update(&self, fun: impl FnOnce(&mut Self::Value) -> bool)
    where
        Self::Value: Clone;

    /// Updates the value of the signal without notifying subscribers.
    ///
    /// The closure runs on a copy of the committed value; see the trait docs.
    #[track_caller]
    fn update_untracked(&self, fun: impl FnOnce(&mut Self::Value))
    where
        Self::Value: Clone,
    {
        self.maybe_update(|val| {
            fun(val);
            false
        });
    }

    /// Updates the value of the signal and notifies subscribers, returning what the closure
    /// returns, or `None` if the value is gone or the update could not be committed.
    ///
    /// The closure runs on a copy of the committed value; see the trait docs.
    #[track_caller]
    fn try_update<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U>
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

    #[track_caller]
    fn try_update<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U>
    where
        Self::Value: Clone,
    {
        self.try_update_snapshot(|val| (true, fun(val)))
    }
}

/// Updates the value of the signal by replacing it. This works for every value, `Clone` or
/// not.
pub trait Set {
    /// The type of the value contained in the signal.
    type Value;

    /// Updates the value by replacing it, and notifies subscribers that it has changed.
    fn set(&self, value: Self::Value);

    /// Updates the value by replacing it, without notifying subscribers. Through a weak
    /// handle whose value is gone, does nothing (reported once).
    fn set_untracked(&self, value: Self::Value);

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
        let failed = self.try_commit_value(value, true).is_some();
        report_failed_set::<Self>(self, failed, Location::caller());
    }

    #[track_caller]
    fn set_untracked(&self, value: Self::Value) {
        let failed = self.try_commit_value(value, false).is_some();
        report_failed_set::<Self>(self, failed, Location::caller());
    }

    #[track_caller]
    fn try_set(&self, value: Self::Value) -> Option<Self::Value> {
        if self.is_disposed() {
            Some(value)
        } else {
            self.try_commit_value(value, true)
        }
    }
}

/// Reports a `set` that could not be made: once per call site if the value is gone, and in
/// debug builds otherwise.
fn report_failed_set<T>(
    this: &T,
    failed: bool,
    called_at: &'static Location<'static>,
) where
    T: Write + IsDisposed,
{
    if failed && this.is_disposed() {
        crate::gone::report_gone(
            crate::gone::Attempt::Write,
            std::any::type_name::<T>(),
            this.defined_at(),
            called_at,
        );
        return;
    }

    #[cfg(any(debug_assertions, halyard_debuginfo))]
    if failed {
        let ty = std::any::type_name::<T::Value>();

        crate::log_warning(format_args!(
            "At {called_at}, you tried to update a {ty}, but the update \
             failed. This can happen if this thread is using a value that \
             is not a signal (a resource, an async derived or a mapped \
             signal) while it writes it: inside its `with`, or while a guard \
             of it is alive."
        ));
    }
    #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
    {
        _ = called_at;
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
///
/// Nothing is lent out for a change in place: the guard holds a copy of the value, which
/// replaces it when the guard is dropped.
pub trait WriteValue: Sized + DefinedAt {
    /// The type of the value's value.
    type Value: Sized + 'static;

    /// Returns a non-reactive guard over a copy of the value, which replaces the value when
    /// it is dropped, or `None` if the value has already been disposed.
    #[track_caller]
    fn try_write_value(&self) -> Option<CopyWriteGuard<Self::Value>>
    where
        Self::Value: Clone;

    /// Swaps `value` in as the value (`value` then holds the previous one, to drop).
    /// `false` if the value is gone, or if this thread is using it (inside its own
    /// `with_value`, or while a read guard of it is alive; that is logged once).
    #[doc(hidden)]
    fn try_swap_value(&self, value: &mut Self::Value) -> bool;
}

/// A variation of the [`Update`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`).
///
/// The closure runs on a copy of the value, which then replaces it: inside the closure,
/// reading the same value gives the value as it was. Needs `Value: Clone`; a value that is
/// not `Clone` is changed by replacing it ([`SetValue`]).
pub trait UpdateValue: DefinedAt {
    /// The type of the value contained in the value.
    type Value;

    /// Updates the value, returning the value that is returned by the update function, or
    /// `None` if the value has already been disposed or the result could not be stored.
    #[track_caller]
    fn try_update_value<U>(
        &self,
        fun: impl FnOnce(&mut Self::Value) -> U,
    ) -> Option<U>
    where
        Self::Value: Clone;

    /// Updates the value. Through a weak handle whose value is gone, does nothing (reported
    /// once).
    #[track_caller]
    fn update_value(&self, fun: impl FnOnce(&mut Self::Value))
    where
        Self: IsDisposed,
        Self::Value: Clone,
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
    ) -> Option<U>
    where
        Self::Value: Clone,
    {
        let mut guard = self.try_write_value()?;
        let out = fun(&mut guard);
        guard.commit().then_some(out)
    }
}

/// A variation of the [`Set`] trait that provides a signposted "always-non-reactive" API.
/// E.g. for [`StoredValue`](`crate::owner::StoredValue`). This works for every value,
/// `Clone` or not.
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
        let mut value = value;
        if self.try_swap_value(&mut value) {
            // the previous value
            drop(value);
            None
        } else {
            Some(value)
        }
    }
}

/// Returns a non-reactive write guard, over a copy of the value, to the value of a strong
/// handle ([`ArcStoredValue`](crate::owner::ArcStoredValue)).
pub trait StrongWriteValue: WriteValue + Strong {
    /// Returns the guard.
    #[track_caller]
    fn write_value(&self) -> CopyWriteGuard<<Self as WriteValue>::Value>
    where
        <Self as WriteValue>::Value: Clone;
}

impl<T: WriteValue + Strong> StrongWriteValue for T {
    #[track_caller]
    fn write_value(&self) -> CopyWriteGuard<<Self as WriteValue>::Value>
    where
        <Self as WriteValue>::Value: Clone,
    {
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
