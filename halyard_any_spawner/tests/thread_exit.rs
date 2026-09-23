#![cfg(any(feature = "futures-executor", feature = "async-executor"))]
//! A task spawned locally while a thread exits, from a thread-local value's destructor that
//! runs after the executor's own thread-local pool is gone, is dropped. It used to panic
//! (reading a destroyed thread-local), and a panic in a thread-local destructor aborts the
//! whole process.
//!
//! Uses the `futures` executor when that feature is on, otherwise `async-executor`: both
//! keep a local pool per thread.

use halyard_any_spawner::Executor;
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

/// Spawns a local task when dropped, and counts that it did.
struct SpawnOnDrop(Arc<AtomicUsize>);

impl Drop for SpawnOnDrop {
    fn drop(&mut self) {
        Executor::spawn_local(async {});
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

thread_local! {
    static BEFORE_THE_POOL: RefCell<Option<SpawnOnDrop>> = const { RefCell::new(None) };
    static AFTER_THE_POOL: RefCell<Option<SpawnOnDrop>> = const { RefCell::new(None) };
}

fn init() {
    #[cfg(feature = "futures-executor")]
    let set = Executor::init_futures_executor();
    #[cfg(not(feature = "futures-executor"))]
    let set = Executor::init_async_executor();
    assert!(set.is_ok(), "{set:?}");
}

#[test]
fn spawning_locally_while_the_thread_exits_drops_the_task() {
    init();

    let destructors_run = Arc::new(AtomicUsize::new(0));
    let thread = std::thread::spawn({
        let destructors_run = Arc::clone(&destructors_run);
        move || {
            // one value made before the executor's local pool and one after it: whichever
            // order the thread destroys them in, one of the two spawns after the pool is gone
            BEFORE_THE_POOL.with(|value| {
                *value.borrow_mut() =
                    Some(SpawnOnDrop(Arc::clone(&destructors_run)))
            });
            Executor::spawn_local(async {});
            Executor::poll_local();
            AFTER_THE_POOL.with(|value| {
                *value.borrow_mut() = Some(SpawnOnDrop(destructors_run))
            });
        }
    });

    assert!(thread.join().is_ok(), "the thread panicked while exiting");
    assert_eq!(destructors_run.load(Ordering::SeqCst), 2);
}
