//! A lock is poisoned when a thread panics while holding it. The project's rule (commit
//! `c426d9a3`, "or_poisoned") is that a poisoned lock hands back its guard: the value is
//! still there, and reading or unwrapping it must not panic.

use halyard_reactive_graph::{
    owner::ArcStoredValue,
    prelude::*,
    signal::{ArcRwSignal, ArcWriteSignal},
};
use std::thread;

/// A signal whose value lock was held by a thread that panicked.
fn poisoned_signal() -> ArcRwSignal<u32> {
    let signal = ArcRwSignal::new(7);
    let in_thread = signal.clone();
    _ = thread::spawn(move || {
        let _guard = in_thread.try_write_in_place();
        panic!("poisons the signal's lock (expected in this test)");
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

#[test]
fn a_poisoned_stored_value_still_gives_up_its_value() {
    let stored = ArcStoredValue::new(7_u32);
    let in_thread = stored.clone();
    _ = thread::spawn(move || {
        let _guard = in_thread.try_write_value();
        panic!("poisons the stored value's lock (expected in this test)");
    })
    .join();

    assert_eq!(stored.try_get_value(), Some(7));
    assert_eq!(stored.into_inner(), Some(7));
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
