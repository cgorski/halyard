//! Re-entrant access to a signal never aborts, deadlocks or panics (docs/no-panics.md,
//! "Re-entrant access"):
//! - `update` runs its closure on a copy of the committed value, outside every lock: reads of
//!   the signal inside it (directly, or through memos and derived signals) see the committed
//!   value;
//! - a write made while this thread is using the signal (inside `with` or `update`, while a
//!   guard is alive) is deferred, and applied in order when the outermost use ends;
//! - write guards hold a copy of the value, committed when they are dropped;
//! - writes from several threads serialize (no update is lost), and subscribers are notified
//!   once per committed change.
//!
//! Each scenario runs on a thread of its own under a deadline: before this change most of
//! them panicked (a read inside `update` found the lock held and the panicking accessor gave
//! up), or silently dropped the write, or deadlocked.

use halyard_reactive_graph::{
    computed::{ArcAsyncDerived, Memo},
    effect::{batch, ImmediateEffect},
    executor::Executor,
    owner::Owner,
    prelude::*,
    signal::{arc_signal, ArcRwSignal, RwSignal},
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::Duration,
};
use tokio::runtime::{Builder, Runtime};

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(4)
            .build()
            .expect("a Tokio runtime for the tests")
    })
}

/// Runs `scenario` under a fresh reactive owner on a thread of its own, and returns what it
/// returns; fails the test if it has not finished after ten seconds (it deadlocked) or if it
/// panicked.
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
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(out) => out,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: did not finish within 10 s (deadlocked)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("{what}: panicked")
        }
    }
}

#[test]
fn a_read_inside_its_own_update_sees_the_committed_value() {
    let (seen, after) =
        finishes("a read inside the signal's own update", || {
            let count = RwSignal::new(1);
            let doubled = Memo::new(move |_| count.get() * 2);
            let derived = move || count.get() * 3;
            let mut seen = None;
            count.update(|n| {
                *n = 5;
                seen = Some((
                    count.get(),
                    count.get_untracked(),
                    count.with(|n| *n),
                    *count.read(),
                    doubled.get(),
                    derived(),
                ));
            });
            (seen, (count.get_untracked(), doubled.get_untracked()))
        });
    assert_eq!(seen, Some((1, 1, 1, 1, 2, 3)), "the committed value");
    assert_eq!(after, (5, 10), "the update is committed afterwards");
}

#[test]
fn a_read_inside_an_arc_signals_own_update_sees_the_committed_value() {
    let (seen, after) =
        finishes("a read inside an ArcRwSignal's update", || {
            let (read, write) = arc_signal(String::from("a"));
            let mut seen = None;
            write.update(|s| {
                s.push('b');
                seen = Some(read.get_untracked());
            });
            (seen, read.get_untracked())
        });
    assert_eq!(seen.as_deref(), Some("a"));
    assert_eq!(after, "ab");
}

#[test]
fn a_set_inside_its_own_with_is_applied_afterwards_in_order() {
    let (inside, after, guarded) =
        finishes("a set inside the signal's own with", || {
            let count = ArcRwSignal::new(0);
            let inside = count.with(|n| {
                count.set(n + 1);
                count.set(n + 10);
                count.update(|m| *m *= 2);
                count.get_untracked()
            });
            let after = count.get_untracked();

            // the same while a read guard is alive
            let guard = count.read();
            count.set(3);
            let still = *guard;
            drop(guard);
            (inside, after, (still, count.get_untracked()))
        });
    assert_eq!(inside, 0, "inside, the committed value");
    assert_eq!(after, 20, "applied in order: set 1, set 10, doubled");
    assert_eq!(guarded, (20, 3), "the set waits for the read guard");
}

