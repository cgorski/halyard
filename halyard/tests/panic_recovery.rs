//! Situations in which these components used to panic, and now recover (README, "Project
//! policy": no panics, ever). Rendering to HTML needs the `ssr` feature:
//! `cargo test -p halyard --features ssr --test panic_recovery`.

#![cfg(feature = "ssr")]

use halyard::{context::Provider, prelude::*};

/// The HTML without `<!>` hydration markers, whose layout depends on the render mode.
fn without_markers(html: &str) -> String {
    html.replace("<!>", "")
}

/// Views are usually created under the owner of `mount_to`/`hydrate_body` or of a server
/// integration, but nothing enforces that: `<For/>` used to panic with "no reactive owner".
#[test]
fn for_outside_any_owner_renders_its_rows() {
    assert!(Owner::current().is_none());
    let html = view! {
        <ul>
            <For each=|| [1, 2, 3] key=|n| *n let(n)>
                <li>{n}</li>
            </For>
        </ul>
    }
    .to_html();
    assert!(
        without_markers(&html)
            .contains("<ul><li>1</li><li>2</li><li>3</li></ul>"),
        "unexpected HTML: {html}"
    );
}

#[test]
fn for_enumerate_outside_any_owner_renders_its_rows() {
    assert!(Owner::current().is_none());
    let html = view! {
        <ul>
            <ForEnumerate each=|| ["a", "b"] key=|s| *s let(index, s)>
                <li>{move || index.try_get().unwrap()}":"{s}</li>
            </ForEnumerate>
        </ul>
    }
    .to_html();
    assert!(
        without_markers(&html).contains("<ul><li>0:a</li><li>1:b</li></ul>"),
        "unexpected HTML: {html}"
    );
}

/// `<Provider/>` used to panic with "no current reactive Owner found".
#[test]
fn provider_outside_any_owner_provides_its_value() {
    assert!(Owner::current().is_none());
    let html = view! {
        <p>
            <Provider value=42u8>{use_context::<u8>().unwrap_or(0)}</Provider>
        </p>
    }
    .to_html();
    assert!(
        without_markers(&html).contains("<p>42</p>"),
        "unexpected HTML: {html}"
    );
}

/// Reading a `LocalResource` under `<Suspense/>` on the server renders the fallback and
/// marks the chunk for the client. Without a shared context (a view rendered outside a
/// server integration) marking it used to panic with "no shared context".
#[tokio::test]
async fn suspense_reading_a_local_resource_without_shared_context_renders_the_fallback(
) {
    use futures::StreamExt;
    use halyard_reactive_graph::executor::Executor;

    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    assert!(Owner::current_shared_context().is_none());

    let local = LocalResource::new(|| async { 1 });
    let app = view! {
        <Suspense fallback=|| "loading">
            {move || local.try_get().unwrap().map(|n| n.to_string())}
        </Suspense>
    };
    let html = app.to_html_stream_in_order().collect::<String>().await;
    assert_eq!(without_markers(&html), "loading");
}

#[tokio::test]
async fn transition_reading_a_local_resource_without_shared_context_renders_the_fallback(
) {
    use futures::StreamExt;
    use halyard_reactive_graph::executor::Executor;

    _ = Executor::init_tokio();
    let owner = Owner::new();
    owner.set();
    assert!(Owner::current_shared_context().is_none());

    let local = LocalResource::new(|| async { 1 });
    let app = view! {
        <Transition fallback=|| "loading">
            {move || local.try_get().unwrap().map(|n| n.to_string())}
        </Transition>
    };
    let html = app.to_html_stream_in_order().collect::<String>().await;
    assert_eq!(without_markers(&html), "loading");
}
