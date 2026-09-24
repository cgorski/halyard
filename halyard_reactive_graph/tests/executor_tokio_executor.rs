#![cfg(not(target_family = "wasm"))]

use futures::channel::oneshot;
use halyard_reactive_graph::executor::Executor;

#[tokio::test]
async fn test_tokio_executor() {
    // Initialize the tokio executor
    Executor::init_tokio().expect("Failed to initialize tokio executor");

    let (tx, rx) = oneshot::channel();

    // Spawn a task that sends a value
    Executor::spawn(async move {
        tx.send(42).expect("Failed to send value");
    });

    // Wait for the spawned task to complete
    assert_eq!(rx.await.unwrap(), 42);
}
