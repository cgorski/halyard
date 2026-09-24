//! A lock is poisoned when a thread panics while holding it. The project's rule (commit
//! `c426d9a3`, "or_poisoned") is that a poisoned lock hands back its guard: the value is
//! still there, and reading or unwrapping it must not panic.

use halyard_reactive_graph::{
    owner::ArcStoredValue,
    prelude::*,
    signal::{ArcMappedSignal, ArcRwSignal, ArcWriteSignal},
};
use std::thread;

/// A signal whose value lock was held by a thread that panicked. No value is lent out for a
/// change in place, so no closure runs under a signal's lock; a mapped signal's projection (a
/// plain `fn`, run while its part is swapped in) is the only code that does.
fn poisoned_signal() -> ArcRwSignal<u32> {
    let signal = ArcRwSignal::new(7);
    let in_thread = signal.clone();
    _ = thread::spawn(move || {
        let mapped = ArcMappedSignal::new(
            in_thread,
            |n| n,
            |_| panic!("poisons the signal's lock (expected in this test)"),
        );
        mapped.set(1);
    })
    .join();
    signal
}

/// `into_inner` used to panic (an `unwrap` of the poisoned lock).
#[test]
fn a_poisoned_signal_still_gives_up_its_value() {
    assert_eq!(poisoned_signal().into_inner(), Some(7));

    let signal = poisoned_signal();
    let read = signal.read_only();
    drop(signal);
    assert_eq!(read.into_inner(), Some(7));

    let signal = poisoned_signal();
    let write: ArcWriteSignal<u32> = signal.write_only();
    drop(signal);
    assert_eq!(write.into_inner(), Some(7));
}

/// A stored value's lock used to be poisoned by a panic inside `update_value` or while its
/// write guard was alive (both changed the value in place, under the lock). They work on a
/// copy now: a panic there leaves the value as it was, and its lock unpoisoned.
#[test]
fn a_panic_while_changing_a_stored_value_leaves_it_as_it_was() {
    let stored = ArcStoredValue::new(7_u32);
    let in_thread = stored.clone();
    _ = thread::spawn(move || {
        in_thread.update_value(|n| {
            *n = 8;
            panic!("inside the update (expected in this test)");
        });
    })
    .join();
    let in_thread = stored.clone();
    _ = thread::spawn(move || {
        let mut guard = in_thread.write_value();
        *guard = 9;
        panic!("while the guard is alive (expected in this test)");
    })
    .join();

    assert_eq!(stored.try_get_value(), Some(7));
    stored.set_value(10);
    assert_eq!(stored.into_inner(), Some(10));
}

/// Reading a poisoned signal gave `None` (and `get()` panicked, saying the signal was
/// disposed); writing it was refused. Both reach the value.
#[test]
fn a_poisoned_signal_can_still_be_read_and_written() {
    let signal = poisoned_signal();

    assert_eq!(signal.try_get_untracked(), Some(7));
    signal.set(8);
    assert_eq!(signal.try_get_untracked(), Some(8));
}
