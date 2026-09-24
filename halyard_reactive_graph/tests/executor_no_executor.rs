//! Spawning before any executor is set drops the task, instead of panicking (which it did
//! in debug builds without `tracing`, and for `spawn_local` in every release build). No test
//! in this file sets an executor: it is its own test binary, so its own process.

use halyard_reactive_graph::executor::Executor;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Sets its flag when dropped: moved into a task, it tells a dropped task from a kept one.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn spawn_without_an_executor_drops_the_task() {
    let dropped = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));

    let guard = SetOnDrop(Arc::clone(&dropped));
    let task_ran = Arc::clone(&ran);
    Executor::spawn(async move {
        let _guard = guard;
        task_ran.store(true, Ordering::SeqCst);
    });

    assert!(dropped.load(Ordering::SeqCst), "the task was kept");
    assert!(!ran.load(Ordering::SeqCst), "the task ran");
}

#[test]
fn spawn_local_without_an_executor_drops_the_task() {
    let dropped = Arc::new(AtomicBool::new(false));
    // `Rc` makes the task `!Send`, as `spawn_local` allows
    let ran = Rc::new(Cell::new(false));

    let guard = SetOnDrop(Arc::clone(&dropped));
    let task_ran = Rc::clone(&ran);
    Executor::spawn_local(async move {
        let _guard = guard;
        task_ran.set(true);
    });

    assert!(dropped.load(Ordering::SeqCst), "the task was kept");
    assert!(!ran.get(), "the task ran");
}

#[test]
fn every_later_task_is_dropped_too() {
    let dropped = Arc::new(AtomicBool::new(false));
    for _ in 0..3 {
        Executor::spawn(async {});
        Executor::spawn_local(async {});
    }
    let guard = SetOnDrop(Arc::clone(&dropped));
    Executor::spawn(async move {
        let _guard = guard;
    });
    assert!(dropped.load(Ordering::SeqCst));
}

/// `tick` waits for a task it spawns; that task is dropped, so it returns at once rather
/// than panic or wait forever.
#[test]
fn tick_without_an_executor_returns() {
    futures::executor::block_on(Executor::tick());
}

#[test]
fn poll_local_without_an_executor_does_nothing() {
    Executor::poll_local();
    Executor::poll_local();
}
