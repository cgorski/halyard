#[cfg(feature = "ssr")]
mod imports {
    pub use futures::StreamExt;
    pub use halyard::prelude::*;
    pub use halyard_reactive_graph::executor::Executor;
}

#[cfg(feature = "ssr")]
#[tokio::test]
async fn chain_await_resource() {
    use imports::*;

    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let (rs, ws) = signal(0);
    let source = Resource::new(
        || (),
        move |_| async move {
            #[cfg(feature = "ssr")]
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            1
        },
    );
    let consuming = Resource::new(
        || (),
        move |_| async move {
            let result = source.await;
            ws.update(|s| *s += 1);
            result
        },
    );
    let app = view! {
        <Suspense>{
            move || {
                Suspend::new(async move {
                    consuming.await;
                    rs.try_get().unwrap()
                })
            }
        }</Suspense>
    };

    assert_eq!(app.to_html_stream_in_order().collect::<String>().await, "1");
}

#[cfg(feature = "ssr")]
#[tokio::test]
async fn chain_no_await_resource() {
    use imports::*;

    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();

    let (rs, ws) = signal(0);
    let source = Resource::new(|| (), move |_| async move { 1 });
    let consuming = Resource::new(
        || (),
        move |_| async move {
            let result = source.await;
            ws.update(|s| *s += 1);
            result
        },
    );
    let app = view! {
        <Suspense>{
            move || {
                Suspend::new(async move {
                    consuming.await;
                    rs.try_get().unwrap()
                })
            }
        }</Suspense>
    };

    assert_eq!(app.to_html_stream_in_order().collect::<String>().await, "1");
}
