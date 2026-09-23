//! Executors set per thread with `Executor::init_local_custom_executor`. A thread without
//! one used to panic when it spawned (an `unwrap` on its missing executor), and a second
//! thread that set its own got `Err(AlreadySet)` even though its executor was then used.
//!
//! Each test does its per-thread work on threads it starts itself, so it does not depend on
//! how the test harness reuses threads. The tests share this process's global executor,
//! which the first `init_local_custom_executor` makes per-thread.

use halyard_any_spawner::{
    CustomExecutor, Executor, ExecutorError, PinnedFuture, PinnedLocalFuture,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

/// Counts what it is given, and runs each task to completion at once.
#[derive(Clone, Default)]
struct Counting {
    spawned: Arc<AtomicUsize>,
    spawned_local: Arc<AtomicUsize>,
    polled: Arc<AtomicUsize>,
}

impl CustomExecutor for Counting {
    fn spawn(&self, fut: PinnedFuture<()>) {
        self.spawned.fetch_add(1, Ordering::SeqCst);
        futures::executor::block_on(fut);
    }

    fn spawn_local(&self, fut: PinnedLocalFuture<()>) {
        self.spawned_local.fetch_add(1, Ordering::SeqCst);
        futures::executor::block_on(fut);
    }

    fn poll_local(&self) {
        self.polled.fetch_add(1, Ordering::SeqCst);
    }
}

/// Sets its flag when dropped: moved into a task, it tells a dropped task from a kept one.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Runs `f` on a new thread and returns its result; a panic there fails the test.
fn on_new_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    match thread::spawn(f).join() {
        Ok(value) => value,
        Err(_) => panic!("the thread panicked"),
    }
}

#[test]
fn a_thread_without_its_own_executor_drops_its_tasks() {
    let mine = Counting::default();
    let set = {
        let mine = mine.clone();
        on_new_thread(move || Executor::init_local_custom_executor(mine))
    };
    assert!(set.is_ok(), "{set:?}");

    let spawn_dropped = Arc::new(AtomicBool::new(false));
    let spawn_local_dropped = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));
    {
        let (spawn_dropped, spawn_local_dropped, ran) = (
            Arc::clone(&spawn_dropped),
            Arc::clone(&spawn_local_dropped),
            Arc::clone(&ran),
        );
        on_new_thread(move || {
            let guard = SetOnDrop(spawn_dropped);
            let task_ran = Arc::clone(&ran);
            Executor::spawn(async move {
                let _guard = guard;
                task_ran.store(true, Ordering::SeqCst);
            });
            let guard = SetOnDrop(spawn_local_dropped);
            Executor::spawn_local(async move {
                let _guard = guard;
                ran.store(true, Ordering::SeqCst);
            });
            Executor::poll_local();
        });
    }

    assert!(
        spawn_dropped.load(Ordering::SeqCst),
        "the spawned task was kept"
    );
    assert!(
        spawn_local_dropped.load(Ordering::SeqCst),
        "the locally spawned task was kept"
    );
    assert!(!ran.load(Ordering::SeqCst), "a task ran");
    // nor did another thread's executor get them
    assert_eq!(mine.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(mine.spawned_local.load(Ordering::SeqCst), 0);
    assert_eq!(mine.polled.load(Ordering::SeqCst), 0);
}

#[test]
fn every_thread_can_set_its_own_executor() {
    let first = Counting::default();
    let second = Counting::default();

    for executor in [first.clone(), second.clone()] {
        let spawned = on_new_thread(move || {
            let set = Executor::init_local_custom_executor(executor);
            Executor::spawn(async {});
            Executor::spawn_local(async {});
            Executor::poll_local();
            set
        });
        assert!(spawned.is_ok(), "{spawned:?}");
    }

    for executor in [&first, &second] {
        assert_eq!(executor.spawned.load(Ordering::SeqCst), 1);
        assert_eq!(executor.spawned_local.load(Ordering::SeqCst), 1);
        assert_eq!(executor.polled.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn a_thread_cannot_set_two_executors_and_keeps_the_first() {
    let first = Counting::default();
    let second = Counting::default();
    let (set_first, set_second) = {
        let (first, second) = (first.clone(), second.clone());
        on_new_thread(move || {
            let set_first = Executor::init_local_custom_executor(first);
            let set_second = Executor::init_local_custom_executor(second);
            Executor::spawn(async {});
            (set_first, set_second)
        })
    };

    assert!(set_first.is_ok(), "{set_first:?}");
    assert!(
        matches!(set_second, Err(ExecutorError::AlreadySet)),
        "{set_second:?}"
    );
    assert_eq!(first.spawned.load(Ordering::SeqCst), 1);
    assert_eq!(second.spawned.load(Ordering::SeqCst), 0);
}

#[test]
fn a_shared_executor_cannot_replace_the_per_thread_ones() {
    let mine = Counting::default();
    let shared = Counting::default();
    let (set_mine, set_shared) = {
        let (mine, shared) = (mine.clone(), shared.clone());
        on_new_thread(move || {
            let set_mine = Executor::init_local_custom_executor(mine);
            let set_shared = Executor::init_custom_executor(shared);
            Executor::spawn(async {});
            (set_mine, set_shared)
        })
    };

    assert!(set_mine.is_ok(), "{set_mine:?}");
    assert!(
        matches!(set_shared, Err(ExecutorError::AlreadySet)),
        "{set_shared:?}"
    );
    assert_eq!(mine.spawned.load(Ordering::SeqCst), 1);
    assert_eq!(shared.spawned.load(Ordering::SeqCst), 0);
}
