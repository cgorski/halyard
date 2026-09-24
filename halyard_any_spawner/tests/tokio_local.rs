#![cfg(feature = "tokio")]
//! Local (`!Send`) tasks on the Tokio executor run on the `LocalSet` they were spawned in,
//! and can spawn further local tasks.

use futures::channel::oneshot;
use halyard_any_spawner::Executor;
use std::rc::Rc;
use tokio::task::LocalSet;

#[tokio::test]
async fn a_local_task_runs_and_can_spawn_another() {
    Executor::init_tokio().expect("Failed to initialize tokio executor");

    LocalSet::new()
        .run_until(async {
            let (tx, rx) = oneshot::channel();
            let not_send = Rc::new(42);
            Executor::spawn_local(async move {
                Executor::spawn_local(async move {
                    _ = tx.send(*not_send);
                });
            });
            assert_eq!(rx.await, Ok(42));
        })
        .await;
}
