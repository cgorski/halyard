use crate::traits::{
    DefinedAt, Track, TryGet, TryGetUntracked, TryRead, TryReadUntracked,
    TryWith, TryWithUntracked,
};
use std::panic::Location;

impl<T> DefinedAt for Option<T>
where
    T: DefinedAt,
{
    fn defined_at(&self) -> Option<&'static Location<'static>> {
        self.as_ref().map(DefinedAt::defined_at).unwrap_or(None)
    }
}

impl<T> Track for Option<T>
where
    T: Track,
{
    fn track(&self) {
        if let Some(signal) = self {
            signal.track();
        }
    }
}

/// An alternative [`TryReadUntracked`](crate) trait that works with `Option<Readable>` types.
pub trait ReadUntrackedOptional: Sized + DefinedAt {
    /// The guard type that will be returned, which can be dereferenced to the value.
    type Value;

    /// Returns the guard, or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_read_untracked(&self) -> Option<Self::Value>;
}

impl<T> ReadUntrackedOptional for Option<T>
where
    Self: DefinedAt,
    T: TryReadUntracked,
{
    type Value = Option<<T as TryReadUntracked>::Value>;

    fn try_read_untracked(&self) -> Option<Self::Value> {
        Some(if let Some(signal) = self {
            Some(signal.try_read_untracked()?)
        } else {
            None
        })
    }
}

/// An alternative [`TryRead`](crate) trait that works with `Option<Readable>` types.
pub trait ReadOptional: DefinedAt {
    /// The guard type that will be returned, which can be dereferenced to the value.
    type Value;

    /// Subscribes to the signal, and returns the guard, or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_read(&self) -> Option<Self::Value>;
}

impl<T> ReadOptional for Option<T>
where
    Self: DefinedAt,
    T: TryRead,
{
    type Value = Option<<T as TryRead>::Value>;

    fn try_read(&self) -> Option<Self::Value> {
        Some(if let Some(readable) = self {
            Some(readable.try_read()?)
        } else {
            None
        })
    }
}

/// An alternative [`TryWithUntracked`](crate) trait that works with `Option<Withable>` types.
pub trait WithUntrackedOptional: DefinedAt {
    /// The type of the value contained in the signal.
    type Value: ?Sized;

    /// Applies the closure to the value, and returns the result,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_with_untracked<U>(
        &self,
        fun: impl FnOnce(Option<&Self::Value>) -> U,
    ) -> Option<U>;
}

impl<T> WithUntrackedOptional for Option<T>
where
    Self: DefinedAt,
    T: TryWithUntracked,
    <T as TryWithUntracked>::Value: Sized,
{
    type Value = <T as TryWithUntracked>::Value;

    fn try_with_untracked<U>(
        &self,
        fun: impl FnOnce(Option<&Self::Value>) -> U,
    ) -> Option<U> {
        if let Some(signal) = self {
            Some(signal.try_with_untracked(|val| fun(Some(val)))?)
        } else {
            Some(fun(None))
        }
    }
}

/// An alternative [`TryWith`](crate) trait that works with `Option<Withable>` types.
pub trait WithOptional: DefinedAt {
    /// The type of the value contained in the signal.
    type Value: ?Sized;

    /// Subscribes to the signal, applies the closure to the value, and returns the result,
    /// or `None` if the signal has already been disposed.
    #[track_caller]
    fn try_with<U>(
        &self,
        fun: impl FnOnce(Option<&Self::Value>) -> U,
    ) -> Option<U>;
}

impl<T> WithOptional for Option<T>
where
    Self: DefinedAt,
    T: TryWith,
    <T as TryWith>::Value: Sized,
{
    type Value = <T as TryWith>::Value;

    fn try_with<U>(
        &self,
        fun: impl FnOnce(Option<&Self::Value>) -> U,
    ) -> Option<U> {
        if let Some(signal) = self {
            Some(signal.try_with(|val| fun(Some(val)))?)
        } else {
            Some(fun(None))
        }
    }
}

impl<T> TryGetUntracked for Option<T>
where
    Self: DefinedAt,
    T: TryGetUntracked,
{
    type Value = Option<<T as TryGetUntracked>::Value>;

    fn try_get_untracked(&self) -> Option<Self::Value> {
        Some(if let Some(signal) = self {
            Some(signal.try_get_untracked()?)
        } else {
            None
        })
    }
}

impl<T> TryGet for Option<T>
where
    Self: DefinedAt,
    T: TryGet,
{
    type Value = Option<<T as TryGet>::Value>;

    fn try_get(&self) -> Option<Self::Value> {
        Some(if let Some(signal) = self {
            Some(signal.try_get()?)
        } else {
            None
        })
    }
}

/// Helper trait to implement flatten() on `Option<&Option<T>>`.
pub trait FlattenOptionRefOption {
    /// The type of the value contained in the double option.
    type Value;

    /// Converts from `Option<&Option<T>>` to `Option<&T>`.
    fn flatten(&self) -> Option<&Self::Value>;
}

impl<'a, T> FlattenOptionRefOption for Option<&'a Option<T>> {
    type Value = T;

    fn flatten(&self) -> Option<&'a T> {
        self.map(Option::as_ref).flatten()
    }
}
