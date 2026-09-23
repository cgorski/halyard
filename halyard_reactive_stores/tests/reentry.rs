//! No subscriber runs while a store's value is locked (docs/no-panics.md, "Structural changes"
//! 2): a subscriber that runs at once (an `ImmediateEffect`) can read the store it was
//! notified by. Each of these failed on the previous code.

use halyard_reactive_graph::{
    effect::ImmediateEffect,
    owner::Owner,
    traits::{ReadUntracked, Track, Write},
};
use halyard_reactive_stores::{AtKeyed, Patch, Store};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Store, Patch)]
struct Todo {
    id: usize,
    label: String,
}

#[derive(Debug, Clone, Store, Patch)]
struct Todos {
    #[store(key: usize = |todo| todo.id)]
    todos: Vec<Todo>,
}

#[derive(Debug, Clone, Store, Patch)]
struct Settings {
    title: String,
    size: usize,
}

fn todos(ids: &[usize]) -> Vec<Todo> {
    ids.iter()
        .map(|&id| Todo {
            id,
            label: format!("#{id}"),
        })
        .collect()
}

/// An effect that tracks what `track` tracks and records what `read` reads, each run.
fn watch<T: Send + 'static>(
    track: impl Fn() + Send + Sync + 'static,
    read: impl Fn() -> T + Send + Sync + 'static,
) -> (ImmediateEffect, Arc<Mutex<Vec<T>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let effect = ImmediateEffect::new({
        let seen = Arc::clone(&seen);
        move || {
            track();
            let value = read();
            if let Ok(mut seen) = seen.lock() {
                seen.push(value);
            }
        }
    });
    (effect, seen)
}

fn seen<T: Clone>(seen: &Mutex<Vec<T>>) -> Vec<T> {
    seen.lock().map(|s| s.clone()).unwrap_or_default()
}

/// The effect first saw `before`, then ran again (once per trigger notified: an
/// `ImmediateEffect` is not batched) and each time saw `after`.
fn saw<T: PartialEq + std::fmt::Debug>(seen: &[T], before: T, after: T) {
    assert!(seen.len() > 1, "the effect ran again: {seen:?}");
    assert_eq!(seen.first(), Some(&before), "{seen:?}");
    assert!(seen.iter().skip(1).all(|s| *s == after), "{seen:?}");
}

/// Before: `patch` notified while it held the write lock, so the effect's read was refused
/// (logged as re-entry) and it saw no value.
#[test]
fn a_subscriber_notified_by_a_patch_reads_the_new_value() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Settings {
        title: "a".into(),
        size: 1,
    });
    let (_effect, titles) = watch(
        move || store.title().track(),
        move || store.title().try_read_untracked().map(|t| t.clone()),
    );

    store.patch(Settings {
        title: "b".into(),
        size: 1,
    });

    saw(&seen(&titles), Some("a".to_string()), Some("b".to_string()));
}

/// Before: as above, for the keyed patch.
#[test]
fn a_subscriber_notified_by_a_keyed_patch_reads_the_new_value() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });
    let two = AtKeyed::new(store.todos(), 2);
    let (_effect, labels) = watch(
        move || two.track(),
        move || two.try_read_untracked().map(|t| t.label.clone()),
    );

    let mut new = todos(&[1, 2]);
    if let Some(todo) = new.get_mut(1) {
        todo.label = "two".into();
    }
    store.todos().patch(new);

    saw(
        &seen(&labels),
        Some("#2".to_string()),
        Some("two".to_string()),
    );
}

/// Before: a write to a keyed field notified its subscribers before its keys were updated,
/// so the row for key 3 looked its entry up at its old index: past the end of the list (a
/// panic) or at another entry.
#[test]
fn a_subscriber_notified_by_a_keyed_write_finds_entries_by_their_new_keys() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2, 3]),
    });
    let three = AtKeyed::new(store.todos(), 3);
    let (_effect, ids) = watch(
        move || three.track(),
        move || three.try_read_untracked().map(|t| t.id),
    );

    store.todos().write().remove(0);

    saw(&seen(&ids), Some(3), Some(3));
}
