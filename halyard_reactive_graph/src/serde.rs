#[allow(deprecated)]
use crate::wrappers::read::{MaybeProp, MaybeSignal};
use crate::{
    computed::{ArcMemo, Memo},
    owner::Storage,
    signal::{ArcReadSignal, ArcRwSignal, ReadSignal, RwSignal},
    traits::TryWith,
    wrappers::read::{Signal, SignalTypes},
};
use serde::{ser::Error as _, Deserialize, Serialize};

/// Serializes the value a reactive value holds (tracking it, as `with` does). A reactive value
/// whose value is gone (its owner was disposed), or is being written by this thread, has no
/// value to serialize: that is a serialization error, not a panic.
fn serialize_value<R, S>(reactive: &R, serializer: S) -> Result<S::Ok, S::Error>
where
    R: TryWith + ?Sized,
    R::Value: Serialize,
    S: serde::Serializer,
{
    let mut serializer = Some(serializer);
    reactive
        .try_with(|value| serializer.take().map(|s| value.serialize(s)))
        .flatten()
        .unwrap_or_else(|| {
            Err(S::Error::custom(
                "the reactive value has no value to serialize: its owner was \
                 disposed, or this thread is writing it",
            ))
        })
}

impl<T, St> Serialize for ReadSignal<T, St>
where
    T: Serialize + 'static,
    St: Storage<ArcReadSignal<T>>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T, St> Serialize for RwSignal<T, St>
where
    T: Serialize + 'static,
    St: Storage<ArcRwSignal<T>>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T, St> Serialize for Memo<T, St>
where
    T: Serialize + 'static,
    St: Storage<ArcMemo<T, St>> + Storage<T>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T: Serialize + 'static> Serialize for ArcReadSignal<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T: Serialize + 'static> Serialize for ArcRwSignal<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T: Serialize + 'static, St: Storage<T>> Serialize for ArcMemo<T, St> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

#[allow(deprecated)]
impl<T, St> Serialize for MaybeSignal<T, St>
where
    T: Clone + Send + Sync + Serialize,
    St: Storage<SignalTypes<T, St>> + Storage<T>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

impl<T, St> Serialize for MaybeProp<T, St>
where
    T: Send + Sync + Serialize,
    St: Storage<SignalTypes<Option<T>, St>> + Storage<Option<T>>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            None => None::<T>.serialize(serializer),
            Some(signal) => serialize_value(signal, serializer),
        }
    }
}

impl<T, St> Serialize for Signal<T, St>
where
    T: Send + Sync + Serialize + 'static,
    St: Storage<SignalTypes<T, St>> + Storage<T>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_value(self, serializer)
    }
}

/* Deserialization for signal types */

impl<'de, T, S> Deserialize<'de> for RwSignal<T, S>
where
    T: Send + Sync + Deserialize<'de> + 'static,
    S: Storage<ArcRwSignal<T>>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(RwSignal::new_with_storage)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ArcRwSignal<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(ArcRwSignal::new)
    }
}

#[allow(deprecated)]
impl<'de, T: Deserialize<'de>, St> Deserialize<'de> for MaybeSignal<T, St>
where
    St: Storage<T>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(MaybeSignal::Static)
    }
}

#[allow(deprecated)]
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Signal<T>
where
    T: Send + Sync + Serialize + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Signal::stored)
    }
}
