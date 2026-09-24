//! Resources whose source may have no value (`Resource::new_try`, `ArcResource::new_try`,
//! `LocalResource::new_try`): `None` means "no fetch". The fetcher is not called and the
//! resource stays as it is: pending if it never loaded, else with the value it loaded last.

use halyard::prelude::*;
use halyard_reactive_graph::executor::Executor;
use std::{
    future::IntoFuture,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Lets the resource's loader run.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// A fetcher that counts its calls and loads `id * 10`.
fn counting_fetcher(
    fetches: &Arc<AtomicUsize>,
) -> impl Fn(u32) -> std::future::Ready<u32> + Send + Sync + 'static {
    let fetches = Arc::clone(fetches);
    move |id| {
        fetches.fetch_add(1, Ordering::SeqCst);
        std::future::ready(id * 10)
    }
}

fn is_pending<F: IntoFuture>(resource: F) -> bool {
    use futures::FutureExt;
    resource.into_future().now_or_never().is_none()
}

#[tokio::test]
async fn a_source_without_a_value_fetches_nothing() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    let fetches = Arc::new(AtomicUsize::new(0));
    let id = RwSignal::new(None::<u32>);
    let resource =
        Resource::new_try(move || id.try_get()?, counting_fetcher(&fetches));

    // never loaded: pending, and nothing fetched
    settle().await;
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
    assert_eq!(resource.try_get(), Some(None));
    assert!(is_pending(resource));

    // a value: it loads
    id.set(Some(1));
    settle().await;
    assert_eq!(resource.try_get(), Some(Some(10)));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    // no value again: it keeps what it loaded, and fetches nothing
    id.set(None);
    settle().await;
    assert_eq!(resource.try_get(), Some(Some(10)));
    assert!(!is_pending(resource));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    // the value it loaded with: nothing new to load
    id.set(Some(1));
    settle().await;
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    // a refetch while there is no value fetches nothing then...
    id.set(None);
    resource.refetch();
    settle().await;
    assert_eq!(resource.try_get(), Some(Some(10)));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    // ...and is not lost: the next value loads, even the one it loaded with
    id.set(Some(1));
    settle().await;
    assert_eq!(fetches.load(Ordering::SeqCst), 2);

    // a new value: it loads; a refetch with a value loads again
    id.set(Some(2));
    settle().await;
    assert_eq!(resource.try_get(), Some(Some(20)));
    resource.refetch();
    settle().await;
    assert_eq!(fetches.load(Ordering::SeqCst), 4);
}

/// A source that reads a weak handle with `?`: once the handle's value is gone, a refetch
/// fetches nothing and the resource keeps its value.
#[tokio::test]
async fn a_source_whose_handle_is_gone_fetches_nothing() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    let child = owner.child();
    let id = child.with(|| RwSignal::new(3_u32));
    let fetches = Arc::new(AtomicUsize::new(0));
    let resource =
        ArcResource::new_try(move || id.try_get(), counting_fetcher(&fetches));
    assert_eq!(resource.clone().await, 30);

    child.cleanup();
    resource.refetch();
    settle().await;
    assert_eq!(resource.get(), Some(30));
    assert!(!is_pending(resource.clone()));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

/// `new` is `new_try` with a source that always has a value: unchanged behaviour.
#[tokio::test]
async fn new_still_fetches_on_every_change_and_refetch() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    let id = RwSignal::new(1_u32);
    let fetches = Arc::new(AtomicUsize::new(0));
    let resource = Resource::new(
        move || id.try_get().unwrap_or_default(),
        counting_fetcher(&fetches),
    );
    assert_eq!(resource.await, 10);
    id.set(2);
    settle().await;
    resource.refetch();
    settle().await;
    assert_eq!(resource.try_get(), Some(Some(20)));
    assert_eq!(fetches.load(Ordering::SeqCst), 3);
}

/// In the browser (without `ssr`), a local resource made with `new_try` behaves the same.
#[cfg(not(feature = "ssr"))]
#[tokio::test]
async fn a_local_resource_without_a_source_value_fetches_nothing() {
    _ = Executor::init_tokio();
    tokio::task::LocalSet::new()
        .run_until(async {
            let owner = Owner::new();
            owner.set();
            let fetches = Arc::new(AtomicUsize::new(0));
            let id = RwSignal::new(None::<u32>);
            let resource = LocalResource::new_try(
                move || id.try_get()?,
                counting_fetcher(&fetches),
            );

            settle().await;
            assert_eq!(fetches.load(Ordering::SeqCst), 0);
            assert_eq!(resource.try_get(), Some(None));

            id.set(Some(5));
            settle().await;
            assert_eq!(resource.try_get(), Some(Some(50)));

            id.set(None);
            resource.refetch();
            settle().await;
            assert_eq!(resource.try_get(), Some(Some(50)));
            assert_eq!(fetches.load(Ordering::SeqCst), 1);
        })
        .await;
}

/// On the server, a `<Suspense>` over a resource whose source has no value shows its
/// fallback, and nothing is fetched.
#[cfg(feature = "ssr")]
#[tokio::test]
async fn suspense_shows_its_fallback_while_the_source_has_no_value() {
    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    let fetches = Arc::new(AtomicUsize::new(0));
    let id = RwSignal::new(None::<u32>);
    let resource =
        Resource::new_try(move || id.try_get()?, counting_fetcher(&fetches));

    let html = view! {
        <Suspense fallback=|| "loading">
            {move || Suspend::new(async move { resource.await.to_string() })}
        </Suspense>
    }
    .to_html();

    assert!(html.contains("loading"), "{html}");
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
}
