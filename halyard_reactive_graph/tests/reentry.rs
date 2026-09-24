//! No user code under a framework guard (docs/no-panics.md, "Structural changes" 2).
//!
//! Each test runs its scenario on a thread of its own and waits for it with a deadline: a
//! scenario that re-enters a lock the thread already holds used to deadlock natively (and to
//! abort in the browser, where std's single-threaded lock aborts on a conflicting
//! acquisition), and must now finish.
//!
//! A deadlock that holds the arena's lock wedges every other test in this binary, so on the
//! old code run these one at a time (`cargo test --test reentry <name>`).

use halyard_reactive_graph::executor::Executor;
use halyard_reactive_graph::{
    actions::Action,
    callback::{Callable, Callback},
    computed::Memo,
    effect::RenderEffect,
    owner::{Owner, StoredValue},
    prelude::*,
    signal::{ArcRwSignal, RwSignal},
    wrappers::write::SignalSetter,
};
use std::{
    sync::{mpsc, Arc, OnceLock},
    thread,
    time::Duration,
};
use tokio::runtime::{Builder, Runtime};

/// The Tokio runtime whose worker threads run the tasks the scenarios spawn (a render
/// effect spawns one that waits for changes).
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .build()
            .expect("a Tokio runtime for the tests")
    })
}

/// Runs `scenario` under a fresh reactive owner on a thread of its own, and returns what it
/// returns; fails the test if it has not finished after five seconds (it deadlocked) or if
/// it panicked.
fn finishes<T: Send + 'static>(
    what: &str,
    scenario: impl FnOnce() -> T + Send + 'static,
) -> T {
    _ = Executor::init_tokio();
    let runtime = runtime().handle().clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _runtime = runtime.enter();
        let owner = Owner::new();
        owner.set();
        let out = scenario();
        _ = tx.send(out);
        drop(owner);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(out) => out,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: did not finish within 5 s (deadlocked)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("{what}: panicked")
        }
    }
}

/// `Callback::run` borrowed the stored function while it ran. A callback that reaches itself
/// again (here through a slot, as a recursive handler does) now runs a clone of the function
/// with nothing borrowed.
#[test]
fn a_callback_that_calls_itself_finishes() {
    let depth = finishes("a callback that calls itself", || {
        let slot: StoredValue<Option<Callback<u32, u32>>> =
            StoredValue::new(None);
        let countdown = Callback::new(move |n: u32| {
            if n == 0 {
                0
            } else {
                slot.try_get_value()
                    .flatten()
                    .and_then(|me| me.try_run(n - 1))
                    .map_or(0, |below| below + 1)
            }
        });
        slot.set_value(Some(countdown));
        countdown.run(5)
    });
    assert_eq!(depth, 5);
}

/// A stored closure that replaces the value it is stored in. Run through `with_value` (whose
/// contract is to borrow the value while the closure runs), the replacement is refused and
/// logged, and the call finishes; run from a clone taken with `get_value`, it takes effect.
#[test]
fn a_stored_closure_that_replaces_itself_finishes() {
    type Step = Arc<dyn Fn() -> u32 + Send + Sync>;

    let (borrowed, cloned, after) =
        finishes("a stored closure that replaces itself", || {
            let slot: StoredValue<Step> = StoredValue::new(Arc::new(|| 0));
            let replaces_itself: Step = Arc::new(move || {
                slot.set_value(Arc::new(|| 2));
                1
            });

            slot.set_value(Arc::clone(&replaces_itself));
            let borrowed = slot.try_with_value(|step| step());
            let still_first = slot.try_with_value(|step| step());

            slot.set_value(replaces_itself);
            let cloned = slot.try_get_value().map(|step| step());
            let after = slot.try_get_value().map(|step| step());
            (borrowed.zip(still_first), cloned, after)
        });
    assert_eq!(borrowed, Some((1, 1)), "the borrowed run cannot replace it");
    assert_eq!(cloned, Some(1));
    assert_eq!(after, Some(2), "a cloned run replaces it");
}

/// A stored closure that updates the value it is stored in from inside `update_value`: the
/// nested update is refused (its `try_*` returns `None`), and the outer one applies.
#[test]
fn a_stored_value_updated_from_inside_its_own_update_finishes() {
    let (nested, value) =
        finishes("a stored value updated inside its own update", || {
            let counter = StoredValue::new(0_u32);
            let mut nested = None;
            counter.update_value(|n| {
                nested = Some(counter.try_update_value(|n| *n += 10));
                *n += 1;
            });
            (nested, counter.try_get_value())
        });
    assert_eq!(nested, Some(None), "the nested update is refused");
    assert_eq!(value, Some(1));
}

