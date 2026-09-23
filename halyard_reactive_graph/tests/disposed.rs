//! Arena (`Copy`) handles whose value is gone. Until decision B (docs/no-panics.md) removes
//! the panicking accessors from these handles, `get()` and friends still panic on them; but
//! nothing inside the graph may: the graph's own code uses the `try_*` forms, and a handle
//! derived from a disposed one is itself disposed.

use halyard_reactive_graph::{
    callback::UnsyncCallback, owner::Owner, prelude::*, signal::RwSignal,
    wrappers::read::Signal,
};

fn with_owner<T>(scenario: impl FnOnce() -> T) -> T {
    let owner = Owner::new();
    owner.set();
    scenario()
}

/// `UnsyncCallback::matches` read both callbacks with the panicking `with_value`.
#[test]
fn a_disposed_unsync_callback_matches_nothing() {
    with_owner(|| {
        let callback = UnsyncCallback::new(|n: u32| n);
        let other = callback;
        callback.dispose();

        assert!(!callback.matches(&other));
    });
}

/// Tracking a disposed `Signal` panicked; it tracks nothing.
#[test]
fn tracking_a_disposed_signal_does_nothing() {
    with_owner(|| {
        let signal = Signal::derive(|| 1);
        signal.dispose();

        signal.track();
        assert_eq!(signal.try_get_untracked(), None);
    });
}

/// Splitting a disposed `RwSignal` panicked; its halves are disposed too.
#[test]
fn the_halves_of_a_disposed_signal_are_disposed() {
    with_owner(|| {
        let signal = RwSignal::new(1);
        signal.dispose();

        let (read, write) = signal.split();

        assert!(read.is_disposed());
        assert!(write.is_disposed());
        assert_eq!(read.try_get_untracked(), None);
        assert_eq!(write.try_set(2), Some(2));
    });
}

/// `Signal<T>` into `Signal<Option<T>>` reads the source inside a derived closure; once the
/// source is gone, that read panicked. It reads `None`.
#[test]
fn an_optional_signal_made_from_a_disposed_signal_reads_none() {
    with_owner(|| {
        let source = Signal::derive(|| 1);
        let optional: Signal<Option<i32>> = source.into();
        assert_eq!(optional.try_get_untracked(), Some(Some(1)));

        source.dispose();

        assert_eq!(optional.try_get_untracked(), Some(None));
    });
}

/// With `sandboxed-arenas`, a reactive value created where no arena is active (a thread that
/// never set an owner) panicked; it is created disposed.
#[cfg(feature = "sandboxed-arenas")]
#[test]
fn a_signal_created_where_no_arena_is_active_is_disposed() {
    let outcome = std::thread::spawn(|| {
        let signal = RwSignal::new(1);
        let disposed = signal.is_disposed();
        let value = signal.try_get_untracked();
        signal.dispose();
        (disposed, value)
    })
    .join();

    assert!(matches!(outcome, Ok((true, None))), "{outcome:?}");
}
