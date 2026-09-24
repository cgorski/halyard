//! Deriving from one handle or a tuple of handles: [`Map::map`], [`Map::memo`], and a tuple
//! [`TryGetAll::try_get`].

use crate::{
    computed::Memo,
    traits::{TryGet, TryGetUntracked, TryRead},
    wrappers::read::Signal,
};
use std::ops::Deref;

/// Reads one handle, or a tuple of handles, by reference.
///
/// A tuple is read only if every handle in it has its value.
pub trait TryWithAll {
    /// The references the closure gets: `&T` for one handle, `(&A, &B, ...)` for a tuple.
    type Refs<'a>;

    /// Subscribes to the handles, applies the closure to their values, and returns the
    /// result, or `None` if any value is gone.
    #[track_caller]
    fn try_with_all<U>(
        &self,
        fun: impl FnOnce(Self::Refs<'_>) -> U,
    ) -> Option<U>;
}

impl<H> TryWithAll for H
where
    H: TryRead,
    <H::Value as Deref>::Target: 'static,
{
    type Refs<'a> = &'a <H::Value as Deref>::Target;

    #[track_caller]
    fn try_with_all<U>(
        &self,
        fun: impl FnOnce(Self::Refs<'_>) -> U,
    ) -> Option<U> {
        let value = self.try_read()?;
        Some(fun(&*value))
    }
}

/// Derives from one handle, or a tuple of handles, without cloning their values.
///
/// ```rust
/// # use halyard_reactive_graph::prelude::*;
/// # use halyard_reactive_graph::signal::RwSignal;
/// # let owner = halyard_reactive_graph::owner::Owner::new(); owner.set();
/// let count = RwSignal::new(2);
/// let double = count.map(|n| n * 2);
/// assert_eq!(double.try_get(), Some(4));
///
/// let name = RwSignal::new(String::from("Ada"));
/// let email = RwSignal::new(String::from("ada@example.com"));
/// let valid = (name, email)
///     .memo(|(name, email)| !name.trim().is_empty() && email.contains('@'));
/// assert_eq!(valid.try_get(), Some(true));
/// ```
///
/// The derived value is gone (reads give `None`, it renders nothing) while any source is
/// gone.
pub trait Map: TryWithAll + Clone + Send + Sync + 'static {
    /// A derived [`Signal`]: the closure runs on every read.
    #[track_caller]
    fn map<U>(
        self,
        fun: impl Fn(Self::Refs<'_>) -> U + Send + Sync + 'static,
    ) -> Signal<U>
    where
        U: Send + Sync + 'static,
    {
        Signal::derive_try(move || self.try_with_all(&fun))
    }

    /// A [`Memo`]: the closure runs when a source changes, and subscribers are notified only
    /// if its result changed.
    #[track_caller]
    fn memo<U>(
        self,
        fun: impl Fn(Self::Refs<'_>) -> U + Send + Sync + 'static,
    ) -> Memo<U>
    where
        U: PartialEq + Send + Sync + 'static,
    {
        Memo::new_try(move |_| self.try_with_all(&fun))
    }
}

impl<T: TryWithAll + Clone + Send + Sync + 'static> Map for T {}

/// Clones the values of a tuple of handles at once, for an event handler:
/// `let Some((name, email)) = (name, email).try_get() else { return };`.
pub trait TryGetAll {
    /// The tuple of values.
    type Value;

    /// Subscribes to the handles, then clones and returns their values, or `None` if any
    /// value is gone.
    #[track_caller]
    fn try_get(&self) -> Option<Self::Value>;

    /// Clones and returns the values, without tracking them, or `None` if any value is
    /// gone.
    #[track_caller]
    fn try_get_untracked(&self) -> Option<Self::Value>;
}

macro_rules! tuples {
    ($($name:ident $idx:tt),+) => {
        impl<$($name),+> TryWithAll for ($($name,)+)
        where
            $($name: TryRead, <$name::Value as Deref>::Target: 'static,)+
        {
            type Refs<'a> = ($(&'a <$name::Value as Deref>::Target,)+);

            #[track_caller]
            fn try_with_all<U>(
                &self,
                fun: impl FnOnce(Self::Refs<'_>) -> U,
            ) -> Option<U> {
                let guards = ($(self.$idx.try_read()?,)+);
                Some(fun(($(&*guards.$idx,)+)))
            }
        }

        impl<$($name),+> TryGetAll for ($($name,)+)
        where
            $($name: TryGet + TryGetUntracked<Value = <$name as TryGet>::Value>,)+
        {
            type Value = ($(<$name as TryGet>::Value,)+);

            #[track_caller]
            fn try_get(&self) -> Option<Self::Value> {
                Some(($(TryGet::try_get(&self.$idx)?,)+))
            }

            #[track_caller]
            fn try_get_untracked(&self) -> Option<Self::Value> {
                Some(($(TryGetUntracked::try_get_untracked(&self.$idx)?,)+))
            }
        }
    };
}

tuples!(A 0, B 1);
tuples!(A 0, B 1, C 2);
tuples!(A 0, B 1, C 2, D 3);
tuples!(A 0, B 1, C 2, D 3, E 4);
tuples!(A 0, B 1, C 2, D 3, E 4, F 5);
tuples!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuples!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
