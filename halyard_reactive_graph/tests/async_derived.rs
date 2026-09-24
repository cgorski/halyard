use halyard_reactive_graph::executor::Executor;
use halyard_reactive_graph::{
    computed::{ArcAsyncDerived, AsyncDerived},
    owner::Owner,
    prelude::*,
    signal::{ArcRwSignal, RwSignal},
};
use std::future::pending;

#[tokio::test]
async fn arc_async_derived_calculates_eagerly() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let value = ArcAsyncDerived::new(|| async {
        Executor::tick().await;
        42
    });

    assert_eq!(value.clone().await, 42);
}

#[tokio::test]
async fn arc_async_derived_tracks_signal_change() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let signal = RwSignal::new(10);
    let value = ArcAsyncDerived::new(move || async move {
        Executor::tick().await;
        signal.try_get().unwrap()
    });

    assert_eq!(value.clone().await, 10);
    signal.set(30);
    Executor::tick().await;
    assert_eq!(value.clone().await, 30);
    signal.set(50);
    Executor::tick().await;
    assert_eq!(value.clone().await, 50);
}

#[tokio::test]
async fn async_derived_calculates_eagerly() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let value = AsyncDerived::new(|| async {
        Executor::tick().await;
        42
    });

    assert_eq!(value.await, 42);
}

#[tokio::test]
async fn async_derived_tracks_signal_change() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let signal = RwSignal::new(10);
    let value = AsyncDerived::new(move || async move {
        Executor::tick().await;
        signal.try_get().unwrap()
    });

    assert_eq!(value.await, 10);
    signal.set(30);
    Executor::tick().await;
    assert_eq!(value.await, 30);
    signal.set(50);
    Executor::tick().await;
    assert_eq!(value.await, 50);
}

#[tokio::test]
async fn read_signal_traits_on_arc() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let value = ArcAsyncDerived::new(pending::<()>);
    assert_eq!(value.read(), None);
    assert_eq!(value.try_with_untracked(|n| *n), Some(None));
    assert_eq!(value.with(|n| *n), None);
    assert_eq!(value.get(), None);
}

#[tokio::test]
async fn read_signal_traits_on_arena() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let value = AsyncDerived::new(pending::<()>);
    println!("{:?}", value.try_read().unwrap());
    assert_eq!(value.try_read().unwrap(), None);
    assert_eq!(value.try_with_untracked(|n| *n), Some(None));
    assert_eq!(value.try_with(|n| *n), Some(None));
    assert_eq!(value.try_get(), Some(None));
}

#[tokio::test]
async fn async_derived_with_initial() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let signal1 = RwSignal::new(0);
    let signal2 = RwSignal::new(0);
    let derived =
        ArcAsyncDerived::new_with_initial(Some(5), move || async move {
            // reactive values can be tracked anywhere in the `async` block
            let value1 = signal1.try_get().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let value2 = signal2.try_get().unwrap();

            value1 + value2
        });

    // the value can be accessed synchronously as `Option<T>`
    assert_eq!(derived.get(), Some(5));
    // we can also .await the value, i.e., convert it into a Future
    assert_eq!(derived.clone().await, 0);
    assert_eq!(derived.get(), Some(0));

    signal1.set(1);
    // while the new value is still pending, the signal holds the old value
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(derived.get(), Some(0));

    // setting multiple dependencies will hold until the latest change is ready
    signal2.set(1);
    assert_eq!(derived.await, 2);
}

/// A loader whose result is ready while a reader holds the value (a guard from
/// `by_ref().await`, held across an `.await`) waits for the reader without queueing as a
/// writer: a writer waiting for the lock would keep new readers out, so a strong read made
/// meanwhile would wait for the reader too (natively) or find the lock busy (in the browser).
/// Here the strong read returns the previous value at once, and the new value comes in when
/// the reader lets go.
#[tokio::test]
async fn a_strong_read_does_not_wait_for_a_loader_that_waits_for_a_reader() {
    use std::{sync::mpsc, time::Duration};

    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let source = ArcRwSignal::new(0);
    let derived = ArcAsyncDerived::new({
        let source = source.clone();
        move || {
            let n = source.get();
            async move { n }
        }
    });
    assert_eq!(derived.clone().await, 0);

    // a reader holds the value across `.await`s
    let reader = derived.by_ref().await;
    assert_eq!(*reader, 0);

    // the loader runs again, and its result is ready while the reader holds the value
    source.set(1);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // a strong read on another thread: it must not wait for the reader held here
    let (tx, rx) = mpsc::channel();
    std::thread::spawn({
        let derived = derived.clone();
        move || _ = tx.send(derived.get())
    });
    let read = rx.recv_timeout(Duration::from_secs(5));
    drop(reader);
    assert_eq!(
        read,
        Ok(Some(0)),
        "a strong read waited for a loader that was waiting for a reader"
    );

    // once the reader lets go, the loaded value comes in
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(derived.get(), Some(1));
    assert_eq!(derived.await, 1);
}
