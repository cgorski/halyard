use crate::Store;
use halyard_reactive_graph::{
    owner::{LocalStorage, SyncStorage},
    traits::With,
};
use serde::{ser::Error as _, Deserialize, Serialize, Serializer};

/// Serializes the store's value (tracking it, as `with` does). A store whose value is gone
/// (its owner was disposed), or is being written by this thread, has no value to serialize:
/// that is a serialization error, not a panic.
impl<T: Serialize> Serialize for Store<T>
where
    Store<T>: With<Value = T>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut serializer = Some(serializer);
        self.try_with(|item| serializer.take().map(|s| item.serialize(s)))
            .flatten()
            .unwrap_or_else(|| {
                Err(S::Error::custom(
                    "the store has no value to serialize: its owner was disposed, or \
                     this thread is writing it",
                ))
            })
    }
}

impl<'de, T: Deserialize<'de> + Send + Sync + 'static> Deserialize<'de>
    for Store<T, SyncStorage>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(|inner| Store::new(inner))
    }
}

impl<'de, T: Deserialize<'de> + 'static> Deserialize<'de>
    for Store<T, LocalStorage>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(|inner| Store::new_local(inner))
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use halyard::serde_json;
    use halyard_reactive_graph::{owner::Owner, traits::Dispose};

    #[test]
    fn a_store_serializes_its_value() {
        let owner = Owner::new();
        owner.set();
        let store = Store::new(vec![1, 2]);
        assert_eq!(
            serde_json::to_string(&store).ok().as_deref(),
            Some("[1,2]")
        );
    }

    /// Before: `with` on a disposed store panicked inside `serialize`.
    #[test]
    fn a_disposed_store_is_a_serialization_error() {
        let owner = Owner::new();
        owner.set();
        let store = Store::new(vec![1, 2]);
        store.dispose();

        let error = serde_json::to_string(&store).err().map(|e| e.to_string());

        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains("owner was disposed")),
            "{error:?}"
        );
    }
}
