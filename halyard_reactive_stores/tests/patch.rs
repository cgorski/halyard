//! Patching: every patch leaves the store holding the new value, notifies what changed, and
//! never panics.
//!
//! Tests named `..._before_panicked` panicked on the previous code; the others are noted.

use halyard_reactive_graph::{
    effect::ImmediateEffect,
    owner::Owner,
    traits::{GetUntracked, Track},
};
use halyard_reactive_stores::{
    AtKeyed, Patch, PatchField, PatchFieldKeyed, Store, StorePath,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

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

fn todos(ids: &[usize]) -> Vec<Todo> {
    ids.iter()
        .map(|&id| Todo {
            id,
            label: format!("#{id}"),
        })
        .collect()
}

fn relabeled(ids: &[usize], id: usize, label: &str) -> Vec<Todo> {
    let mut todos = todos(ids);
    for todo in &mut todos {
        if todo.id == id {
            todo.label = label.to_string();
        }
    }
    todos
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

fn path(segments: &[usize]) -> StorePath {
    segments.iter().map(|&s| s.into()).collect()
}

// ----- whole-store patch of a struct with a keyed field (store_field.rs:153) -----

/// Before: the derived patch looked the field's keys up under the struct's path, which left a
/// key map with no indices at the root, and the notification then panicked finding a key for
/// index 0 there. (The derived patch still looks under the struct's path, so the field is
/// notified as a whole, logged once: see the macro's keyed branch.)
#[test]
fn patching_a_store_with_a_keyed_field_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });
    let two = AtKeyed::new(store.todos(), 2);
    let (_effect, two_runs) = runs(move || two.track());

    store.patch(Todos {
        todos: relabeled(&[2, 1, 3], 2, "two"),
    });

    assert_eq!(
        store.todos().get_untracked(),
        relabeled(&[2, 1, 3], 2, "two")
    );
    assert!(count(&two_runs) > 1, "the changed entry's subscriber ran");
}

// ----- keyed patch (patch.rs) -----

/// Before: the patch read the keys while holding the write lock (refused, logged as
/// re-entry), found no path for any entry, and then dropped the new value of every entry it
/// kept: the patch did nothing to them.
#[test]
fn keyed_patch_on_a_store_whose_keys_were_never_read_is_applied() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });

    store.todos().patch(relabeled(&[1, 2], 2, "two"));

    assert_eq!(store.todos().get_untracked(), relabeled(&[1, 2], 2, "two"));
}

#[test]
fn keyed_patch_notifies_only_the_changed_entry() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });
    let one = AtKeyed::new(store.todos(), 1);
    let two = AtKeyed::new(store.todos(), 2);
    let (_one, one_runs) = runs(move || one.track());
    let (_two, two_runs) = runs(move || two.track());

    store.todos().patch(relabeled(&[1, 2], 2, "two"));

    assert_eq!(count(&one_runs), 1);
    assert_eq!(count(&two_runs), 2);
}

/// Before: a key repeated in the new value made the patch index past the end of the list it
/// was rebuilding (or patch the wrong entry), and dropped the repeated entries.
#[test]
fn keyed_patch_with_repeated_keys_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });
    let (_effect, list_runs) = runs(move || store.todos().track());

    let mut new = todos(&[1, 2, 1, 3, 1]);
    if let Some(todo) = new.get_mut(2) {
        todo.label = "the second 1".into();
    }
    store.todos().patch(new.clone());

    assert_eq!(store.todos().get_untracked(), new);
    assert_eq!(count(&list_runs), 2, "the list is notified as changed");
}

/// Before: after a read (which set the field's keys up without recording their indices), a
/// patch at a key panicked finding the key for its index.
#[test]
fn patching_an_entry_after_it_was_read_before_panicked() {
    let owner = Owner::new();
    owner.set();
    let store = Store::new(Todos {
        todos: todos(&[1, 2]),
    });
    let one = AtKeyed::new(store.todos(), 1);
    let two = AtKeyed::new(store.todos(), 2);
    assert!(two.try_get_untracked().is_some());
    let (_one, one_runs) = runs(move || one.track());
    let (_two, two_runs) = runs(move || two.track());

    two.patch(Todo {
        id: 2,
        label: "two".into(),
    });

    assert_eq!(store.todos().get_untracked(), relabeled(&[1, 2], 2, "two"));
    assert_eq!(count(&one_runs), 1, "only the patched entry is notified");
    assert_eq!(count(&two_runs), 2);
}

/// Before: an entry kept by a keyed patch but with no path to notify it by kept its old value
/// (the new one was dropped), in all three collections.
#[test]
fn keyed_patch_without_a_path_keeps_the_new_value() {
    let new = relabeled(&[1, 2], 2, "two");

    let mut list = todos(&[1, 2]);
    let changed = list.patch_field_keyed(
        new.clone(),
        &mut |_| {},
        None,
        |todo| todo.id,
        |_| None,
    );
    assert!(changed);
    assert_eq!(list, new);

    let by_id = |todos: Vec<Todo>| {
        todos
            .into_iter()
            .map(|t| (t.id, t))
            .collect::<BTreeMap<_, _>>()
    };
    let mut tree = by_id(todos(&[1, 2]));
    let changed = tree.patch_field_keyed(
        by_id(new.clone()),
        &mut |_| {},
        None,
        |(id, _)| *id,
        |_| None,
    );
    assert!(changed);
    assert_eq!(tree, by_id(new.clone()));

    let by_id = |todos: Vec<Todo>| {
        todos
            .into_iter()
            .map(|t| (t.id, t))
            .collect::<HashMap<_, _>>()
    };
    let mut hash = by_id(todos(&[1, 2]));
    let changed = hash.patch_field_keyed(
        by_id(new.clone()),
        &mut |_| {},
        None,
        |(id, _)| *id,
        |_| None,
    );
    assert!(changed);
    assert_eq!(hash, by_id(new));
}

// ----- unkeyed patches (patch.rs arithmetic; these guard the rewrite, and passed before) -----

fn patched<T: PatchField>(mut old: T, new: T) -> (T, Vec<StorePath>) {
    let mut notified = Vec::new();
    old.patch_field(new, &path(&[7]), &mut |p| notified.push(p.clone()), None);
    (old, notified)
}

#[test]
fn vec_patch_notifies_changed_entries_and_a_new_length() {
    assert_eq!(
        patched(vec![1, 2, 3], vec![1, 5, 3]),
        (vec![1, 5, 3], vec![path(&[7, 1])])
    );
    assert_eq!(
        patched(vec![1, 2, 3], vec![1, 2]),
        (vec![1, 2], vec![path(&[7])])
    );
    assert_eq!(
        patched(vec![1, 2], vec![0, 2, 3, 4]),
        (vec![0, 2, 3, 4], vec![path(&[7, 0]), path(&[7])])
    );
    assert_eq!(patched(vec![1, 2], vec![]), (vec![], vec![path(&[7])]));
    assert_eq!(patched(Vec::<i32>::new(), vec![]), (vec![], vec![]));
}

#[test]
fn tuple_patch_notifies_each_changed_element_by_its_index() {
    assert_eq!(
        patched((1, 2, 3), (1, 5, 6)),
        ((1, 5, 6), vec![path(&[7, 1]), path(&[7, 2])])
    );
    assert_eq!(patched((1,), (1,)), ((1,), vec![]));
}
