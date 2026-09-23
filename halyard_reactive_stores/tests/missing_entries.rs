//! A store field whose value is not there (an `Option` that is `None`, an index past the end,
//! a key that is not in the collection, a disposed store) has no value: its `try_*` accessors
//! return `None`, a write does nothing, and nothing panics.
//!
//! Every test named `..._before_panicked` panicked on the previous code; the others are noted.

use halyard_reactive_graph::{
    effect::ImmediateEffect,
    owner::Owner,
    traits::{
        Dispose, GetUntracked, Notify, ReadUntracked, Set, Track, Update,
        UpdateUntracked, Write,
    },
};
use halyard_reactive_stores::{
    AtIndex, AtKeyed, OptionStoreExt, Store, StoreFieldIterator,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[derive(Debug, Clone, PartialEq, Store)]
struct Todo {
    id: usize,
    label: String,
}

#[derive(Debug, Clone, Store)]
struct Todos {
    #[store(key: usize = |todo| todo.id)]
    todos: Vec<Todo>,
}

#[derive(Debug, Clone, Store)]
struct TodoMaps {
    #[store(key: usize = |(id, _)| *id)]
    by_id: BTreeMap<usize, Todo>,
    #[store(key: String = |(label, _)| label.clone())]
    by_label: HashMap<String, Todo>,
}

#[derive(Debug, Clone, Store)]
struct User {
    name: Option<Name>,
}

#[derive(Debug, Clone, PartialEq, Store)]
struct Name {
    first: String,
}

#[derive(Debug, Clone, Store)]
struct Numbers {
    numbers: Vec<i32>,
    pair: [i32; 2],
}

fn todos(ids: &[usize]) -> Vec<Todo> {
    ids.iter()
        .map(|&id| Todo {
            id,
            label: format!("#{id}"),
        })
        .collect()
}

fn todo_maps(ids: &[usize]) -> TodoMaps {
    TodoMaps {
        by_id: todos(ids).into_iter().map(|t| (t.id, t)).collect(),
        by_label: todos(ids)
            .into_iter()
            .map(|t| (t.label.clone(), t))
            .collect(),
    }
}

/// An effect that tracks what `track` tracks, and the number of times it ran.
fn runs(
    track: impl Fn() + Send + Sync + 'static,
) -> (ImmediateEffect, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let effect = ImmediateEffect::new({
        let count = Arc::clone(&count);
        move || {
            track();
            count.fetch_add(1, Ordering::Relaxed);
        }
    });
    (effect, count)
}

fn count(runs: &AtomicUsize) -> usize {
    runs.load(Ordering::Relaxed)
}

// ----- OptionStoreExt::unwrap (option.rs) -----

#[test]
fn unwrapped_option_while_none_has_no_value_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(User { name: None });
    let name = store.name().unwrap();

    assert_eq!(name.try_get_untracked(), None);
    assert!(name.try_read_untracked().is_none());
    assert_eq!(name.first().try_get_untracked(), None);
    assert!(name.try_update(|n| n.first.push('!')).is_none());
    name.first().set("ignored".into());
    assert!(
        store.name().get_untracked().is_none(),
        "the write does nothing"
    );

    store.name().set(Some(Name {
        first: "Ada".into(),
    }));
    assert_eq!(name.first().try_get_untracked().as_deref(), Some("Ada"));
    name.first().update(|first| first.push('!'));
    assert_eq!(name.first().try_get_untracked().as_deref(), Some("Ada!"));
}

/// A subfield taken while the option held a value, read after it was emptied: the case of a
/// row rendered for `Some` whose effect runs once more after the value went away.
#[test]
fn unwrapped_option_emptied_after_it_was_taken_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(User {
        name: Some(Name {
            first: "Ada".into(),
        }),
    });
    let first = store.name().map_untracked(|name| name.first());
    assert!(first.is_some());

    store.name().set(None);

    assert_eq!(first.and_then(|first| first.try_get_untracked()), None);
}