#[test]
fn nested_updates_of_the_same_signal() {
    let (nested, inside_with) = finishes("nested updates", || {
        let count = RwSignal::new(1);
        // both start from the committed 1; the inner is applied after the outer
        count.update(|n| {
            count.update(|m| *m += 10);
            *n += 1;
        });
        let nested = count.get_untracked();

        // deferred updates inside a read compose, in order, with the writes before them
        let total = RwSignal::new(0);
        total.with(|_| {
            total.set(5);
            total.update(|m| *m += 1);
            total.update(|m| *m *= 10);
        });
        (nested, total.get_untracked())
    });
    assert_eq!(
        nested, 11,
        "the inner update replaces the outer one's result"
    );
    assert_eq!(inside_with, 60);
}

#[test]
fn an_update_inside_a_memo_recomputed_from_inside_an_update() {
    let (seen, count, memo) =
        finishes("an update inside a memo inside an update", || {
            let count = RwSignal::new(1);
            let other = RwSignal::new(0);
            let memo = Memo::new(move |_| {
                let n = count.get();
                if n == 1 {
                    // the memo writes the signal that is being updated...
                    count.update(|c| *c += 100);
                    // ...and another one
                    other.update(|o| *o += 1);
                }
                n * 10
            });
            let mut seen = None;
            count.update(|c| {
                seen = Some(memo.get());
                *c += 1;
            });
            (
                seen,
                (count.get_untracked(), other.get_untracked()),
                memo.get_untracked(),
            )
        });
    assert_eq!(seen, Some(10), "the memo computed from the committed value");
    assert_eq!(
        count,
        (101, 1),
        "the memo's update was deferred until the outer one had committed"
    );
    assert_eq!(memo, 1010, "the memo recomputed after the changes");
}

#[test]
fn a_write_guard_with_a_read_inside() {
    let (during, deferred, after, nested) =
        finishes("a write guard with a read inside", || {
            let count = RwSignal::new(1);
            let mut guard = count.write();
            *guard += 1;
            let during = (count.get_untracked(), *count.read_untracked());
            count.set(10);
            let deferred = count.get_untracked();
            drop(guard);
            let after = count.get_untracked();

            // a write guard taken inside the signal's own `with`
            count.with(|_| *count.write() += 5);
            (during, deferred, after, count.get_untracked())
        });
    assert_eq!(during, (1, 1), "the committed value while the guard lives");
    assert_eq!(deferred, 1, "the set waits for the guard");
    assert_eq!(after, 10, "the guard committed 2, then the set applied");
    assert_eq!(nested, 15);
}

