#![cfg(not(target_family = "wasm"))]
//! With Tokio as the executor, `spawn` outside a Tokio runtime drops the task instead of
//! panicking (`tokio::spawn` panics there); inside a runtime it spawns as before.

use futures::channel::oneshot;
use halyard_reactive_graph::executor::Executor;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Sets its flag when dropped: moved into a task, it tells a dropped task from a kept one.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn spawn_outside_a_tokio_runtime_drops_the_task() {
    let _ = Executor::init_tokio();

    // a plain test thread: there is no Tokio runtime here
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
fn spawn_inside_a_tokio_runtime_runs_the_task() {
    let _ = Executor::init_tokio();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
    let value = runtime.block_on(async {
        let (tx, rx) = oneshot::channel();
        Executor::spawn(async move {
            _ = tx.send(42);
        });
        rx.await
    });
    assert_eq!(value, Ok(42));
}