/// A refused write notifies nothing: no subscriber of the option runs for it.
#[test]
fn unwrapped_option_refused_write_notifies_nothing() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(User { name: None });
    let (_effect, option_runs) = runs(move || store.name().track());
    assert_eq!(count(&option_runs), 1);

    store.name().unwrap().first().set("ignored".into());

    assert_eq!(count(&option_runs), 1);
}

// ----- AtIndex (iter.rs; the indexing was invisible to the panic ratchet) -----

#[test]
fn index_past_the_end_has_no_value_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Numbers {
        numbers: vec![1, 2, 3],
        pair: [1, 2],
    });
    let (_effect, list_runs) = runs(move || store.numbers().track());

    let fifth = store.numbers().at_unkeyed(5);
    assert_eq!(fifth.try_get_untracked(), None);
    assert!(fifth.try_update(|n| *n += 1).is_none());
    fifth.set(10);
    assert_eq!(store.numbers().get_untracked(), vec![1, 2, 3]);
    assert_eq!(count(&list_runs), 1, "a refused write notifies nothing");

    assert_eq!(store.numbers().at_unkeyed(2).try_get_untracked(), Some(3));
    // an array field (`AtIndex` now checks the length, which arrays have too)
    assert_eq!(AtIndex::new(store.pair(), 1).try_get_untracked(), Some(2));
    assert_eq!(AtIndex::new(store.pair(), 2).try_get_untracked(), None);
}

/// A row of `iter_unkeyed` whose item was removed: its effect may run once more before the
/// row is disposed.
#[test]
fn removed_unkeyed_row_has_no_value_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Numbers {
        numbers: vec![1, 2, 3],
        pair: [1, 2],
    });
    let rows = store.numbers().iter_unkeyed().collect::<Vec<_>>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _effect = ImmediateEffect::new({
        let seen = Arc::clone(&seen);
        let last = rows[2];
        move || {
            last.track();
            if let Ok(mut seen) = seen.lock() {
                seen.push(last.try_get_untracked());
            }
        }
    });

    store.numbers().write().pop();

    let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
    assert_eq!(seen, vec![Some(3), None]);
}

/// The iterator hands out every index once, from either end (a `Range` now, not arithmetic).
#[test]
fn iter_unkeyed_hands_out_every_index_once_from_either_end() {
    let store = Store::new(Numbers {
        numbers: vec![1, 2, 3, 4],
        pair: [1, 2],
    });
    let mut rows = store.numbers().iter_unkeyed();
    let mut seen = Vec::new();
    seen.extend(rows.next().and_then(|r| r.try_get_untracked()));
    seen.extend(rows.next_back().and_then(|r| r.try_get_untracked()));
    seen.extend(rows.next().and_then(|r| r.try_get_untracked()));
    seen.extend(rows.next_back().and_then(|r| r.try_get_untracked()));
    assert!(rows.next().is_none());
    assert!(rows.next_back().is_none());
    assert_eq!(seen, vec![1, 4, 2, 3]);

    let reversed = store
        .numbers()
        .iter_unkeyed()
        .rev()
        .filter_map(|r| r.try_get_untracked())
        .collect::<Vec<_>>();
    assert_eq!(reversed, vec![4, 3, 2, 1]);
}

// ----- AtKeyed over maps (keyed.rs: the `expect`s of KeyedAccess) -----

#[test]
fn btree_map_key_removed_through_the_store_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(todo_maps(&[1, 2]));
    let two = AtKeyed::new(store.by_id(), 2);
    assert_eq!(two.try_get_untracked().map(|t| t.id), Some(2));

    store.write().by_id.remove(&2);

    assert_eq!(two.try_get_untracked(), None);
    assert!(two.try_update(|t| t.label.push('!')).is_none());
    two.label().set("ignored".into());
    assert_eq!(store.by_id().get_untracked().len(), 1);
}