/// Four threads each increment the signal 500 times, reading it inside the update: no update
/// is lost, and every closure sees the value it is applied over.
#[test]
fn two_threads_updating_concurrently_serialize() {
    let (total, torn) = finishes("threads updating one signal", || {
        let count = ArcRwSignal::new(0_u32);
        let torn = Arc::new(AtomicUsize::new(0));
        let threads = (0..4)
            .map(|_| {
                let count = count.clone();
                let torn = Arc::clone(&torn);
                thread::spawn(move || {
                    for _ in 0..500 {
                        count.update(|n| {
                            if count.get_untracked() != *n {
                                torn.fetch_add(1, Ordering::Relaxed);
                            }
                            *n += 1;
                        });
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("an updating thread");
        }
        (count.get_untracked(), torn.load(Ordering::Relaxed))
    });
    assert_eq!(total, 2000, "no update is lost");
    assert_eq!(torn, 0, "each update started from the committed value");
}

/// The same with tasks on a multi-threaded Tokio runtime, yielding between updates.
#[test]
fn tokio_tasks_updating_concurrently_serialize() {
    let (total, finished) = finishes("Tokio tasks updating one signal", || {
        let count = ArcRwSignal::new(0_u32);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        for _ in 0..8 {
            let count = count.clone();
            let done_tx = done_tx.clone();
            runtime().spawn(async move {
                for _ in 0..250 {
                    count.update(|n| *n = count.get_untracked() + 1);
                    tokio::task::yield_now().await;
                }
                _ = done_tx.send(());
            });
        }
        let finished = (0..8)
            .filter(|_| done_rx.recv_timeout(Duration::from_secs(5)).is_ok())
            .count();
        (count.get_untracked(), finished)
    });
    assert_eq!(finished, 8);
    assert_eq!(total, 2000);
}

#[test]
fn subscribers_are_notified_once_per_committed_change() {
    let (runs, value) = finishes("notifications per committed change", || {
        let count = RwSignal::new(0);
        let runs = Arc::new(AtomicUsize::new(0));
        let effect = ImmediateEffect::new_isomorphic({
            let runs = Arc::clone(&runs);
            move || {
                count.track();
                runs.fetch_add(1, Ordering::Relaxed);
            }
        });
        let mut after = vec![runs.load(Ordering::Relaxed)];
        let mut step = |f: &dyn Fn()| {
            f();
            after.push(runs.load(Ordering::Relaxed));
        };
        step(&|| count.set(1));
        step(&|| count.update(|n| *n += 1));
        // committed, but not notified
        step(&|| {
            count.maybe_update(|n| {
                *n += 1;
                false
            })
        });
        // deferred: one commit, one notification, after the read
        step(&|| count.with(|_| count.set(10)));
        // the update's commit, then the deferred set
        step(&|| {
            count.update(|n| {
                count.set(20);
                *n += 1;
            })
        });
        step(&|| *count.write() += 1);
        drop(effect);
        (after, count.get_untracked())
    });
    assert_eq!(runs, vec![1, 2, 3, 3, 4, 6, 7]);
    assert_eq!(value, 21);
}

/// A value that cannot be cloned has no `update`; `set` is deferred like any write, and
/// `try_update` changes it in place, or returns `None` while this thread is using it.
#[test]
fn a_value_that_cannot_be_cloned_is_set_or_updated_in_place() {
    #[derive(Debug, PartialEq)]
    struct Token(u32);

    let (inside, in_place, after) =
        finishes("a value that is not Clone", || {
            let token = RwSignal::new(Token(1));
            let inside = token.with(|_| {
                token.set(Token(2));
                token.try_update(|t| t.0 += 1)
            });
            let in_place = token.try_update(|t| {
                t.0 += 1;
                t.0
            });
            (inside, in_place, token.with_untracked(|t| t.0))
        });
    assert_eq!(inside, None, "not while this thread is reading it");
    assert_eq!(in_place, Some(3), "the deferred set applied first");
    assert_eq!(after, 3);
}

/// An `AsyncDerived` written from inside its own read waited for itself natively (a
/// deadlock). The write is now refused, and logged.
#[test]
fn an_async_derived_written_inside_its_own_read_finishes() {
    let (inside, after) =
        finishes("an AsyncDerived written inside its read", || {
            let derived = ArcAsyncDerived::new(|| async { 1 });
            let inside = derived.with_untracked(|value| {
                derived.set(Some(2));
                *value
            });
            (inside, derived.get_untracked())
        });
    assert_eq!(inside, Some(1));
    assert_eq!(after, Some(1), "the write inside the read is refused");
}

/// `batch` deferred the ImmediateEffects of every thread, and ran them on the batching thread.
/// A batch now belongs to its thread.
#[test]
fn a_batch_on_one_thread_does_not_defer_another_threads_effects() {
    let ran_at_once = finishes("a batch on another thread", || {
        let count = RwSignal::new(0);
        let runs = Arc::new(AtomicUsize::new(0));
        let effect = ImmediateEffect::new_isomorphic({
            let runs = Arc::clone(&runs);
            move || {
                count.track();
                runs.fetch_add(1, Ordering::Relaxed);
            }
        });
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<usize>();
        let other = thread::spawn({
            let runs = Arc::clone(&runs);
            move || {
                _ = go_rx.recv_timeout(Duration::from_secs(5));
                count.set(1);
                _ = done_tx.send(runs.load(Ordering::Relaxed));
            }
        });
        let ran_at_once = batch(|| {
            _ = go_tx.send(());
            done_rx.recv_timeout(Duration::from_secs(5)).ok()
        });
        _ = other.join();
        drop(effect);
        ran_at_once
    });
    assert_eq!(
        ran_at_once,
        Some(2),
        "the other thread's effect ran at once"
    );
}