/// An effect whose closure writes a signal it is reading (`with` + `set`): the write blocked
/// on the read lock this same thread held. The write is now deferred until the `with` ends,
/// then applied, and the effect runs again for each change, to a fixed point.
#[test]
fn an_effect_that_writes_a_signal_it_reads_finishes() {
    let (first, last) =
        finishes("an effect that writes a signal it reads", || {
            let count = RwSignal::new(0);
            let effect = RenderEffect::new_isomorphic(move |_| {
                count.with(|n| {
                    if *n < 3 {
                        count.set(*n + 1);
                    }
                    *n
                })
            });
            let first = count.try_get_untracked();
            // the effect runs again on the runtime's threads
            let mut last = first;
            for _ in 0..200 {
                last = count.try_get_untracked();
                if last == Some(3) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            drop(effect);
            (first, last)
        });
    assert!(
        first.is_some_and(|n| n >= 1),
        "the write inside `with` is applied when it ends: {first:?}"
    );
    assert_eq!(last, Some(3));
}

/// The same with a reference-counted signal, whose write took the lock directly, and without
/// an effect: two writes inside `with` are applied when it ends, in order.
#[test]
fn writes_to_an_arc_signal_inside_its_with_are_applied_in_order() {
    let (inside, after) =
        finishes("writes to an ArcRwSignal inside its with", || {
            let count = ArcRwSignal::new(0);
            let inside = count.with(|n| {
                count.set(*n + 1);
                count.set(*n + 2);
                count.try_get_untracked()
            });
            (inside, count.try_get_untracked())
        });
    assert_eq!(inside, Some(0), "inside, the committed value");
    assert_eq!(after, Some(2));
}

/// Reading a signal inside its own `update` used to panic (its `try_get` called the
/// panicking `read_untracked`); the update now runs on a copy, so the read gives the
/// committed value, and the update applies.
#[test]
fn reading_a_signal_inside_its_own_update_gives_the_committed_value() {
    let (seen, count) = finishes("a signal read inside its own update", || {
        let count = RwSignal::new(0);
        let mut seen = Some(Some(-1));
        count.update(|n| {
            seen = Some(count.try_get_untracked());
            *n += 1;
        });
        (seen, count.try_get_untracked())
    });
    assert_eq!(seen, Some(Some(0)));
    assert_eq!(count, Some(1));
}

/// A mapped `SignalSetter` ran the user's setter while the arena was locked: a setter that
/// creates a reactive value (as a navigation does) deadlocked.
#[test]
fn a_signal_setter_that_creates_a_signal_finishes() {
    let created = finishes("a signal setter that creates a signal", || {
        let created = StoredValue::new(None);
        let setter: SignalSetter<i32> = SignalSetter::map(move |n: i32| {
            let signal = RwSignal::new(n);
            created.set_value(signal.try_get_untracked());
        });
        setter.set(7);
        created.try_get_value().flatten()
    });
    assert_eq!(created, Some(7));
}

/// Values owned by an owner were dropped while the arena was locked: a value whose `Drop`
/// uses the reactive graph deadlocked the owner's cleanup.
#[test]
fn cleaning_up_a_value_whose_drop_uses_the_graph_finishes() {
    struct WritesOnDrop(RwSignal<u32>);

    impl Drop for WritesOnDrop {
        fn drop(&mut self) {
            self.0.set(1);
        }
    }

    let flag =
        finishes("cleaning up a value whose Drop writes a signal", || {
            let flag = RwSignal::new(0);
            let child = Owner::current().map(|owner| owner.child());
            if let Some(child) = &child {
                child.with(|| StoredValue::new(WritesOnDrop(flag)));
                child.cleanup();
            }
            flag.try_get_untracked()
        });
    assert_eq!(flag, Some(1));
}

/// A callback that owns an owner (the last reference to it): cleaning the callback up drops
/// the closure, which drops the owner, whose own cleanup removes its values from the arena,
/// which was still locked.
#[test]
fn cleaning_up_a_callback_that_owns_an_owner_finishes() {
    let finished =
        finishes("cleaning up a callback that owns an owner", || {
            let child = Owner::current().map(|owner| owner.child());
            // a sibling of `child`, so that cleaning up `child` does not clean it up first
            let owned = Owner::new();
            let signal = owned.with(|| RwSignal::new(0));
            if let Some(child) = &child {
                child.with(move || {
                    Callback::new(move |_: ()| {
                        owned.with(|| signal.try_get_untracked())
                    });
                });
                child.cleanup();
            }
            signal.is_disposed()
        });
    assert!(
        finished,
        "the owned owner was dropped, and its signal with it"
    );
}

/// `Action::clear` ran inside the arena's lock, and its write notified subscribers there: an
/// effect watching the action's value that creates a reactive value deadlocked.
#[test]
fn clearing_an_action_watched_by_an_effect_finishes() {
    let runs = finishes("clearing an action watched by an effect", || {
        let action = Action::new_with_value(Some(1), |n: &i32| {
            let n = *n;
            async move { n }
        });
        let runs = StoredValue::new(0_u32);
        let effect =
            halyard_reactive_graph::effect::ImmediateEffect::new_isomorphic(
                move || {
                    action.value().track();
                    let _created = RwSignal::new(());
                    runs.update_value(|n| *n += 1);
                },
            );
        action.clear();
        drop(effect);
        runs.try_get_value()
    });
    assert_eq!(runs, Some(2), "the effect ran on creation and on clear");
}

/// A memo's recomputation took its value lock while this thread held a read guard on it: a
/// memo read, after its source changed, under a guard on its own value deadlocked. It now
/// returns the previous value, and recomputes once the guard is gone.
#[test]
fn reading_a_memo_under_its_own_guard_after_a_change_finishes() {
    let (during, after) = finishes("a memo read under its own guard", || {
        let source = RwSignal::new(1);
        let memo = Memo::new(move |_| source.try_get().unwrap_or(0) * 10);
        let guard = memo.try_read_untracked();
        source.set(2);
        let during = memo.try_get_untracked();
        drop(guard);
        (during, memo.try_get_untracked())
    });
    assert_eq!(during, Some(10), "the previous value, while it is borrowed");
    assert_eq!(after, Some(20));
}

/// `RenderEffect::with_value_mut` holds the value's lock while its closure runs: a nested
/// call on the same effect deadlocked. It now returns `None`.
#[test]
fn render_effect_with_value_mut_inside_itself_finishes() {
    let (outer, inner) =
        finishes("RenderEffect::with_value_mut inside itself", || {
            let effect = Arc::new(RenderEffect::new_isomorphic(|_| 1_u32));
            let mut inner = Some(Some(0));
            let outer = effect.with_value_mut(|value| {
                inner = Some(effect.with_value_mut(|value| *value));
                *value += 1;
                *value
            });
            (outer, inner)
        });
    assert_eq!(outer, Some(2));
    assert_eq!(inner, Some(None));
}

/// An `ImmediateEffect` built with `new_mut` that retriggers itself panicked ("The effect
/// recursed"); the nested run is now skipped and logged.
#[cfg(feature = "effects")]
#[test]
fn an_immediate_effect_with_new_mut_that_retriggers_itself_finishes() {
    let count =
        finishes("ImmediateEffect::new_mut retriggering itself", || {
            let count = RwSignal::new(0);
            let effect =
                halyard_reactive_graph::effect::ImmediateEffect::new_mut(
                    move || {
                        let n = count.try_get().unwrap_or(0);
                        if n < 3 {
                            count.set(n + 1);
                        }
                    },
                );
            let count = count.try_get_untracked();
            drop(effect);
            count
        });
    assert_eq!(count, Some(1), "the retriggered run is skipped");
}

/// The graph serves a multi-threaded server: its handles stay `Send + Sync`.
#[test]
fn the_graph_types_are_still_send_and_sync() {
    fn send_sync<T: Send + Sync>() {}

    send_sync::<RwSignal<u32>>();
    send_sync::<ArcRwSignal<u32>>();
    send_sync::<StoredValue<u32>>();
    send_sync::<Callback<u32, u32>>();
    send_sync::<Memo<u32>>();
    send_sync::<Action<u32, u32>>();
    send_sync::<SignalSetter<u32>>();
    send_sync::<halyard_reactive_graph::wrappers::read::Signal<u32>>();
    send_sync::<
        halyard_reactive_graph::effect::Effect<
            halyard_reactive_graph::owner::SyncStorage,
        >,
    >();
    send_sync::<RenderEffect<u32>>();
    send_sync::<Owner>();
}

/// Not re-entry, and fine before and after: an effect that reads a signal (the guard is
/// released) and then writes it runs again for each write.
#[test]
fn an_effect_that_reads_then_writes_its_signal_runs_to_a_fixed_point() {
    let count = finishes("an effect that reads and then writes", || {
        let count = RwSignal::new(0);
        let effect =
            halyard_reactive_graph::effect::ImmediateEffect::new_isomorphic(
                move || {
                    let n = count.try_get().unwrap_or(0);
                    if n < 3 {
                        count.set(n + 1);
                    }
                },
            );
        let count = count.try_get_untracked();
        drop(effect);
        count
    });
    // ImmediateEffect runs only with the `effects` feature; `new_isomorphic` always runs
    assert_eq!(count, Some(3));
}