#[test]
fn hash_map_key_removed_through_the_store_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(todo_maps(&[1, 2]));
    let two = AtKeyed::new(store.by_label(), "#2".to_string());
    assert_eq!(two.try_get_untracked().map(|t| t.id), Some(2));

    store.write().by_label.remove("#2");

    assert_eq!(two.try_get_untracked(), None);
    assert!(two.try_update_untracked(|t| t.id = 0).is_none());
    assert_eq!(store.by_label().get_untracked().len(), 1);
}

// ----- AtKeyed over a Vec: stale positions (keyed.rs; `Vec::index` was invisible to the
// ratchet) -----

#[test]
fn vec_entry_removed_through_the_store_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2, 3]),
    });
    let three = AtKeyed::new(store.todos(), 3);
    assert_eq!(three.try_get_untracked().map(|t| t.id), Some(3));

    store.write().todos.truncate(1);

    assert_eq!(three.try_get_untracked(), None);
    three.label().set("ignored".into());
    assert_eq!(store.todos().get_untracked(), todos(&[1]));
}

/// Before: the entry at the old position was given, whatever its key: here the row for key 3
/// read (and a write would have changed) the entry with key 1.
#[test]
fn vec_entries_reordered_through_the_store_give_the_right_entry() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2, 3]),
    });
    let three = AtKeyed::new(store.todos(), 3);
    assert_eq!(three.try_get_untracked().map(|t| t.id), Some(3));

    store.write().todos.reverse();

    assert_eq!(three.try_get_untracked().map(|t| t.id), Some(3));
    three.label().set("three".into());
    let labels = store
        .todos()
        .get_untracked()
        .into_iter()
        .map(|t| t.label)
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["three", "#2", "#1"]);
}

/// Before: a key added through the store (not through the keyed field) was not found until
/// the keyed field was iterated.
#[test]
fn vec_entry_added_through_the_store_is_found() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos { todos: todos(&[1]) });
    let four = AtKeyed::new(store.todos(), 4);
    assert_eq!(four.try_get_untracked(), None);

    store.write().todos.extend(todos(&[4]));

    assert_eq!(four.try_get_untracked().map(|t| t.id), Some(4));
}

/// Before: the write resolved its key while holding the store's write lock; on a store whose
/// keys were never read, that read of the collection was refused (logged as re-entry), so the
/// key was not found and the write was silently dropped.
#[test]
fn first_write_at_a_key_is_applied() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });

    AtKeyed::new(store.todos(), 2).label().set("two".into());

    assert_eq!(
        store
            .todos()
            .get_untracked()
            .get(1)
            .map(|t| t.label.clone()),
        Some("two".to_string())
    );
}

// ----- a disposed store (keyed.rs: `keys().expect(..)` in path, path_unkeyed, update_keys) -----

#[test]
fn keyed_field_of_a_disposed_store_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos { todos: todos(&[1]) });
    let one = AtKeyed::new(store.todos(), 1);
    let field = store.todos();
    store.dispose();

    one.track();
    one.notify();
    assert_eq!(one.try_get_untracked(), None);
    one.label().set("ignored".into());
    field.update_keys();
    assert_eq!(field.into_iter().count(), 0);
}

#[cfg(feature = "slotmap")]
mod slotmap_fields {
    use super::*;
    use slotmap::{DefaultKey, SlotMap};

    #[derive(Debug, Store)]
    struct Slots {
        #[store(key: DefaultKey = |(key, _)| key)]
        todos: SlotMap<DefaultKey, Todo>,
    }

    /// Before: `KeyedAccess` for the slot maps `expect`ed the key (not counted by the
    /// ratchet, which builds without the `slotmap` feature).
    #[test]
    fn slot_map_key_removed_through_the_store_before_panicked() {
        let owner = Owner::new();
        owner.set();
        let mut todos = SlotMap::new();
        let key = todos.insert(Todo {
            id: 1,
            label: "#1".into(),
        });
        let store = Store::new(Slots { todos });
        let entry = AtKeyed::new(store.todos(), key);
        assert_eq!(entry.try_get_untracked().map(|t| t.id), Some(1));

        store.write().todos.remove(key);

        assert_eq!(entry.try_get_untracked(), None);
    }
}
